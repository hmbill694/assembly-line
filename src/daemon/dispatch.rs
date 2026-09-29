//! The queue and the cap: each queued job's round, launched on the daemon's
//! runner and collected into the job's log.

use super::Serving;
use crate::collect::{cancel_and_wait_out, collect, record_launch_failure};
use crate::config::{REPO_CONFIG_PATH, RepoConfig};
use crate::event::{EventKind, EventLog};
use crate::job::JobId;
use crate::paths::{self, JobPaths, RepoKey};
use crate::report::JobReport;
use crate::runner::{LaunchSpec, RoundHandle, Runner, RunningRound};
use crate::state::JobState;
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::{OwnedMutexGuard, mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Why a job cancelled before its round started has failed.
pub const CANCELLED_BEFORE_IT_STARTED: &str = "cancelled before it started";

/// Where one job's state lives, which is all the daemon needs to find it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    fn record_cancelled_before_it_started(&self) -> std::io::Result<()> {
        EventLog::open_append(self.paths().events())?
            .append(EventKind::RoundFailed {
                reason: CANCELLED_BEFORE_IT_STARTED.to_string(),
            })
            .map(|_| ())
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

/// Each launched round, until its task is over — which can be well after
/// its verdict, while `run` delivers and cleans up.
type CancelTable = Arc<Mutex<HashMap<JobAddress, LaunchedRound>>>;

#[derive(Debug)]
struct LaunchedRound {
    round: u32,
    cancel: CancellationToken,
}

impl LaunchedRound {
    /// Whether this is the round `report` is waiting on — not yet started,
    /// or running — rather than one past its verdict.
    fn is_awaited_by(&self, report: &JobReport) -> bool {
        match report.state {
            JobState::Queued => report.rounds + 1 == self.round,
            JobState::Running => report.rounds == self.round,
            JobState::Pending | JobState::Passed | JobState::Failed => false,
        }
    }
}

fn lock_cancel_table(
    cancels: &Mutex<HashMap<JobAddress, LaunchedRound>>,
) -> MutexGuard<'_, HashMap<JobAddress, LaunchedRound>> {
    // Nothing done under the lock leaves the table half-changed.
    cancels.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Jobs waiting for a slot, how many rounds are running now, and a way to
/// stop launching more.
#[derive(Debug)]
pub struct JobQueue {
    pending: mpsc::UnboundedSender<JobAddress>,
    running: watch::Sender<usize>,
    /// How many dispatched rounds have yet to be recorded as launched — or
    /// to end without launching.
    launching: watch::Sender<usize>,
    /// Fired when the daemon stops. It gives up the launches in progress;
    /// launched rounds never see it: each has a cancel of its own, fired
    /// only by [`JobQueue::cancel`].
    stopping: CancellationToken,
    /// Held while deciding whether a job is queued or launched and acting
    /// on it — by [`JobQueue::cancel`] and by the dispatcher — so a job is
    /// never both cancelled while queued and launched.
    cancels: CancelTable,
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
                launching: watch::Sender::new(0),
                stopping: CancellationToken::new(),
                cancels: CancelTable::default(),
            },
            receiving,
        )
    }

    /// Stop `address`'s job: cancel the round it is waiting on, or close it
    /// before it starts if it is still waiting for a slot. Returns which it
    /// was: [`JobState::Running`] or [`JobState::Queued`].
    ///
    /// # Errors
    ///
    /// Why it cannot be cancelled: there is no such job, it is neither
    /// queued nor running, or its log cannot be read or written.
    pub fn cancel(&self, address: &JobAddress) -> Result<JobState, String> {
        let cancels = lock_cancel_table(&self.cancels);
        let id = address.id;
        paths::open_job(&address.jobs_dir, id).map_err(|e| e.to_string())?;
        let report = address
            .report()
            .map_err(|e| format!("reading job {id}'s log: {e:#}"))?;
        match (cancels.get(address), report.state) {
            (Some(launched), _) if launched.is_awaited_by(&report) => {
                launched.cancel.cancel();
                Ok(JobState::Running)
            }
            (_, JobState::Queued) => address
                .record_cancelled_before_it_started()
                .map(|()| JobState::Queued)
                .map_err(|e| format!("recording job {id}'s cancel: {e}")),
            (_, JobState::Pending | JobState::Running | JobState::Passed | JobState::Failed) => {
                Err(format!(
                    "job {id} is not queued or running, so there is nothing to cancel"
                ))
            }
        }
    }

    /// Enter `address`'s next round in the cancel table, unless its job is
    /// no longer queued — cancelled while it waited — or that round was
    /// already launched from an earlier place in line: a job cancelled while
    /// queued and then revised is in line twice.
    fn register_if_still_queued(&self, address: &JobAddress) -> Option<Registration> {
        let mut cancels = lock_cancel_table(&self.cancels);
        let report = match address.report() {
            Ok(report) => report,
            Err(e) => {
                tracing::error!("job {} in {}: {e:#}", address.id, address.key);
                return None;
            }
        };
        let already_launched = cancels
            .get(address)
            .is_some_and(|launched| launched.is_awaited_by(&report));
        (report.state == JobState::Queued && !already_launched)
            .then(|| self.register(&mut cancels, address, report.rounds + 1))
    }

    /// Enter round `round` of `address`'s job, which an earlier daemon
    /// launched, in the cancel table.
    fn register_reattached(&self, address: &JobAddress, round: u32) -> Registration {
        self.register(&mut lock_cancel_table(&self.cancels), address, round)
    }

    fn register(
        &self,
        cancels: &mut HashMap<JobAddress, LaunchedRound>,
        address: &JobAddress,
        round: u32,
    ) -> Registration {
        let cancel = CancellationToken::new();
        cancels.insert(
            address.clone(),
            LaunchedRound {
                round,
                cancel: cancel.clone(),
            },
        );
        Registration {
            cancels: Arc::clone(&self.cancels),
            address: address.clone(),
            round,
            cancel,
        }
    }

    /// Put a job in line behind every job already waiting.
    pub fn enqueue(&self, address: JobAddress) {
        // Fails only once the dispatcher is gone, which is shutdown; the job
        // stays queued in its log.
        let _ = self.pending.send(address);
    }

    /// Launch nothing more: the dispatcher returns, a round still waiting to
    /// start leaves its job queued in its log, and a launch in progress is
    /// given up — its runner removing whatever it had created — and closed
    /// as failed. Rounds already launched run on.
    pub fn stop_launching(&self) {
        self.stopping.cancel();
    }

    /// Wait until no dispatched round is still launching. Call once the
    /// dispatcher has returned, so it can dispatch no more.
    pub async fn launches_settled(&self) {
        // Cannot fail: `self` holds the sender.
        let _ = self.launching.subscribe().wait_for(|n| *n == 0).await;
    }
}

/// A launched round's entry in the cancel table, removed however its task
/// ends — unless a later round of the job has taken its place, which a
/// revise accepted after this round's verdict can do while `run` is still
/// delivering it.
struct Registration {
    cancels: CancelTable,
    address: JobAddress,
    round: u32,
    cancel: CancellationToken,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut cancels = lock_cancel_table(&self.cancels);
        if cancels
            .get(&self.address)
            .is_some_and(|launched| launched.round == self.round)
        {
            cancels.remove(&self.address);
        }
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
        let Some(registration) = queue.register_if_still_queued(&address) else {
            continue;
        };
        let launching = Tally::counted_in(&queue.launching);
        spawn_round(
            &serving,
            address,
            registration,
            RoundToRun::Queued(launching),
        );
    }
}

/// Collect round `round` of `address`'s job, which an earlier daemon on this
/// root launched, as a round of this one's: in the cancel table and counted
/// under the cap. It runs whatever the cap says — a round already running
/// is not something the cap can prevent, only count.
pub fn spawn_reattached<R: Runner + Send + Sync + 'static>(
    serving: &Arc<Serving<R>>,
    address: JobAddress,
    round: u32,
    handle: RoundHandle,
) {
    let registration = serving.queue.register_reattached(&address, round);
    spawn_round(
        serving,
        address,
        registration,
        RoundToRun::Reattached(handle),
    );
}

enum RoundToRun {
    /// Counted in [`JobQueue::launching`] until it is launched.
    Queued(Tally),
    Reattached(RoundHandle),
}

/// Run a registered round in a task of its own, holding its place under the
/// cap and its entry in the cancel table until the task is over.
fn spawn_round<R: Runner + Send + Sync + 'static>(
    serving: &Arc<Serving<R>>,
    address: JobAddress,
    registration: Registration,
    to_run: RoundToRun,
) {
    let slot = Tally::counted_in(&serving.queue.running);
    let serving = Arc::clone(serving);
    tokio::spawn(async move {
        let _slot = slot;
        let (round, cancel) = (registration.round, registration.cancel.clone());
        let _registration = registration;
        let ran = match to_run {
            RoundToRun::Queued(launching) => {
                run_queued_round(&serving, &address, round, cancel, launching).await
            }
            RoundToRun::Reattached(handle) => {
                reattach_round(&serving, &address, round, &handle, cancel).await
            }
        };
        if let Err(e) = ran {
            tracing::error!("job {} in {}: {e:#}", address.id, address.key);
        }
    });
}

/// One entry in a count of rounds — running, or launching — taken back
/// however the round's task ends, a panic included, or the daemon would
/// wait on it forever.
struct Tally(watch::Sender<usize>);

impl Tally {
    fn counted_in(count: &watch::Sender<usize>) -> Tally {
        count.send_modify(|n| *n += 1);
        Tally(count.clone())
    }
}

impl Drop for Tally {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

/// The job's next round, `round`: launched from its latest request,
/// collected into its log.
///
/// A round cancelled before it started — still waiting for its repository's
/// cache, which a submit's fetch can hold for as long as the remote takes to
/// answer — is not started. The daemon stopping leaves its job queued; a
/// `cancel` closes it.
///
/// The daemon stopping mid-launch gives the launch up rather than leave a
/// round running that no later daemon could find: until `RoundLaunched` is
/// in the log there is no handle to find it by. `launching` is dropped once
/// the launch has settled one way or the other.
async fn run_queued_round<R: Runner>(
    serving: &Serving<R>,
    address: &JobAddress,
    round: u32,
    cancel: CancellationToken,
    launching: Tally,
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
        () = cancel.cancelled() => return Ok(address.record_cancelled_before_it_started()?),
        () = serving.queue.stopping.cancelled() => return Ok(()),
        config = config_in_cache(serving, &address.key, &base.sha) => config,
    };
    let paths = address.paths();
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
        &paths,
        round,
        &remote_url,
        &base,
        &prompt,
        &provider,
        config.command_limit_secs(),
    );
    let launch_cancel = cancel.child_token();
    let give_up_on_stopping = async {
        serving.queue.stopping.cancelled().await;
        launch_cancel.cancel();
        std::future::pending::<Infallible>().await
    };
    let launched = tokio::select! {
        launched = serving.daemon.runner.launch(&spec, serving.daemon.secrets(), &launch_cancel) => launched,
        never = give_up_on_stopping => match never {},
    };
    let running = match (launched, serving.queue.stopping.is_cancelled()) {
        (Ok(running), _) => running,
        (Err(_), true) => {
            return log
                .append(EventKind::RoundFailed {
                    reason: stopped_while_launching(address.id),
                })
                .map(|_| ())
                .map_err(Into::into);
        }
        (Err(e), false) => return record_launch_failure(&mut log, &e).map(|_| ()),
    };
    if let Err(e) = log.append(EventKind::RoundLaunched {
        handle: running.handle(),
    }) {
        // A round nobody could find again would run on uncollected.
        cancel_and_wait_out(running).await;
        return Err(e.into());
    }
    drop(launching);
    collect(
        running,
        &mut log,
        &paths.log(),
        &paths.position(round),
        cancel,
    )
    .await
    .map(|_| ())
}

/// Why a round the daemon gave up launching as it stopped has failed.
fn stopped_while_launching(job: JobId) -> String {
    format!(
        "the daemon stopped while this round was launching, so it was not started — \
         `submit --job {job}` to try again"
    )
}

/// Round `round`, launched by an earlier daemon and found again by
/// `handle`, collected into its log from where that daemon left off.
async fn reattach_round<R: Runner>(
    serving: &Serving<R>,
    address: &JobAddress,
    round: u32,
    handle: &RoundHandle,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let paths = address.paths();
    let mut log = EventLog::open_append(paths.events())?;
    match serving.daemon.runner.reattach(handle).await {
        Ok(running) => collect(
            running,
            &mut log,
            &paths.log(),
            &paths.position(round),
            cancel,
        )
        .await
        .map(|_| ()),
        Err(e) => log
            .append(EventKind::RoundFailed {
                reason: format!("could not find the round again after a restart: {e:#}"),
            })
            .map(|_| ())
            .map_err(Into::into),
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
