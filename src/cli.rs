use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "assembly",
    version,
    about = "Run a set of independent coding-agent tasks"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check a graph for config mistakes
    Validate { graph: PathBuf },

    /// Execute a graph
    Run {
        graph: PathBuf,
        /// Maximum nodes running concurrently
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },

    /// Continue an interrupted run by replaying its event log
    Resume {
        run_id: u64,
        /// Override the job cap recorded for the original run
        #[arg(long)]
        jobs: Option<usize>,
    },

    /// Show the node tree and timings for a run
    Status {
        /// Defaults to the most recent run
        run_id: Option<u64>,
    },

    /// Run a node again, based on its own branch, with feedback
    ///
    /// A new job, not a resumption: the agent's prior work arrives as files on
    /// disk, and this round appends to the node's branch.
    Revise {
        run_id: u64,
        node: String,
        /// What to change about the previous round's work
        feedback: String,
    },

    /// Remove worktrees left behind by failed nodes
    Gc {
        /// Also remove worktrees untouched for this long, e.g. "7d"
        #[arg(long)]
        older_than: Option<String>,
        /// Report what would be removed, and remove nothing
        #[arg(long)]
        dry_run: bool,
    },

    /// Print a node's captured output
    Logs {
        run_id: u64,
        node: String,
        /// Follow the log as it grows
        #[arg(short, long)]
        follow: bool,
    },
}
