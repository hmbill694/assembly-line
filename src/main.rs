use assembly_line::cli::{Cli, Command, RunnerArgs, RunnerKind};
use assembly_line::daemon::api::Queued;
use assembly_line::daemon::client::{DaemonClient, Reply};
use assembly_line::daemon::{self, Daemon};
use assembly_line::frame::{FrameWriter, ReadableFrames};
use assembly_line::round::Verdict;
use assembly_line::run::{self, RunRefused, RunRequest};
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::kubernetes::KubernetesRunner;
use assembly_line::runner::local::LocalRunner;
use assembly_line::runner::{self, Runner};
use assembly_line::submission::{self, SubmitRequest};
use assembly_line::{locate, paths};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tokio_util::sync::CancellationToken;

/// Reserved for the user's mistake — a repository that has not opted in, a
/// missing job, a wrong directory. A round that runs and fails exits 1 instead.
const EXIT_USAGE: u8 = 2;
const EXIT_ROUND_FAILED: u8 = 1;

fn main() -> ExitCode {
    install_tracing();

    let cli = Cli::parse();
    let root = paths::state_root(cli.root, |name| std::env::var(name).ok());
    // `run` keeps no state, so a round inside a container with no `$HOME`
    // still runs.
    match (cli.command, root) {
        (
            Command::Run {
                prompt,
                prompt_file,
                repo,
                base_ref,
                job,
                provider,
                frames,
                provision_toolchain,
            },
            _,
        ) => in_async_runtime(run_whole_job(
            RunRequest {
                repo,
                base_ref,
                job,
                prompt,
                prompt_file,
                provider,
                provision_toolchain,
            },
            frames,
        )),
        (_, Err(e)) => fail_with_usage_error(e),
        (
            Command::Submit {
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
                job,
            },
            Ok(root),
        ) => in_async_runtime(submit_to_daemon(
            &root,
            SubmitRequest {
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
                job,
            },
        )),
        (Command::Daemon { runner, max_jobs }, Ok(root)) => {
            in_async_runtime(serve_daemon_on_chosen_runner(&runner, root, max_jobs))
        }
        (Command::Status { job_id, repo }, Ok(root)) => {
            in_async_runtime(print_job_status(&root, job_id, repo))
        }
        (
            Command::Logs {
                job_id,
                follow,
                repo,
            },
            Ok(root),
        ) => in_async_runtime(print_job_log(&root, job_id, follow, repo)),
    }
}

/// On stderr: stdout belongs to frames.
fn install_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "assembly_line=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// `run`: prepare, announce, run, conclude. With frames, stdout carries them
/// and everything a person reads goes to stderr; without, a person reads
/// stdout. The exit code mirrors the job, but a collector decides from the
/// frames — the code only matters when the frames never said.
async fn run_whole_job(request: RunRequest, as_frames: bool) -> Result<ExitCode, String> {
    let cancel = CancellationToken::new();
    cancel_on_termination_signal(cancel.clone());
    let ready = run::prepare_run(request, &std::env::temp_dir(), &cancel)
        .await
        .map_err(|refused| report_run_refusal(&refused))?;

    let conclusion = if as_frames {
        eprintln!("{}", ready.to_announcement_line());
        ready
            .run(&FrameWriter::new(std::io::stdout()), cancel)
            .await
    } else {
        println!("{}", ready.to_announcement_line());
        ready
            .run(
                &FrameWriter::new(ReadableFrames::new(std::io::stdout())),
                cancel,
            )
            .await
    }
    .map_err(|e| e.to_string())?;

    let lines = conclusion.to_lines();
    match as_frames {
        true => lines.iter().for_each(|line| eprintln!("{line}")),
        false => lines.iter().for_each(|line| println!("{line}")),
    }
    Ok(exit_code_for(conclusion.verdict))
}

fn report_run_refusal(refused: &RunRefused) -> String {
    refused
        .itemized_reasons()
        .iter()
        .for_each(|reason| eprintln!("error: {reason}"));
    refused.to_string()
}

/// SIGTERM is how the local runner cancels, SIGINT is Ctrl-C reaching the
/// whole foreground process group, and SIGHUP is the terminal closing — which
/// the agent, leading a session of its own, never hears. Any of them cancels
/// the round, which then still reports itself.
fn cancel_on_termination_signal(cancel: CancellationToken) {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let (Ok(mut terminate), Ok(mut hangup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = hangup.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        cancel.cancel();
    });
}

/// The runner `RunnerArgs` chose, built.
enum ChosenRunner {
    Local(LocalRunner),
    Docker(DockerRunner),
    K8s(KubernetesRunner),
}

/// The one place flags become a concrete runner; everything after it is
/// generic over [`Runner`].
fn chosen_runner(args: &RunnerArgs) -> Result<ChosenRunner, String> {
    if let Some(inapplicable) = args.inapplicable_flags() {
        return Err(inapplicable.to_string());
    }
    let image = || args.image.clone().unwrap_or_else(runner::published_image);
    match args.runner {
        RunnerKind::Local => LocalRunner::current_binary()
            .map(ChosenRunner::Local)
            .map_err(|e| e.to_string()),
        RunnerKind::Docker => Ok(ChosenRunner::Docker(DockerRunner::new(image()))),
        // clap has already required a namespace for k8s, so the default is
        // never taken.
        RunnerKind::K8s => Ok(ChosenRunner::K8s(KubernetesRunner::new(
            image(),
            args.namespace.clone().unwrap_or_default(),
            args.context.clone(),
        ))),
    }
}

async fn serve_daemon_on_chosen_runner(
    args: &RunnerArgs,
    root: PathBuf,
    max_jobs: usize,
) -> Result<ExitCode, String> {
    match chosen_runner(args)? {
        ChosenRunner::Local(runner) => serve_daemon(root, runner, max_jobs, &args.pass_env).await,
        ChosenRunner::Docker(runner) => serve_daemon(root, runner, max_jobs, &args.pass_env).await,
        ChosenRunner::K8s(runner) => serve_daemon(root, runner, max_jobs, &args.pass_env).await,
    }
}

/// Hold the root, check the runner, and serve until SIGTERM or Ctrl-C.
async fn serve_daemon<R: Runner + Send + Sync + 'static>(
    root: PathBuf,
    runner: R,
    max_jobs: usize,
    pass_env: &[String],
) -> Result<ExitCode, String> {
    let lock = daemon::root::hold_root(&root).map_err(|e| e.to_string())?;
    let daemon = Daemon::prepare(root, runner, max_jobs, pass_env, |name| {
        std::env::var(name).ok()
    })
    .await
    .map_err(|problems| {
        problems
            .iter()
            .for_each(|problem| eprintln!("error: {problem}"));
        "the daemon's runner cannot run".to_string()
    })?;
    let shutdown = termination_requested().map_err(|e| format!("handling SIGTERM: {e}"))?;
    eprintln!(
        "listening on {}",
        daemon::root::socket_path(&daemon.root).display()
    );
    daemon::serve(daemon, lock, shutdown)
        .await
        .map(|()| ExitCode::SUCCESS)
        .map_err(|e| e.to_string())
}

/// Resolves on SIGTERM or Ctrl-C. Both handlers are installed before this
/// returns: a signal landing before them would kill the daemon outright,
/// leaving its socket behind.
fn termination_requested() -> std::io::Result<impl Future<Output = ()> + Send + 'static> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

/// `submit`: resolve, post to the daemon, report what it said.
async fn submit_to_daemon(root: &Path, request: SubmitRequest) -> Result<ExitCode, String> {
    let prepared = submission::prepare_submission(request)
        .await
        .map_err(|e| e.to_string())?;
    prepared.notes.iter().for_each(|note| println!("{note}"));
    match DaemonClient::for_root(root)
        .post_json::<_, Queued>("/jobs", &prepared.submission)
        .await
        .map_err(|e| e.to_string())?
    {
        Reply::Accepted(queued) => {
            queued
                .warnings
                .iter()
                .for_each(|warning| eprintln!("warn: {warning}"));
            println!("{queued}");
            Ok(ExitCode::SUCCESS)
        }
        Reply::Refused(refused) => {
            refused
                .reasons
                .iter()
                .for_each(|reason| eprintln!("error: {reason}"));
            Err(refused.summary)
        }
    }
}

/// Where every async command's usage error is reported, so none of them has to
/// unwind one by hand.
fn in_async_runtime(work: impl Future<Output = Result<ExitCode, String>>) -> ExitCode {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => match runtime.block_on(work) {
            Ok(code) => code,
            Err(e) => fail_with_usage_error(e),
        },
        Err(e) => fail_with_usage_error(format!("starting the async runtime: {e}")),
    }
}

fn fail_with_usage_error(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(EXIT_USAGE)
}

fn exit_code_for(verdict: Verdict) -> ExitCode {
    match verdict {
        Verdict::Failed => ExitCode::from(EXIT_ROUND_FAILED),
        Verdict::Passed => ExitCode::SUCCESS,
    }
}

async fn print_job_status(
    root: &Path,
    job_id: Option<u64>,
    repo: Option<String>,
) -> Result<ExitCode, String> {
    let report = locate::report_for_job(root, job_id, repo)
        .await
        .map_err(|e| e.to_string())?;
    report
        .to_status_lines()
        .iter()
        .for_each(|line| println!("{line}"));
    Ok(ExitCode::SUCCESS)
}

async fn print_job_log(
    root: &Path,
    job_id: u64,
    follow: bool,
    repo: Option<String>,
) -> Result<ExitCode, String> {
    let path = locate::output_log_of(root, job_id, repo)
        .await
        .map_err(|e| e.to_string())?;

    match follow {
        // Delegated to `tail` rather than reimplemented; a machine without it
        // gets the spawn error.
        true => std::process::Command::new("tail")
            .arg("-f")
            .arg(&path)
            .status()
            .map(|_| ExitCode::SUCCESS)
            .map_err(|e| e.to_string()),
        false => std::fs::read_to_string(&path)
            .map(|body| {
                print!("{body}");
                ExitCode::SUCCESS
            })
            .map_err(|e| e.to_string()),
    }
}
