//! The long-lived process: one root, one runner, a socket. It knows where a
//! job runs and whether it would be refused, never how it is done — that is
//! `assembly run`'s.

pub mod api;
pub mod client;
pub mod dispatch;
pub mod root;
pub mod submit;

use crate::runner::{JobSecrets, Runner, RunnerProblem};
use anyhow::Context;
use root::{RootLock, socket_path};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A daemon whose runner and credentials have been checked.
#[derive(Debug)]
pub struct Daemon<R> {
    pub root: PathBuf,
    pub runner: R,
    pub max_jobs: usize,
    secrets: JobSecrets,
}

impl<R: Runner> Daemon<R> {
    /// Check the runner, and gather what it will send each round, before
    /// anything listens.
    ///
    /// # Errors
    ///
    /// Every reason the runner cannot run, and every credential it would
    /// send that `env` lacks.
    pub async fn prepare(
        root: PathBuf,
        runner: R,
        max_jobs: usize,
        pass_env: &[String],
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Daemon<R>, Vec<RunnerProblem>> {
        let (secrets, unsendable) = match R::RUNS_IN_A_CONTAINER {
            true => JobSecrets::from_lookup(pass_env, env),
            false => (JobSecrets::default(), Vec::new()),
        };
        let problems: Vec<RunnerProblem> = runner
            .reasons_it_cannot_run()
            .await
            .into_iter()
            .chain(unsendable)
            .collect();
        match problems.is_empty() {
            true => Ok(Daemon {
                root,
                runner,
                max_jobs,
                secrets,
            }),
            false => Err(problems),
        }
    }

    #[must_use]
    pub fn secrets(&self) -> &JobSecrets {
        &self.secrets
    }
}

/// What every route and every round shares while the daemon runs.
#[derive(Debug)]
pub struct Serving<R> {
    pub daemon: Daemon<R>,
    pub repos: dispatch::RepoLocks,
    pub queue: dispatch::JobQueue,
}

/// Serve `daemon` on its root's socket, launching the jobs submitted to it,
/// until `shutdown` resolves; then stop its rounds and remove the socket.
///
/// # Errors
///
/// When the socket cannot be bound or the server fails.
pub async fn serve<R: Runner + Send + Sync + 'static>(
    daemon: Daemon<R>,
    lock: RootLock,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let socket = socket_path(&daemon.root);
    let listener = tokio::net::UnixListener::bind(&socket)
        .with_context(|| format!("listening on {}", socket.display()))?;
    let (queue, pending) = dispatch::JobQueue::new();
    let serving = Arc::new(Serving {
        daemon,
        repos: dispatch::RepoLocks::default(),
        queue,
    });
    let dispatcher = tokio::spawn(dispatch::dispatch_until_stopped(
        Arc::clone(&serving),
        pending,
    ));

    let served = serve_until_shutdown(listener, api::router(Arc::clone(&serving)), shutdown).await;
    // P7: until reattach, a round the daemon cannot come back to is
    // cancelled and given its verdict before the daemon goes.
    serving.queue.stop_rounds_and_wait().await;
    let _ = dispatcher.await;
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    served.map_err(Into::into)
}

/// How long requests already in flight get to finish once shutdown is asked
/// for.
const REQUEST_GRACE_PERIOD: Duration = Duration::from_secs(5);

/// Serve until `shutdown` resolves and the requests in flight have finished,
/// or [`REQUEST_GRACE_PERIOD`] has passed — a client that stalls mid-request
/// must not keep the daemon from stopping.
async fn serve_until_shutdown(
    listener: tokio::net::UnixListener,
    router: axum::Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let stopping = CancellationToken::new();
    let on_shutdown = stopping.clone();
    tokio::spawn(async move {
        shutdown.await;
        on_shutdown.cancel();
    });
    let graceful = axum::serve(listener, router)
        .with_graceful_shutdown(stopping.clone().cancelled_owned())
        .into_future();
    tokio::select! {
        served = graceful => served,
        () = async {
            stopping.cancelled().await;
            tokio::time::sleep(REQUEST_GRACE_PERIOD).await;
        } => Ok(()),
    }
}
