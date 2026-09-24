use assembly_line::paths::{
    JobMeta, create_job, git_root, jobs_root, latest_job_id, next_job_id, open_job, read_meta,
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
fn allocates_monotonic_job_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());

    assert_eq!(next_job_id(&root).unwrap(), 1);
    assert_eq!(latest_job_id(&root).unwrap(), None);

    create_job(&root, 1).unwrap();
    assert_eq!(next_job_id(&root).unwrap(), 2);
    assert_eq!(latest_job_id(&root).unwrap(), Some(1));

    create_job(&root, 2).unwrap();
    assert_eq!(next_job_id(&root).unwrap(), 3);
    assert_eq!(latest_job_id(&root).unwrap(), Some(2));
}

#[test]
fn ignores_non_numeric_directories_when_allocating() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());
    fs::create_dir_all(root.join("scratch")).unwrap();
    create_job(&root, 7).unwrap();
    assert_eq!(next_job_id(&root).unwrap(), 8);
}

#[test]
fn create_job_lays_out_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = jobs_root(tmp.path());
    let p = create_job(&root, 42).unwrap();

    assert_eq!(p.id, 42);
    assert!(p.dir.is_dir());
    assert_eq!(p.events(), p.dir.join("events.jsonl"));
    assert_eq!(p.meta(), p.dir.join("meta.json"));
    assert_eq!(p.log(), p.dir.join("job.log"), "one job, one log");

    assert_eq!(open_job(&root, 42).unwrap().dir, p.dir);
}

#[test]
fn open_job_fails_for_a_missing_job() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(open_job(&jobs_root(tmp.path()), 99).is_err());
}

#[test]
fn meta_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let p = create_job(&jobs_root(tmp.path()), 1).unwrap();
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
    let job = create_job(&jobs_root(tmp.path()), 1).unwrap();
    fs::write(
        job.meta(),
        r#"{"repo":"/work/acme","base_ref":"main","prompt":"go","provider":"claude","branch":"al/job-1"}"#,
    )
    .unwrap();

    let meta = read_meta(&job).unwrap();
    assert_eq!(meta.prompt, "go");
    assert_eq!(meta.base_ref, "main");
}
