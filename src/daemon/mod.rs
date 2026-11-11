//! The long-lived process: one root, one runner, a socket. It knows where a
//! job runs and whether it would be refused, never how it is done — that is
//! `assembly run`'s.

pub mod api;
pub mod client;
pub mod dispatch;
pub mod fleet;
pub mod root;
pub mod submit;

use crate::event::EventKind;
use crate::paths::JobPlace;
use crate::report::JobReport;
use crate::runner::{Credentials, JobSecrets, Runner, RunnerProblem};
use anyhow::Context;
use fleet::Resumption;
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
        let (secrets, unsendable) = match R::ENVIRONMENT.credentials {
            Credentials::PassedIn => JobSecrets::from_lookup(pass_env, env),
            Credentials::OfTheHost => (JobSecrets::default(), Vec::new()),
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

/// Give every job under the root what it is owed, then serve `daemon` on
/// its root's socket, launching the jobs submitted to it, until `shutdown`
/// resolves; then launch nothing more, give up the launches in progress, and
/// remove the socket. Rounds already launched are left running: each
/// finishes without this daemon, and the next one on the root reattaches to
/// it.
///
/// # Errors
///
/// When a job's log under the root cannot be read or written, or the socket
/// cannot be bound, or the server fails.
pub async fn serve<R: Runner + Send + Sync + 'static>(
    daemon: Daemon<R>,
    lock: RootLock,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (queue, pending) = dispatch::JobQueue::new();
    let serving = Arc::new(Serving {
        daemon,
        repos: dispatch::RepoLocks::default(),
        queue,
    });
    resume_jobs_under_root(&serving)?;
    let socket = socket_path(&serving.daemon.root);
    let listener = tokio::net::UnixListener::bind(&socket)
        .with_context(|| format!("listening on {}", socket.display()))?;
    let dispatcher = tokio::spawn(dispatch::dispatch_until_stopped(
        Arc::clone(&serving),
        pending,
    ));

    let served = serve_until_shutdown(listener, api::router(Arc::clone(&serving)), shutdown).await;
    serving.queue.stop_launching();
    let _ = dispatcher.await;
    // Past the bound, a launch still cleaning up is dropped, and the next
    // daemon closes its round as lost mid-launch.
    let _ = tokio::time::timeout(LAUNCH_CLEANUP_GRACE, serving.queue.launches_settled()).await;
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    served.map_err(Into::into)
}

/// Reattach to every round launched without a verdict, close every round
/// lost mid-launch, and requeue every job still waiting, in the order they
/// were asked for.
fn resume_jobs_under_root<R: Runner + Send + Sync + 'static>(
    serving: &Arc<Serving<R>>,
) -> anyhow::Result<()> {
    let root = &serving.daemon.root;
    let owed: Vec<(JobPlace, JobReport, Resumption)> = fleet::jobs_under(root)?
        .into_iter()
        .filter_map(|(place, report)| {
            Resumption::for_report(&report).map(|owed| (place, report, owed))
        })
        .collect();
    let (mut requeued, others): (Vec<_>, Vec<_>) = owed
        .into_iter()
        .partition(|(_, _, owed)| *owed == Resumption::Requeue);

    others
        .into_iter()
        .try_for_each(|(place, _, owed)| match owed {
            Resumption::Reattach { round, handle } => {
                dispatch::spawn_reattached(serving, place, round, handle);
                Ok(())
            }
            Resumption::LostWhileLaunching { .. } => {
                let reason = fleet::lost_while_launching(place.id);
                place
                    .paths(root)
                    .append(EventKind::RoundFailed { reason })
                    .with_context(|| format!("closing job {} in {}", place.id, place.key))
            }
            Resumption::Requeue => Ok(()),
        })?;

    requeued.sort_by_key(|(_, report, _)| {
        report
            .requests
            .as_ref()
            .map(|requests| requests.latest.requested_at)
    });
    requeued
        .into_iter()
        .for_each(|(place, _, _)| serving.queue.enqueue(place));
    Ok(())
}

/// How long a launch given up at shutdown gets to remove what it had
/// created — a container, a Job and its Secret.
const LAUNCH_CLEANUP_GRACE: Duration = Duration::from_secs(10);

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
