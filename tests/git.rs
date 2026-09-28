use assembly_line::git::{
    self, DiffStat, check_out_new_branch, clone_into, commit_as_assembly_line, diff_stat_against,
    head_sha,
};
use std::path::{Path, PathBuf};
use support::commit_all;

mod support;

/// A repository with one commit, plus a sibling directory for scratch clones.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    clones: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let clones = tmp.path().join("clones");
        std::fs::create_dir_all(&clones).unwrap();
        support::init_git_repo(&repo).await;

        Fixture {
            _tmp: tmp,
            repo,
            clones,
        }
    }

    async fn base(&self) -> String {
        head_sha(&self.repo).await.unwrap()
    }

    async fn with_origin(&self) -> PathBuf {
        let origin = self.repo.parent().expect("a parent").join("origin.git");
        support::add_origin(&self.repo, &origin).await;
        origin
    }

    /// Clone the repo itself and check `branch` out at its current HEAD — a
    /// plain clone standing in for the scratch checkout a real job makes.
    async fn clone_on_branch(&self, name: &str, branch: &str) -> PathBuf {
        let base = self.base().await;
        clone_checked_out(&self.repo, &self.clones, name, branch, &base).await
    }
}

/// Clone `source` into a fresh directory under `into`, and check `branch` out
/// at `at`. The same two calls `workspace::create` makes to build a job's
/// scratch checkout.
async fn clone_checked_out(
    source: &Path,
    into: &Path,
    name: &str,
    branch: &str,
    at: &str,
) -> PathBuf {
    let path = into.join(name);
    std::fs::create_dir_all(&path).unwrap();
    clone_into(source.to_str().unwrap(), &path, None)
        .await
        .unwrap();
    check_out_new_branch(&path, branch, at).await.unwrap();
    commit_as_assembly_line(&path).await.unwrap();
    path
}

async fn write_and_commit(clone: &Path, name: &str, body: &str, message: &str) -> String {
    std::fs::write(clone.join(name), body).unwrap();
    commit_all(clone, message)
        .await
        .unwrap()
        .expect("something to commit")
}

#[tokio::test]
async fn reports_head_and_current_branch() {
    let fx = Fixture::new().await;

    assert_eq!(head_sha(&fx.repo).await.unwrap().len(), 40);
    assert_eq!(
        git::current_branch(&fx.repo).await.unwrap().as_deref(),
        Some("main")
    );
}

#[tokio::test]
async fn diff_stat_counts_files_and_lines_against_a_base() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    let node = fx.clone_on_branch("stat", "al/job-2").await;

    std::fs::write(node.join("one.txt"), "a\nb\nc\n").unwrap();
    std::fs::write(node.join("README.md"), "base\nextra\n").unwrap();
    commit_all(&node, "changes").await.unwrap().unwrap();

    assert_eq!(
        diff_stat_against(&node, &base).await.unwrap(),
        DiffStat {
            files: 2,
            insertions: 4,
            deletions: 0
        }
    );
}

#[tokio::test]
async fn diff_stat_of_an_unchanged_clone_is_empty() {
    let fx = Fixture::new().await;
    let base = fx.base().await;
    assert_eq!(
        diff_stat_against(&fx.repo, &base).await.unwrap(),
        DiffStat::default()
    );
}

#[tokio::test]
async fn a_failed_git_command_carries_gits_own_message() {
    let fx = Fixture::new().await;

    // A ref git itself rejects, so the error is git's stderr rather than a
    // spawn failure — which would never reach `stdout_or_error` at all.
    let err = git::sha_at_ref(&fx.repo, "no-such-ref")
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("no-such-ref"), "{err}");
    assert!(
        err.contains("unknown revision") || err.contains("Needed a single revision"),
        "not git's own message: {err}"
    );
}

/// Whether a bare repository carries `branch`, asked of the remote itself
/// rather than of the pushing side's tracking refs.
async fn remote_carries(origin: &Path, branch: &str) -> bool {
    git::run_allowing_failure(
        origin,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .unwrap()
    .succeeded()
}

/// A push happens from a job's own clone, not from the repository it was cut
/// from — so this pushes from a clone of `origin`, exactly as
/// `workspace::create` builds one.
#[tokio::test]
async fn pushing_a_branch_puts_it_on_the_remote() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    support::publish_main(&fx.repo).await;
    let base = fx.base().await;
    let node = clone_checked_out(&origin, &fx.clones, "node", "al/job-1", &base).await;
    write_and_commit(&node, "work.txt", "done\n", "node work").await;

    assert!(!remote_carries(&origin, "al/job-1").await);
    git::push_head_as(&node, "origin", "al/job-1")
        .await
        .unwrap();
    assert!(remote_carries(&origin, "al/job-1").await);
}

/// A revise round appends a commit to a branch that was already published, so
/// the second push must fast-forward rather than be rejected.
#[tokio::test]
async fn pushing_a_branch_again_after_another_commit_fast_forwards() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    support::publish_main(&fx.repo).await;
    let base = fx.base().await;
    let node = clone_checked_out(&origin, &fx.clones, "node", "al/job-1", &base).await;

    write_and_commit(&node, "work.txt", "round one\n", "round 1").await;
    git::push_head_as(&node, "origin", "al/job-1")
        .await
        .unwrap();

    let second = write_and_commit(&node, "work.txt", "round two\n", "round 2").await;
    git::push_head_as(&node, "origin", "al/job-1")
        .await
        .unwrap();

    let on_remote = git::run_allowing_failure(&origin, &["rev-parse", "al/job-1"])
        .await
        .unwrap();
    assert_eq!(on_remote.stdout.trim(), second);
}

/// Pushing the commit a branch already points at exits 0 as "up to date",
/// lease or not — which must still read as taken.
#[tokio::test]
async fn a_branch_already_at_the_commit_is_not_created_again() {
    let fx = Fixture::new().await;
    let _origin = fx.with_origin().await;
    let base = fx.base().await;

    assert!(
        git::create_branch_if_absent(&fx.repo, "origin", &base, "al/job-1")
            .await
            .unwrap()
    );
    assert!(
        !git::create_branch_if_absent(&fx.repo, "origin", &base, "al/job-1")
            .await
            .unwrap()
    );
}

/// A plain push would fast-forward a branch sitting at an ancestor — taking
/// over somebody else's job.
#[tokio::test]
async fn a_branch_at_an_ancestor_is_taken_not_fast_forwarded() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    let ancestor = fx.base().await;
    git::push_head_as(&fx.repo, "origin", "al/job-1")
        .await
        .unwrap();
    let descendant = write_and_commit(&fx.repo, "more.txt", "more\n", "more").await;

    assert!(
        !git::create_branch_if_absent(&fx.repo, "origin", &descendant, "al/job-1")
            .await
            .unwrap()
    );
    assert_eq!(
        git::sha_at_ref(&origin, "al/job-1").await.unwrap(),
        ancestor
    );
}

/// Pushes that all saw the branch absent lose on the remote itself, which
/// reports "reference already exists" rather than a lost lease.
#[tokio::test]
async fn of_pushes_racing_to_create_one_branch_exactly_one_creates_it() {
    let fx = Fixture::new().await;
    let _origin = fx.with_origin().await;
    let base = fx.base().await;

    let pushes: Vec<_> = (0..12)
        .map(|_| {
            let (repo, base) = (fx.repo.clone(), base.clone());
            tokio::spawn(async move {
                git::create_branch_if_absent(&repo, "origin", &base, "al/job-1").await
            })
        })
        .collect();
    let mut created = 0;
    for push in pushes {
        created += usize::from(push.await.unwrap().unwrap());
    }

    assert_eq!(created, 1);
}

#[tokio::test]
async fn creating_a_branch_on_a_missing_remote_is_an_error() {
    let fx = Fixture::new().await;
    let base = fx.base().await;

    let err = git::create_branch_if_absent(&fx.repo, "origin", &base, "al/job-1")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("al/job-1"), "{err}");
}

#[tokio::test]
async fn a_remotes_branches_are_listed_by_pattern_without_their_prefix() {
    let fx = Fixture::new().await;
    let origin = fx.with_origin().await;
    support::publish_main(&fx.repo).await;
    let base = fx.base().await;
    let node = clone_checked_out(&origin, &fx.clones, "node", "al/job-3", &base).await;
    git::push_head_as(&node, "origin", "al/job-3")
        .await
        .unwrap();

    let jobs = git::remote_branches_matching(&fx.repo, "origin", "al/job-*")
        .await
        .unwrap();
    assert_eq!(jobs, ["al/job-3"]);

    let unmatched = git::remote_branches_matching(&fx.repo, "origin", "nothing-*")
        .await
        .unwrap();
    assert!(unmatched.is_empty(), "{unmatched:?}");
}

/// Configuration is read from a ref, never from a checkout — which is what
/// stops a job editing the settings that govern it.
#[tokio::test]
async fn a_file_is_read_from_a_ref_not_the_working_tree() {
    let fx = Fixture::new().await;
    std::fs::write(fx.repo.join("marker"), "committed\n").unwrap();
    commit_all(&fx.repo, "add marker").await.unwrap().unwrap();

    // The working tree disagrees with HEAD.
    std::fs::write(fx.repo.join("marker"), "edited\n").unwrap();

    let at_head = git::file_at_ref(&fx.repo, "HEAD", "marker").await.unwrap();
    assert_eq!(at_head.as_deref(), Some("committed\n"));

    let absent = git::file_at_ref(&fx.repo, "HEAD", "nope").await.unwrap();
    assert_eq!(absent, None, "a missing path is None, not an error");
}

#[tokio::test]
async fn a_ref_resolves_to_the_commit_it_names() {
    let fx = Fixture::new().await;

    assert_eq!(
        git::sha_at_ref(&fx.repo, "main").await.unwrap(),
        head_sha(&fx.repo).await.unwrap()
    );
    assert!(git::sha_at_ref(&fx.repo, "no-such-ref").await.is_err());
}

#[tokio::test]
async fn a_remotes_url_is_read_back_and_a_missing_remote_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    assert_eq!(git::remote_url(&repo, "origin").await.unwrap(), None);

    let origin = tmp.path().join("origin.git");
    support::add_origin(&repo, &origin).await;
    assert_eq!(
        git::remote_url(&repo, "origin").await.unwrap().as_deref(),
        Some(origin.to_str().unwrap())
    );
}

/// Git reads a relative remote path from the repository; a job's clone runs
/// from a scratch directory, where the same path names nothing.
#[tokio::test]
async fn a_relative_remote_path_is_resolved_against_the_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    git::run_allowing_failure(&repo, &["remote", "add", "origin", "../origin.git"])
        .await
        .unwrap();

    assert_eq!(
        git::remote_url(&repo, "origin").await.unwrap(),
        Some(repo.join("../origin.git").to_string_lossy().into_owned())
    );
}

#[tokio::test]
async fn remote_urls_that_name_a_host_are_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;

    for url in [
        "git@example.com:team/repo.git",
        "https://example.com/team/repo.git",
    ] {
        git::run_allowing_failure(&repo, &["remote", "add", "origin", url])
            .await
            .unwrap();
        assert_eq!(
            git::remote_url(&repo, "origin").await.unwrap().as_deref(),
            Some(url)
        );
        git::run_allowing_failure(&repo, &["remote", "remove", "origin"])
            .await
            .unwrap();
    }
}

/// A job's clone pushes back to where it cloned from, so a remote that
/// pushes elsewhere would send the job's branch to the wrong place.
#[tokio::test]
async fn a_remote_that_pushes_somewhere_else_is_refused_with_what_to_change() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    git::run_allowing_failure(
        &repo,
        &["remote", "set-url", "--push", "origin", "/elsewhere.git"],
    )
    .await
    .unwrap();

    let err = git::remote_url(&repo, "origin")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("remote.origin.pushurl"), "{err}");
}

/// A push rewrite in the repository's own config is as invisible to the
/// clone as a pushurl, and the refusal names the setting that caused it.
#[tokio::test]
async fn a_push_rewrite_only_this_repository_holds_is_refused_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let origin = tmp.path().join("origin.git");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &origin).await;
    let rewrite_key = format!(
        "url.{}.pushInsteadOf",
        tmp.path().join("elsewhere.git").display()
    );
    git::run_allowing_failure(&repo, &["config", &rewrite_key, origin.to_str().unwrap()])
        .await
        .unwrap();

    let err = git::remote_url(&repo, "origin")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("pushinsteadof"), "{err}");
    assert!(err.contains("global config"), "{err}");
}

/// A job starts from what the remote says a ref is, not what the local
/// repository says: unpushed work is not something a clone can see.
#[tokio::test]
async fn a_ref_is_pinned_to_the_commit_the_remote_has_for_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;
    let pushed = head_sha(&repo).await.unwrap();

    std::fs::write(repo.join("unpushed.txt"), "local only\n").unwrap();
    commit_all(&repo, "unpushed").await.unwrap().unwrap();

    let pinned = git::pinned(&repo, "origin", "main").await.unwrap();
    assert_eq!(
        pinned,
        git::PinnedRef {
            name: "main".into(),
            sha: pushed
        }
    );
}

#[tokio::test]
async fn a_ref_the_remote_does_not_have_cannot_be_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    support::add_origin(&repo, &tmp.path().join("origin.git")).await;
    support::publish_main(&repo).await;

    let err = git::pinned(&repo, "origin", "never-pushed")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("never-pushed"), "{err}");
    assert!(err.to_string().contains("push it first"), "{err}");
}

/// Pushing cannot fix a remote that does not answer, so a fetch that failed
/// for any reason other than the ref being absent keeps git's own complaint.
#[tokio::test]
async fn a_remote_that_cannot_be_reached_is_not_told_to_push_first() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    let nowhere = tmp.path().join("no-such-origin.git");
    let added = git::run_allowing_failure(
        &repo,
        &["remote", "add", "origin", nowhere.to_str().unwrap()],
    )
    .await
    .unwrap();
    assert!(added.succeeded(), "{}", added.stderr);

    let err = git::pinned(&repo, "origin", "main")
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("push it first"), "{err}");
    assert!(err.contains("fetch origin main"), "{err}");
}

#[tokio::test]
async fn a_clone_checks_out_a_new_branch_at_the_commit_it_is_given() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    let origin = tmp.path().join("origin.git");
    support::add_origin(&repo, &origin).await;
    support::publish_main(&repo).await;
    let at = head_sha(&repo).await.unwrap();

    let clone = tmp.path().join("clone");
    std::fs::create_dir_all(&clone).unwrap();
    git::clone_into(origin.to_str().unwrap(), &clone, None)
        .await
        .unwrap();
    git::check_out_new_branch(&clone, "al/job-1", &at)
        .await
        .unwrap();

    assert_eq!(head_sha(&clone).await.unwrap(), at);
    assert_eq!(
        git::current_branch(&clone).await.unwrap().as_deref(),
        Some("al/job-1")
    );
    assert!(clone.join("README.md").is_file());
}

/// A clone of a fresh remote under `tmp`, made with the token helper.
async fn clone_with_the_token_helper(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    support::init_git_repo(&repo).await;
    let origin = tmp.join("origin.git");
    support::add_origin(&repo, &origin).await;
    support::publish_main(&repo).await;

    let clone = tmp.join("clone");
    std::fs::create_dir_all(&clone).unwrap();
    git::clone_into(
        origin.to_str().unwrap(),
        &clone,
        Some(git::TOKEN_CREDENTIAL_HELPER),
    )
    .await
    .unwrap();
    clone
}

/// `git credential <action>` in `clone`, fed `input`, as git would run it
/// during a push. `global_config` stands in for the machine's own config, so
/// the real one never takes part.
fn git_credential(clone: &Path, global_config: &Path, action: &str, input: &str) -> String {
    let mut git = std::process::Command::new("git")
        .args(["credential", action])
        .current_dir(clone)
        .env("GIT_CONFIG_GLOBAL", global_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("ASSEMBLY_GIT_TOKEN", "t0ken")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(git.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
    String::from_utf8(git.wait_with_output().unwrap().stdout).unwrap()
}

/// The token helper stays configured in the clone, so the push that ends the
/// round authenticates the same way the clone did — and it answers with the
/// token from the environment.
#[tokio::test]
async fn a_clone_given_the_token_helper_answers_git_with_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let clone = clone_with_the_token_helper(tmp.path()).await;

    let answer = git_credential(
        &clone,
        Path::new("/dev/null"),
        "fill",
        "protocol=https\nhost=github.com\n\n",
    );

    assert!(answer.contains("username=x-access-token"), "{answer}");
    assert!(answer.contains("password=t0ken"), "{answer}");
}

/// Once a credential works, git offers it to every configured helper to
/// `store` — a keychain or a plaintext file would then keep the token. The
/// token helper replaces the machine's helpers instead of joining them.
#[tokio::test]
async fn the_token_helper_replaces_every_helper_the_machine_configures() {
    let tmp = tempfile::tempdir().unwrap();
    let clone = clone_with_the_token_helper(tmp.path()).await;
    let consulted = tmp.path().join("consulted");
    let machine_helper = support::fake_cli(
        &tmp.path().join("fakes"),
        "machine-helper",
        &format!("echo \"$1\" >> {}\n", consulted.display()),
    );
    let global_config = tmp.path().join("gitconfig");
    std::fs::write(
        &global_config,
        format!("[credential]\n\thelper = {}\n", machine_helper.display()),
    )
    .unwrap();

    let answer = git_credential(
        &clone,
        &global_config,
        "fill",
        "protocol=https\nhost=github.com\n\n",
    );
    git_credential(&clone, &global_config, "approve", &answer);

    assert!(answer.contains("password=t0ken"), "{answer}");
    assert!(
        !consulted.exists(),
        "the machine's helper was asked: {}",
        std::fs::read_to_string(&consulted).unwrap_or_default()
    );
}

/// A clone that is given up on — dropped by a timeout around it — takes the
/// `ssh` it started with it, rather than leaving it running.
#[tokio::test]
async fn a_git_given_up_on_takes_what_it_started_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    // A stand-in `ssh` that hangs, named uniquely enough to look for.
    let hang = format!("sleep 9{}", std::process::id());
    let ssh_command = format!("core.sshCommand={hang} #");

    let gave_up = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        git::run_allowing_failure(
            tmp.path(),
            &[
                "-c",
                &ssh_command,
                "clone",
                "ssh://example.invalid/r.git",
                ".",
            ],
        ),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(gave_up.is_err(), "the stand-in ssh did not hang");
    let still_running = std::process::Command::new("pgrep")
        .args(["-f", &hang])
        .output()
        .unwrap();
    assert!(
        !still_running.status.success(),
        "the ssh outlived its git: {}",
        String::from_utf8_lossy(&still_running.stdout)
    );
}
