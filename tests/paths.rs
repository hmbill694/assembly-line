use assembly_line::job::JobId;
use assembly_line::paths::{
    RepoKey, create_job, git_root, job_id_past, latest_job_id, open_job, state_root,
};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn finds_the_git_root_by_walking_up() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let nested = root.join("a/b/c");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();

    assert_eq!(git_root(&nested).unwrap(), root);
    assert_eq!(git_root(&root).unwrap(), root);
}

#[test]
fn returns_none_outside_a_repo() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(git_root(tmp.path()).is_none());
}

#[test]
fn a_new_job_id_is_past_the_remotes_job_branches() {
    let remote = ["al/job-4".to_string(), "al/job-12".to_string()];
    assert_eq!(job_id_past(&remote), Some(JobId::from(13)));
    assert_eq!(job_id_past(&[]), Some(JobId::from(1)));
}

/// Anyone who can push can make a branch at the very last id; that is a
/// refusal to allocate, not an overflow.
#[test]
fn a_branch_at_the_last_id_leaves_none_to_allocate() {
    assert_eq!(job_id_past(&[JobId::from(u64::MAX).branch_name()]), None);
}

#[test]
fn branches_that_are_not_a_jobs_are_ignored_when_allocating() {
    let remote = [
        "main".to_string(),
        "al/job-x".to_string(),
        "al/jobs-9".to_string(),
        "feature/al/job-50".to_string(),
        "al/job-2".to_string(),
    ];
    assert_eq!(job_id_past(&remote), Some(JobId::from(3)));
}

#[test]
fn the_latest_job_is_the_highest_job_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    assert_eq!(latest_job_id(&jobs).unwrap(), None);

    create_job(&jobs, JobId::from(1)).unwrap();
    create_job(&jobs, JobId::from(2)).unwrap();
    assert_eq!(latest_job_id(&jobs).unwrap(), Some(JobId::from(2)));
}

#[test]
fn the_latest_job_ignores_directories_that_are_not_a_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    create_job(&jobs, JobId::from(7)).unwrap();
    fs::create_dir_all(jobs.join("notes")).unwrap();

    assert_eq!(latest_job_id(&jobs).unwrap(), Some(JobId::from(7)));
}

#[test]
fn create_job_lays_out_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    let p = create_job(&jobs, JobId::from(42)).unwrap();

    assert_eq!(p.id, JobId::from(42));
    assert_eq!(p.dir, jobs.join("42"));
    assert!(p.dir.is_dir());
    assert_eq!(p.events(), p.dir.join("events.jsonl"));
    assert_eq!(p.log(), p.dir.join("job.log"), "one job, one log");

    assert_eq!(open_job(&jobs, JobId::from(42)).unwrap().dir, p.dir);
}

#[test]
fn a_job_directory_that_exists_is_not_created_again() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs = tmp.path().join("jobs");
    create_job(&jobs, JobId::from(1)).unwrap();

    let err = create_job(&jobs, JobId::from(1)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn open_job_fails_for_a_missing_job() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(open_job(&tmp.path().join("jobs"), JobId::from(99)).is_err());
}

fn key(url: &str) -> String {
    RepoKey::from_remote_url(url).unwrap().to_string()
}

#[test]
fn a_hosted_repository_is_keyed_by_host_owner_and_name() {
    assert_eq!(key("https://github.com/o/r.git"), "github.com/o/r");
    assert_eq!(key("https://github.com/o/r"), "github.com/o/r");
    assert_eq!(key("https://github.com/o/r/"), "github.com/o/r");
}

/// Review focus 2: one repository, spelled two ways, is one set of jobs.
#[test]
fn ssh_and_https_spellings_of_one_repository_share_a_key() {
    for url in [
        "git@github.com:o/r.git",
        "ssh://git@github.com/o/r.git",
        "ssh://git@github.com:22/o/r.git",
        "https://GitHub.com/o/r.git",
    ] {
        assert_eq!(key(url), "github.com/o/r", "{url}");
    }
}

#[test]
fn a_repository_on_this_machine_is_keyed_under_local() {
    assert_eq!(key("/srv/git/origin.git"), "local/srv/git/origin");
    assert_eq!(key("file:///srv/git/origin.git"), "local/srv/git/origin");
}

#[test]
fn a_url_that_cannot_name_a_directory_is_refused_by_name() {
    for url in [
        "../origin",
        "https://github.com/o/../r",
        "https://github.com/o/r r",
        "",
    ] {
        let err = RepoKey::from_remote_url(url).unwrap_err();
        assert!(err.to_string().contains(url), "{url}: {err}");
    }
}

#[test]
fn a_keys_jobs_and_cache_live_under_the_root() {
    let key = RepoKey::from_remote_url("git@github.com:o/r.git").unwrap();

    assert_eq!(
        key.jobs_dir(Path::new("/state")),
        PathBuf::from("/state/jobs/github.com/o/r")
    );
    assert_eq!(
        key.repo_cache(Path::new("/state")),
        PathBuf::from("/state/repos/github.com/o/r.git")
    );
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

#[test]
fn a_named_root_wins_over_the_environment() {
    let root = state_root(Some("/named".into()), env_of(&[("HOME", "/home/u")])).unwrap();
    assert_eq!(root, PathBuf::from("/named"));
}

#[test]
fn the_default_root_follows_xdg_then_home() {
    assert_eq!(
        state_root(
            None,
            env_of(&[("XDG_STATE_HOME", "/xdg"), ("HOME", "/home/u")])
        )
        .unwrap(),
        PathBuf::from("/xdg/assembly-line")
    );
    assert_eq!(
        state_root(None, env_of(&[("HOME", "/home/u")])).unwrap(),
        PathBuf::from("/home/u/.local/state/assembly-line")
    );
}

#[test]
fn a_relative_xdg_state_home_is_ignored() {
    assert_eq!(
        state_root(
            None,
            env_of(&[("XDG_STATE_HOME", "state"), ("HOME", "/home/u")])
        )
        .unwrap(),
        PathBuf::from("/home/u/.local/state/assembly-line")
    );
}

#[test]
fn with_no_home_the_root_must_be_named() {
    let err = state_root(None, env_of(&[])).unwrap_err();
    assert!(err.to_string().contains("--root"), "{err}");
}
