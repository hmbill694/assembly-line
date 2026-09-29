//! A real `assembly daemon`, started on a root the test owns and stopped
//! when the test is done with it.

use assembly_line::event::EventLog;
use assembly_line::report::JobReport;
use assembly_line::state::JobState;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub struct RunningDaemon {
    child: Option<Child>,
    pub root: PathBuf,
}

impl RunningDaemon {
    /// Start `assembly daemon` on `root` with `args`, and wait until it
    /// answers on its socket.
    ///
    /// # Panics
    ///
    /// If it exits, or does not answer within ten seconds.
    pub fn start(root: &Path, args: &[&str], env: &[(&str, &str)]) -> RunningDaemon {
        let mut child = Command::new(env!("CARGO_BIN_EXE_assembly"))
            .args(["daemon", "--root"])
            .arg(root)
            .args(args)
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let socket = root.join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            if let Some(status) = child.try_wait().unwrap() {
                let stderr = std::io::read_to_string(child.stderr.take().unwrap()).unwrap();
                panic!("the daemon exited {status}: {stderr}");
            }
            if Instant::now() >= deadline {
                stop_within_five_seconds(&mut child);
                panic!("the daemon never answered on {}", socket.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        RunningDaemon {
            child: Some(child),
            root: root.to_path_buf(),
        }
    }

    /// SIGTERM, and wait for it to exit.
    ///
    /// # Panics
    ///
    /// If it has not exited within twenty seconds.
    pub fn stop(self) -> ExitStatus {
        self.stop_with(nix::sys::signal::Signal::SIGTERM)
    }

    /// Send `stopping_signal`, and wait for it to exit.
    ///
    /// # Panics
    ///
    /// If it has not exited within twenty seconds.
    pub fn stop_with(mut self, stopping_signal: nix::sys::signal::Signal) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        signal(&child, stopping_signal);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the daemon ignored {stopping_signal} for twenty seconds");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// SIGKILL: no chance to clean anything up.
    pub fn kill(mut self) {
        let mut child = self.child.take().unwrap();
        signal(&child, nix::sys::signal::Signal::SIGKILL);
        child.wait().unwrap();
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            stop_within_five_seconds(&mut child);
        }
    }
}

/// SIGTERM first, so the daemon cleans up after itself; SIGKILL if it has
/// not gone in five seconds.
fn stop_within_five_seconds(child: &mut Child) {
    signal(child, nix::sys::signal::Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Fold `job_dir`'s log until its latest round has a verdict.
///
/// # Panics
///
/// If there is none within thirty seconds.
pub fn wait_for_verdict(job_dir: &Path) -> JobReport {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = EventLog::read(job_dir.join("events.jsonl")).unwrap_or_default();
        let report = JobReport::from_events(0, &events);
        if matches!(report.state, JobState::Passed | JobState::Failed) {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "no verdict in {}: {events:?}",
            job_dir.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait until `job_dir`'s output log carries `text` — an agent's first
/// line, say, which proves its round got that far.
///
/// # Panics
///
/// If it does not within thirty seconds.
pub fn wait_until_logged(job_dir: &Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !std::fs::read_to_string(job_dir.join("job.log")).is_ok_and(|log| log.contains(text)) {
        assert!(
            Instant::now() < deadline,
            "{} never logged {text:?}",
            job_dir.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fold `job_dir`'s log until its latest round has been launched.
///
/// # Panics
///
/// If it has not within thirty seconds.
pub fn wait_until_launched(job_dir: &Path) -> JobReport {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let events = EventLog::read(job_dir.join("events.jsonl")).unwrap_or_default();
        let report = JobReport::from_events(0, &events);
        if report.launched.is_some() {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "never launched in {}: {events:?}",
            job_dir.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn signal(child: &Child, signal: nix::sys::signal::Signal) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap()),
        signal,
    );
}
