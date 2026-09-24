use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "assembly",
    version,
    about = "Run one coding-agent job and keep the branch it leaves"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run one job: an agent, a prompt, and the branch it leaves
    Run {
        /// What the agent is asked to do
        #[arg(
            long,
            conflicts_with = "prompt_file",
            required_unless_present = "prompt_file"
        )]
        prompt: Option<String>,
        /// Read the prompt from a file instead
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// The repository to work in. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// What to branch from. Defaults to the checked-out branch.
        #[arg(long = "ref")]
        base_ref: Option<String>,
        /// Overrides the repository's declared provider
        #[arg(long)]
        provider: Option<String>,
        #[command(flatten)]
        runner: RunnerArgs,
    },

    /// Run a job again, based on its own branch, with feedback
    ///
    /// A new job, not a resumption: the agent's prior work arrives as files on
    /// disk, and this round appends to the job's branch.
    Revise {
        job_id: u64,
        /// What to change about the previous round's work
        feedback: String,
        /// The repository the job belongs to. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
        #[command(flatten)]
        runner: RunnerArgs,
    },

    /// Show a job's state, timing and diff
    Status {
        /// Defaults to the most recent job
        job_id: Option<u64>,
        /// The repository to look in. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Print a job's captured output
    Logs {
        job_id: u64,
        /// Follow the log as it grows
        #[arg(short, long)]
        follow: bool,
        /// The repository the job belongs to. Defaults to the enclosing one.
        #[arg(long)]
        repo: Option<PathBuf>,
    },

    /// Run the round in `ASSEMBLY_JOB` and report it as frames on stdout.
    /// Started by a runner, never by hand.
    #[command(name = "job-exec", hide = true)]
    JobExec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RunnerKind {
    /// A child process on this machine, with its toolchain and credentials
    Local,
    /// A container, through the `docker` CLI
    Docker,
}

/// Where a round runs. Shared by `run` and `revise`: a revise is a new job
/// cut from the branch, so it may run somewhere the first round did not.
#[derive(Debug, clap::Args)]
pub struct RunnerArgs {
    #[arg(long, value_enum, default_value_t = RunnerKind::Local)]
    pub runner: RunnerKind,
    /// The job image. Defaults to the published image at this version.
    #[arg(long)]
    pub image: Option<String>,
    /// Pass this variable from your environment into the job's container.
    /// Repeatable. `ASSEMBLY_GIT_TOKEN` is always passed.
    #[arg(long = "pass-env", value_name = "NAME")]
    pub pass_env: Vec<String>,
}
