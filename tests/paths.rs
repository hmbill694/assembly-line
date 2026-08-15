use assembly_line::paths::{
    RunMeta, create_run, git_root, latest_run_id, next_run_id, open_run, read_meta, runs_root,
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
fn allocates_monotonic_run_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let root = runs_root(tmp.path());

    assert_eq!(next_run_id(&root).unwrap(), 1);
    assert_eq!(latest_run_id(&root).unwrap(), None);

    create_run(&root, 1).unwrap();
    assert_eq!(next_run_id(&root).unwrap(), 2);
    assert_eq!(latest_run_id(&root).unwrap(), Some(1));

    create_run(&root, 2).unwrap();
    assert_eq!(next_run_id(&root).unwrap(), 3);
    assert_eq!(latest_run_id(&root).unwrap(), Some(2));
}

#[test]
fn ignores_non_numeric_directories_when_allocating() {
    let tmp = tempfile::tempdir().unwrap();
    let root = runs_root(tmp.path());
    fs::create_dir_all(root.join("scratch")).unwrap();
    create_run(&root, 7).unwrap();
    assert_eq!(next_run_id(&root).unwrap(), 8);
}

#[test]
fn create_run_lays_out_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = runs_root(tmp.path());
    let p = create_run(&root, 42).unwrap();

    assert_eq!(p.id, 42);
    assert!(p.dir.is_dir());
    assert!(p.logs_dir().is_dir());
    assert_eq!(p.events(), p.dir.join("events.jsonl"));
    assert_eq!(p.meta(), p.dir.join("meta.json"));
    assert_eq!(p.log("impl-auth"), p.logs_dir().join("impl-auth.log"));

    assert_eq!(open_run(&root, 42).unwrap().dir, p.dir);
}

#[test]
fn open_run_fails_for_a_missing_run() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(open_run(&runs_root(tmp.path()), 99).is_err());
}

#[test]
fn meta_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let p = create_run(&runs_root(tmp.path()), 1).unwrap();
    write_meta(
        &p,
        &RunMeta {
            graph: "graphs/a.toml".into(),
            jobs: 3,
        },
    )
    .unwrap();

    let back = read_meta(&p).unwrap();
    assert_eq!(back.graph, std::path::PathBuf::from("graphs/a.toml"));
    assert_eq!(back.jobs, 3);
}
