//! assembly-line: run a DAG of coding-agent tasks in parallel.

pub mod cli;
pub mod config;
pub mod dag;
pub mod delivery;
pub mod event;
pub mod exec;
pub mod gc;
pub mod git;
pub mod paths;
pub mod provider;
pub mod report;
pub mod scheduler;
pub mod state;
pub mod workspace;
