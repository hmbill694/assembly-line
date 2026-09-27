use assembly_line::cli::{Cli, Command, RunnerArgs, RunnerKind};
use assembly_line::frame::FrameWriter;
use assembly_line::job::{JobOutcome, run_round};
use assembly_line::lifecycle::{self, Note, Prepared, Refusal, RevisionRequest, StartRequest};
use assembly_line::payload::JobPayload;
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::kubernetes::KubernetesRunner;
use assembly_line::runner::local::LocalRunner;
use assembly_line::runner::{self, Runner};
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use tokio_util::sync::CancellationToken;

/// Reserved for the user's mistake — a repository that has not opted in, a
/// missing job, a wrong directory. A job that runs and fails exits 1 instead.
const EXIT_USAGE: u8 = 2;
const EXIT_JOB_FAILED: u8 = 1;

fn main() -> ExitCode {
    install_tracing();

    match Cli::parse().command {
        Command::Run {
            prompt,
            prompt_file,
            repo,
            base_ref,
            provider,
            runner,
        } => in_async_runtime(run_work_on_chosen_runner(
            runner,
            Work::Start(StartRequest {
                prompt,
                prompt_file,
                repo,
                base_ref,
                provider,
            }),
        )),
        Command::Revise {
            job_id,
            feedback,
            repo,
            runner,
        } => in_async_runtime(run_work_on_chosen_runner(
            runner,
            Work::Revise(RevisionRequest {
                job_id,
                feedback,
                repo,
            }),
        )),
        Command::Status { job_id, repo } => print_job_status(job_id, repo),
        Command::Logs {
            job_id,
            follow,
            repo,
        } => print_job_log(job_id, follow, repo),
        Command::JobExec => in_async_runtime(execute_payload_from_environment()),
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

/// `job-exec`: read the payload, run the round, report it on stdout.
///
/// The exit code mirrors the round, but the collector decides from the
/// frames — the code only matters when the frames never said.
async fn execute_payload_from_environment() -> Result<ExitCode, String> {
    let payload = JobPayload::from_environment().map_err(|e| e.to_string())?;

    let frames = FrameWriter::new(std::io::stdout());
    let cancel = CancellationToken::new();
    cancel_on_termination_signal(cancel.clone());

    run_round(&payload, &frames, &std::env::temp_dir(), cancel)
        .await
        .map(exit_code_for)
        .map_err(|e| e.to_string())
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

/// What `run` and `revise` do, independent of where the round runs.
enum Work {
    Start(StartRequest),
    Revise(RevisionRequest),
}

/// The one place flags become a concrete runner; everything after it is
/// generic over [`Runner`].
async fn run_work_on_chosen_runner(args: RunnerArgs, work: Work) -> Result<ExitCode, String> {
    if let Some(inapplicable) = args.inapplicable_flags() {
        return Err(inapplicable.to_string());
    }
    match args.runner {
        RunnerKind::Local => {
            run_work(
                &LocalRunner::current_binary().map_err(|e| e.to_string())?,
                &[],
                work,
            )
            .await
        }
        RunnerKind::Docker => {
            let image = args.image.unwrap_or_else(runner::published_image);
            run_work(&DockerRunner::new(image), &args.pass_env, work).await
        }
        RunnerKind::K8s => {
            let image = args.image.unwrap_or_else(runner::published_image);
            // clap has already required a namespace for k8s, so the default
            // is never taken.
            let namespace = args.namespace.unwrap_or_default();
            run_work(
                &KubernetesRunner::new(image, namespace, args.context),
                &args.pass_env,
                work,
            )
            .await
        }
    }
}

async fn run_work<R: Runner>(
    runner: &R,
    pass_env: &[String],
    work: Work,
) -> Result<ExitCode, String> {
    let Prepared { notes, round } = match work {
        Work::Start(request) => lifecycle::prepare_start(runner, pass_env, request).await,
        Work::Revise(request) => lifecycle::prepare_revision(runner, pass_env, request).await,
    };
    notes.iter().for_each(|note| match note {
        Note::LocalRefDiffers { .. } => println!("{note}"),
        Note::ConfigWarning(_) => eprintln!("{note}"),
    });

    let ready = round.map_err(|refusal| report_refusal(&refusal))?;
    if let Some(announcement) = ready.to_announcement_line() {
        println!("{announcement}");
    }

    let conclusion = lifecycle::run(ready, cancel_on_ctrl_c())
        .await
        .map_err(|e| e.to_string())?;
    conclusion
        .to_lines()
        .iter()
        .for_each(|line| println!("{line}"));
    Ok(exit_code_for(conclusion.outcome))
}

/// Every reason on its own line; the refusal itself becomes the usage error.
fn report_refusal(refusal: &Refusal) -> String {
    refusal
        .itemized_reasons()
        .iter()
        .for_each(|reason| eprintln!("error: {reason}"));
    refusal.to_string()
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

/// A token that Ctrl-C cancels, for the round about to run.
fn cancel_on_ctrl_c() -> CancellationToken {
    let cancel = CancellationToken::new();
    let on_interrupt = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\ninterrupted — cancelling the running agent");
            on_interrupt.cancel();
        }
    });
    cancel
}

fn exit_code_for(outcome: JobOutcome) -> ExitCode {
    match outcome {
        JobOutcome::Failed => ExitCode::from(EXIT_JOB_FAILED),
        JobOutcome::Passed => ExitCode::SUCCESS,
    }
}

fn print_job_status(job_id: Option<u64>, repo: Option<PathBuf>) -> ExitCode {
    match lifecycle::report_for_job(job_id, repo) {
        Ok(report) => {
            report
                .to_status_lines()
                .iter()
                .for_each(|line| println!("{line}"));
            ExitCode::SUCCESS
        }
        Err(e) => fail_with_usage_error(e),
    }
}

fn print_job_log(job_id: u64, follow: bool, repo: Option<PathBuf>) -> ExitCode {
    let path = match lifecycle::output_log_of(job_id, repo) {
        Ok(path) => path,
        Err(e) => return fail_with_usage_error(e),
    };

    match follow {
        // Delegated to `tail` rather than reimplemented; a machine without it
        // gets the spawn error.
        true => match std::process::Command::new("tail")
            .arg("-f")
            .arg(&path)
            .status()
        {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail_with_usage_error(e),
        },
        false => match std::fs::read_to_string(&path) {
            Ok(body) => {
                print!("{body}");
                ExitCode::SUCCESS
            }
            Err(e) => fail_with_usage_error(e),
        },
    }
}
