use assembly_line::paths::{
    JobMeta, create_job, git_root, jobs_root, latest_job_id, next_job_id, open_job, read_meta,
    record_repository_for_worktrees, repo_slug, repository_owning_worktrees, worktree_root,
    worktrees_root_given, write_meta,
};
use std::fs;
use std::path::Path;

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

/// The checkout sits below the job's worktree directory rather than being it,
/// so discarding the checkout still leaves `gc` something to find.
#[test]
fn a_jobs_checkout_lives_below_its_worktree_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let p = create_job(&jobs_root(tmp.path()), 3).unwrap();
    let repo = Path::new("/work/acme");

    let checkout = p
        .worktree(repo)
        .expect("HOME is set in the test environment");
    assert!(checkout.starts_with(worktree_root(repo, 3).unwrap()));
    assert_ne!(checkout, worktree_root(repo, 3).unwrap());
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

#[test]
fn worktrees_live_under_home_not_in_the_repo() {
    let repo = Path::new("/work/acme");
    let root = worktree_root(repo, 42).expect("HOME is set in the test environment");

    assert!(
        root.starts_with(std::env::var("HOME").unwrap()),
        "{}",
        root.display()
    );
    assert!(!root.starts_with(repo), "{}", root.display());
    assert!(
        root.ends_with(format!("{}/42", repo_slug(repo))),
        "{}",
        root.display()
    );
}

#[test]
fn two_repositories_on_the_same_job_id_do_not_share_a_worktree_root() {
    // Job ids restart at 1 in every repository, so the id alone cannot key the
    // directory — the first job of two repos would land in the same place.
    let alpha = worktree_root(Path::new("/work/alpha"), 1).unwrap();
    let beta = worktree_root(Path::new("/work/beta"), 1).unwrap();
    assert_ne!(alpha, beta);
}

#[test]
fn a_repo_slug_names_the_repository_and_still_separates_same_named_ones() {
    let slug = repo_slug(Path::new("/work/my-repo"));

    assert!(slug.starts_with("my-repo-"), "{slug}");
    assert_eq!(slug, repo_slug(Path::new("/work/my-repo")), "not stable");
    assert_ne!(
        slug,
        repo_slug(Path::new("/elsewhere/my-repo")),
        "same name at a different path must not collide"
    );
}

#[test]
fn a_repo_slug_is_usable_as_a_directory_name() {
    let slug = repo_slug(Path::new("/work/we ird:na/me?"));
    assert!(
        slug.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "{slug}"
    );
}

#[test]
fn the_repository_owning_a_worktree_directory_can_be_read_back() {
    // The slug is a hash, so gc cannot invert it — the marker is how a
    // worktree directory says which repository it belongs to.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("some-repo");
    fs::create_dir_all(&repo).unwrap();

    let dir = record_repository_for_worktrees(&repo).unwrap();
    let owner = repository_owning_worktrees(&dir);
    fs::remove_dir_all(&dir).unwrap();

    assert_eq!(owner.unwrap(), repo);
}

#[test]
fn a_worktree_directory_without_a_marker_owns_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(repository_owning_worktrees(tmp.path()).is_none());
}

#[test]
fn the_override_wins_over_home_and_is_used_verbatim() {
    let chosen = worktrees_root_given(Some("/fast/disk/wt"), Some("/home/dev"));
    assert_eq!(chosen.unwrap(), Path::new("/fast/disk/wt"));
}

#[test]
fn without_an_override_worktrees_land_under_home() {
    let chosen = worktrees_root_given(None::<&str>, Some("/home/dev"));
    assert_eq!(chosen.unwrap(), Path::new("/home/dev/.assembly/wt"));
}

#[test]
fn with_neither_set_there_is_no_worktree_root_to_guess() {
    assert!(worktrees_root_given(None::<&str>, None::<&str>).is_none());
}

#[test]
fn an_override_alone_is_enough_even_with_no_home() {
    let chosen = worktrees_root_given(Some("/scratch"), None::<&str>);
    assert_eq!(chosen.unwrap(), Path::new("/scratch"));
}
