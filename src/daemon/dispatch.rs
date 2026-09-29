//! The queue and the cap: each queued job's round, launched on the daemon's
//! runner and collected into the job's log.

use super::Serving;
use crate::collect::{collect, record_launch_failure};
use crate::config::{REPO_CONFIG_PATH, RepoConfig};
use crate::event::{EventKind, EventLog};
use crate::job::JobId;
use crate::paths::{JobPaths, RepoKey};
use crate::report::JobReport;
use crate::runner::{LaunchSpec, Runner};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedMutexGuard, mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Where one job's state lives, which is all the daemon needs to find it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobAddress {
    pub key: RepoKey,
    pub jobs_dir: PathBuf,
    pub id: JobId,
}

impl JobAddress {
    #[must_use]
    pub fn paths(&self) -> JobPaths {
        JobPaths {
            id: self.id,
            dir: self.jobs_dir.join(self.id.to_string()),
        }
    }

    fn report(&self) -> anyhow::Result<JobReport> {
        let events = EventLog::read(self.paths().events())?;
        Ok(JobReport::from_events(self.id.into(), &events))
    }
}

/// One lock per repository cache (P5): two fetches into one bare repository
/// race on `FETCH_HEAD`.
#[derive(Debug, Default)]
pub struct RepoLocks {
    held: Mutex<HashMap<RepoKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl RepoLocks {
    /// # Panics
    ///
    /// If a holder panicked while registering a lock.
    pub async fn hold(&self, key: &RepoKey) -> OwnedMutexGuard<()> {
        let lock = self
            .held
            .lock()
            .expect("repository lock table")
            .entry(key.clone())
            .or_default()
            .clone();
        lock.lock_owned().await
    }
}

/// Jobs waiting for a slot, how many rounds are running now, and a way to
/// stop them.
#[derive(Debug)]
pub struct JobQueue {
    pending: mpsc::UnboundedSender<JobAddress>,
    running: watch::Sender<usize>,
    stopping: CancellationToken,
}

impl JobQueue {
    /// The queue, and the receiving end its dispatcher takes.
    #[must_use]
    pub fn new() -> (JobQueue, mpsc::UnboundedReceiver<JobAddress>) {
        let (pending, receiving) = mpsc::unbounded_channel();
        (
            JobQueue {
                pending,
                running: watch::Sender::new(0),
                stopping: CancellationToken::new(),
            },
            receiving,
        )
    }

    /// Put a job in line behind every job already waiting.
    pub fn enqueue(&self, address: JobAddress) {
        // Fails only once the dispatcher is gone, which is shutdown; the job
        // stays queued in its log.
        let _ = self.pending.send(address);
    }

    /// Cancel every running round and wait until each has its verdict. A
    /// job still waiting for a slot stays queued in its log.
    pub async fn stop_rounds_and_wait(&self) {
        self.stopping.cancel();
        // Cannot fail: `self` holds the sender.
        let _ = self.running.subscribe().wait_for(|n| *n == 0).await;
    }
}

/// Launch queued jobs, oldest first, whenever fewer than `max_jobs` rounds
/// are running, until the daemon stops.
///
/// A loop, not a stream combinator: each job waits for a slot before the
/// next one is looked at, which is what keeps the order.
pub async fn dispatch_until_stopped<R: Runner + Send + Sync + 'static>(
    serving: Arc<Serving<R>>,
    mut pending: mpsc::UnboundedReceiver<JobAddress>,
) {
    let queue = &serving.queue;
    let mut running = queue.running.subscribe();
    loop {
        let address = tokio::select! {
            biased;
            () = queue.stopping.cancelled() => return,
            next = pending.recv() => match next {
                Some(address) => address,
                None => return,
            },
        };
        let slot_free = tokio::select! {
            biased;
            () = queue.stopping.cancelled() => false,
            waited = running.wait_for(|n| *n < serving.daemon.max_jobs) => waited.is_ok(),
        };
        if !slot_free {
            return;
        }
        let slot = Slot::taken(&queue.running);
        let cancel = queue.stopping.child_token();
        let serving = Arc::clone(&serving);
        tokio::spawn(async move {
            let _slot = slot;
            if let Err(e) = run_queued_round(&serving, &address, cancel).await {
                tracing::error!("job {} in {}: {e:#}", address.id, address.key);
            }
        });
    }
}

/// One running round's place under the cap, given back however the round
/// ends — a panic included, or the daemon would wait on it forever.
struct Slot(watch::Sender<usize>);

impl Slot {
    fn taken(running: &watch::Sender<usize>) -> Slot {
        running.send_modify(|n| *n += 1);
        Slot(running.clone())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

/// The job's next round: numbered from its log, launched from its latest
/// request, collected into its log.
///
/// A round the daemon began stopping before it started is not started: the
/// job stays queued. That includes a round still waiting for its
/// repository's cache, which a submit's fetch can hold for as long as the
/// remote takes to answer.
async fn run_queued_round<R: Runner>(
    serving: &Serving<R>,
    address: &JobAddress,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let report = address.report()?;
    let (Some(remote_url), Some(base), Some(prompt), Some(provider)) = (
        report.remote_url,
        report.base,
        report.latest_prompt,
        report.provider,
    ) else {
        anyhow::bail!("the job has no recorded request to run");
    };
    let config = tokio::select! {
        biased;
        () = cancel.cancelled() => return Ok(()),
        config = config_in_cache(serving, &address.key, &base.sha) => config,
    };
    let paths = address.paths();
    let round = report.rounds + 1;
    let mut log = EventLog::open_append(paths.events())?;
    log.append(EventKind::RoundStarted { round })?;

    let config = match config {
        Ok(config) => config,
        Err(e) => {
            log.append(EventKind::RoundFailed {
                reason: format!("reading {REPO_CONFIG_PATH} at the job's base: {e:#}"),
            })?;
            return Ok(());
        }
    };
    let spec = LaunchSpec::for_round::<R>(
        address.id,
        round,
        &remote_url,
        &base,
        &prompt,
        &provider,
        config.command_limit_secs(),
    );
    match serving
        .daemon
        .runner
        .launch(&spec, serving.daemon.secrets(), &cancel)
        .await
    {
        Ok(running) => collect(running, &mut log, &paths.log(), cancel)
            .await
            .map(|_| ()),
        Err(e) => record_launch_failure(&mut log, &e).map(|_| ()),
    }
}

/// The repository's config at `sha`, from the daemon's cache of it, which
/// the submit that queued the job fetched `sha` into.
async fn config_in_cache<R>(
    serving: &Serving<R>,
    key: &RepoKey,
    sha: &str,
) -> anyhow::Result<RepoConfig> {
    let _repo = serving.repos.hold(key).await;
    RepoConfig::from_ref(&key.repo_cache(&serving.daemon.root), sha).await
}
