use assembly_line::job::JobId;
use assembly_line::paths::{
    JobPlace, RepoKey, existing_job_ids, git_root, job_id_past, latest_job_id, state_root,
};
use assembly_line::remote::Remote;
use std::fs;
use std::path::{Path, PathBuf};

fn key_of(url: &str) -> RepoKey {
    Remote::from_url(url).repo_key().unwrap()
}

fn place(url: &str, id: u64) -> JobPlace {
    JobPlace {
        key: key_of(url),
        id: JobId::from(id),
    }
}

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
    let root = tempfile::tempdir().unwrap();
    let jobs = key_of("/o.git").jobs_dir(root.path());
    assert_eq!(latest_job_id(&jobs).unwrap(), None);

    place("/o.git", 1).create(root.path()).unwrap();
    place("/o.git", 2).create(root.path()).unwrap();
    assert_eq!(latest_job_id(&jobs).unwrap(), Some(JobId::from(2)));
}

/// `007` is no name a job is created under, so it is not job 7.
#[test]
fn the_latest_job_ignores_directories_that_are_not_a_jobs() {
    let root = tempfile::tempdir().unwrap();
    let job = place("/o.git", 2).create(root.path()).unwrap();
    let jobs = job.dir.parent().unwrap();
    fs::create_dir_all(jobs.join("notes")).unwrap();
    fs::create_dir_all(jobs.join("007")).unwrap();

    assert_eq!(latest_job_id(jobs).unwrap(), Some(JobId::from(2)));
}

#[test]
fn a_job_is_created_and_opened_at_its_place() {
    let root = tempfile::tempdir().unwrap();
    let place = place("/o.git", 42);
    let p = place.create(root.path()).unwrap();

    assert_eq!(p.id, JobId::from(42));
    assert_eq!(p.dir, place.key.jobs_dir(root.path()).join("42"));
    assert_eq!(p.dir, place.paths(root.path()).dir);
    assert!(p.dir.is_dir());
    assert_eq!(p.events(), p.dir.join("events.jsonl"));
    assert_eq!(p.log(), p.dir.join("job.log"), "one job, one log");

    assert_eq!(place.open(root.path()).unwrap().dir, p.dir);
}

#[test]
fn a_job_directory_that_exists_is_not_created_again() {
    let root = tempfile::tempdir().unwrap();
    place("/o.git", 1).create(root.path()).unwrap();

    let err = place("/o.git", 1).create(root.path()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn opening_a_missing_job_fails_as_not_found() {
    let root = tempfile::tempdir().unwrap();
    let err = place("/o.git", 99).open(root.path()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

/// `/x/a/7.git`'s jobs directory and `/x/a.git`'s job 7 used to be one
/// directory.
#[test]
fn a_nested_repository_does_not_take_another_repositorys_job() {
    let root = tempfile::tempdir().unwrap();
    let nested = place("/x/a/7.git", 1);
    let outer = place("/x/a.git", 7);

    nested.create(root.path()).unwrap();
    outer.create(root.path()).unwrap();

    assert_eq!(
        existing_job_ids(&nested.key.jobs_dir(root.path())).unwrap(),
        [JobId::from(1)]
    );
    assert_eq!(
        existing_job_ids(&outer.key.jobs_dir(root.path())).unwrap(),
        [JobId::from(7)]
    );
}

#[test]
fn a_host_named_local_is_not_this_machine() {
    let hosted = key_of("https://local/x/a");
    let here = key_of("/x/a.git");
    let root = Path::new("/state");

    assert_ne!(hosted, here);
    assert_ne!(hosted.jobs_dir(root), here.jobs_dir(root));
    assert_ne!(hosted.repo_cache(root), here.repo_cache(root));
}

#[test]
fn a_repositorys_cache_is_not_inside_another_repositorys() {
    let root = Path::new("/state");
    let outer = key_of("/x/a.git");
    let inner = key_of("/x/a.git/b");

    assert!(!inner.repo_cache(root).starts_with(outer.repo_cache(root)));
    assert!(!inner.jobs_dir(root).starts_with(outer.jobs_dir(root)));
}

#[test]
fn a_key_is_found_again_from_its_directory_name() {
    for url in ["/x/a.git", "https://local/x/a", "git@github.com:o/r.git"] {
        let key = key_of(url);
        assert_eq!(RepoKey::from_dir_name(&key.dir_name()), Some(key), "{url}");
    }
}

#[test]
fn a_directory_name_no_key_makes_is_no_key() {
    for name in [
        "notes",
        "github.com~",
        "~o",
        "github.com~~r",
        "github.com~..",
        "@locals~o",
        "a b~c",
    ] {
        assert_eq!(RepoKey::from_dir_name(name), None, "{name}");
    }
}

#[test]
fn a_keys_jobs_and_cache_live_under_the_root() {
    let key = key_of("git@github.com:o/r.git");

    assert_eq!(
        key.jobs_dir(Path::new("/state")),
        PathBuf::from("/state/jobs/github.com~o~r")
    );
    assert_eq!(
        key.repo_cache(Path::new("/state")),
        PathBuf::from("/state/repos/github.com~o~r.git")
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
