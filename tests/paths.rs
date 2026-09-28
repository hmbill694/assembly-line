use assembly_line::job::JobId;
use assembly_line::paths::{
    JobMeta, create_job, git_root, job_id_past, jobs_root, latest_job_id, open_job, read_meta,
    write_meta,
};
use std::fs;

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
    let root = jobs_root(tmp.path());
    assert_eq!(latest_job_id(&root).unwrap(), None);

    create_job(&root, JobId::from(1)).unwrap();
    create_job(&root, JobId::from(2)).unwrap();
    assert_eq!(latest_job_id(&root).unwrap(), Some(JobId::from(2)));
}

#[test]
fn the_latest_job_ignores_directories_that_are_not_a_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());
    create_job(&root, JobId::from(7)).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();

    assert_eq!(latest_job_id(&root).unwrap(), Some(JobId::from(7)));
}

#[test]
fn create_job_lays_out_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());
    let p = create_job(&root, JobId::from(42)).unwrap();

    assert_eq!(p.id, JobId::from(42));
    assert_eq!(p.dir, root.join("42"));
    assert!(p.dir.is_dir());
    assert_eq!(p.events(), p.dir.join("events.jsonl"));
    assert_eq!(p.meta(), p.dir.join("meta.json"));
    assert_eq!(p.log(), p.dir.join("job.log"), "one job, one log");

    assert_eq!(open_job(&root, JobId::from(42)).unwrap().dir, p.dir);
}

#[test]
fn a_job_directory_that_exists_is_not_created_again() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());
    create_job(&root, JobId::from(1)).unwrap();

    let err = create_job(&root, JobId::from(1)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn open_job_fails_for_a_missing_job() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(open_job(&jobs_root(tmp.path()), JobId::from(99)).is_err());
}

#[test]
fn meta_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let p = create_job(&jobs_root(tmp.path()), JobId::from(1)).unwrap();
    write_meta(
        &p,
        &JobMeta {
            repo: "/work/acme".into(),
            base_ref: "main".into(),
            prompt: "add authentication".into(),
            provider: "claude".into(),
        },
    )
    .unwrap();

    let back = read_meta(&p).unwrap();
    assert_eq!(back.repo, std::path::PathBuf::from("/work/acme"));
    assert_eq!(back.base_ref, "main");
    assert_eq!(back.prompt, "add authentication");
    assert_eq!(back.provider, "claude");
}

/// `revise` needs the prompt and the ref by id alone, which is exactly why
/// they live here rather than only in the event log.
#[test]
fn meta_written_by_an_older_version_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let job = create_job(&jobs_root(tmp.path()), JobId::from(1)).unwrap();
    fs::write(
        job.meta(),
        r#"{"repo":"/work/acme","base_ref":"main","prompt":"go","provider":"claude","branch":"al/job-1"}"#,
    )
    .unwrap();

    let meta = read_meta(&job).unwrap();
    assert_eq!(meta.prompt, "go");
    assert_eq!(meta.base_ref, "main");
}
