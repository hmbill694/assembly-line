//! assembly-line: run a DAG of shell and coding-agent tasks in parallel.

pub mod cli;
pub mod config;
pub mod dag;
pub mod event;
pub mod exec;
pub mod git;
pub mod paths;
pub mod report;
pub mod scheduler;
pub mod state;
