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

    /// Remove worktrees left behind by jobs that died mid-run
    Gc {
        /// Also remove worktrees untouched for this long, e.g. "7d"
        #[arg(long)]
        older_than: Option<String>,
        /// Report what would be removed, and remove nothing
        #[arg(long)]
        dry_run: bool,
    },
}
