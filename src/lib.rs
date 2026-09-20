//! assembly-line: run one coding-agent job and keep the branch it leaves.

pub mod cli;
pub mod config;
pub mod delivery;
pub mod event;
pub mod exec;
pub mod gc;
pub mod git;
pub mod job;
pub mod paths;
pub mod provider;
pub mod report;
pub mod state;
pub mod workspace;
