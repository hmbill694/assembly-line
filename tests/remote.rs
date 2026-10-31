use assembly_line::config::{JobSettings, RepoConfig};
use assembly_line::git::{self, PinnedRef};
use assembly_line::paths::{JobPaths, RepoKey};
use assembly_line::remote::Remote;
use assembly_line::report::{JobRequests, RoundRequest};
use assembly_line::runner::docker::DockerRunner;
use assembly_line::runner::kubernetes::KubernetesRunner;
use assembly_line::runner::local::LocalRunner;
use assembly_line::runner::{LaunchSpec, Runner};
use chrono::Utc;
use std::path::PathBuf;

mod support;

/// What each way of spelling a remote comes to. `key` is `None` for a
/// remote the daemon refuses, since it names no directory to keep state in.
struct Spelling {
    url: &'static str,
    on_this_machine: bool,
    https: &'static str,
    key: Option<&'static str>,
    launched_on_the_host: &'static str,
    launched_in_a_container: &'static str,
}

const SPELLINGS: &[Spelling] = &[
    Spelling {
        url: "/srv/git/r.git",
        on_this_machine: true,
        https: "/srv/git/r.git",
        key: Some("@local/srv/git/r"),
        launched_on_the_host: "file:///srv/git/r.git",
        launched_in_a_container: "/srv/git/r.git",
    },
    Spelling {
        url: "/srv/git/r/",
        on_this_machine: true,
        https: "/srv/git/r/",
        key: Some("@local/srv/git/r"),
        launched_on_the_host: "file:///srv/git/r/",
        launched_in_a_container: "/srv/git/r/",
    },
    Spelling {
        url: "/x/a.git.git",
        on_this_machine: true,
        https: "/x/a.git.git",
        key: Some("@local/x/a.git"),
        launched_on_the_host: "file:///x/a.git.git",
        launched_in_a_container: "/x/a.git.git",
    },
    Spelling {
        url: "/x/a.git/b",
        on_this_machine: true,
        https: "/x/a.git/b",
        key: Some("@local/x/a.git/b"),
        launched_on_the_host: "file:///x/a.git/b",
        launched_in_a_container: "/x/a.git/b",
    },
    Spelling {
        url: "file:///srv/git/r.git",
        on_this_machine: true,
        https: "file:///srv/git/r.git",
        key: Some("@local/srv/git/r"),
        launched_on_the_host: "file:///srv/git/r.git",
        launched_in_a_container: "file:///srv/git/r.git",
    },
    Spelling {
        url: "https://github.com/o/r.git",
        on_this_machine: false,
        https: "https://github.com/o/r.git",
        key: Some("github.com/o/r"),
        launched_on_the_host: "https://github.com/o/r.git",
        launched_in_a_container: "https://github.com/o/r.git",
    },
    Spelling {
        url: "https://github.com/o/r",
        on_this_machine: false,
        https: "https://github.com/o/r",
        key: Some("github.com/o/r"),
        launched_on_the_host: "https://github.com/o/r",
        launched_in_a_container: "https://github.com/o/r",
    },
    Spelling {
        url: "https://github.com/o/r/",
        on_this_machine: false,
        https: "https://github.com/o/r/",
        key: Some("github.com/o/r"),
        launched_on_the_host: "https://github.com/o/r/",
        launched_in_a_container: "https://github.com/o/r/",
    },
    Spelling {
        url: "https://GitHub.com/o/r.git",
        on_this_machine: false,
        https: "https://GitHub.com/o/r.git",
        key: Some("github.com/o/r"),
        launched_on_the_host: "https://GitHub.com/o/r.git",
        launched_in_a_container: "https://GitHub.com/o/r.git",
    },
    Spelling {
        url: "https://user@github.com:8443/o/r.git",
        on_this_machine: false,
        https: "https://user@github.com:8443/o/r.git",
        key: Some("github.com+8443/o/r"),
        launched_on_the_host: "https://user@github.com:8443/o/r.git",
        launched_in_a_container: "https://user@github.com:8443/o/r.git",
    },
    Spelling {
        url: "https://h:443/o/r",
        on_this_machine: false,
        https: "https://h:443/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "https://h:443/o/r",
        launched_in_a_container: "https://h:443/o/r",
    },
    Spelling {
        url: "https://h:/o/r",
        on_this_machine: false,
        https: "https://h:/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "https://h:/o/r",
        launched_in_a_container: "https://h:/o/r",
    },
    Spelling {
        url: "http://h:80/o/r",
        on_this_machine: false,
        https: "http://h:80/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "http://h:80/o/r",
        launched_in_a_container: "http://h:80/o/r",
    },
    Spelling {
        url: "http://h:443/o/r",
        on_this_machine: false,
        https: "http://h:443/o/r",
        key: Some("h+443/o/r"),
        launched_on_the_host: "http://h:443/o/r",
        launched_in_a_container: "http://h:443/o/r",
    },
    Spelling {
        url: "https://h:abc/o/r",
        on_this_machine: false,
        https: "https://h:abc/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "https://h:abc/o/r",
        launched_in_a_container: "https://h:abc/o/r",
    },
    Spelling {
        url: "git+ssh://h:22/o/r",
        on_this_machine: false,
        https: "git+ssh://h:22/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "git+ssh://h:22/o/r",
        launched_in_a_container: "git+ssh://h:22/o/r",
    },
    Spelling {
        url: "/tmp/x://y",
        on_this_machine: false,
        https: "/tmp/x://y",
        key: None,
        launched_on_the_host: "file:///tmp/x://y",
        launched_in_a_container: "/tmp/x://y",
    },
    Spelling {
        url: "git://h:9418/o/r",
        on_this_machine: false,
        https: "git://h:9418/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "git://h:9418/o/r",
        launched_in_a_container: "git://h:9418/o/r",
    },
    Spelling {
        url: "ssh://h:22/o/r",
        on_this_machine: false,
        https: "https://h/o/r",
        key: Some("h/o/r"),
        launched_on_the_host: "ssh://h:22/o/r",
        launched_in_a_container: "https://h/o/r",
    },
    Spelling {
        url: "ssh://git@h:2222/o/r",
        on_this_machine: false,
        https: "https://h/o/r",
        key: Some("h+2222/o/r"),
        launched_on_the_host: "ssh://git@h:2222/o/r",
        launched_in_a_container: "https://h/o/r",
    },
    Spelling {
        url: "https://local/x/a",
        on_this_machine: false,
        https: "https://local/x/a",
        key: Some("local/x/a"),
        launched_on_the_host: "https://local/x/a",
        launched_in_a_container: "https://local/x/a",
    },
    Spelling {
        url: "git://example.com/o/r.git",
        on_this_machine: false,
        https: "git://example.com/o/r.git",
        key: Some("example.com/o/r"),
        launched_on_the_host: "git://example.com/o/r.git",
        launched_in_a_container: "git://example.com/o/r.git",
    },
    Spelling {
        url: "ssh://git@github.com/o/r.git",
        on_this_machine: false,
        https: "https://github.com/o/r.git",
        key: Some("github.com/o/r"),
        launched_on_the_host: "ssh://git@github.com/o/r.git",
        launched_in_a_container: "https://github.com/o/r.git",
    },
    Spelling {
        url: "ssh://git@github.com:22/o/r.git",
        on_this_machine: false,
        https: "https://github.com/o/r.git",
        key: Some("github.com/o/r"),
        launched_on_the_host: "ssh://git@github.com:22/o/r.git",
        launched_in_a_container: "https://github.com/o/r.git",
    },
    Spelling {
        url: "ssh://github.com/o/r",
        on_this_machine: false,
        https: "https://github.com/o/r",
        key: Some("github.com/o/r"),
        launched_on_the_host: "ssh://github.com/o/r",
        launched_in_a_container: "https://github.com/o/r",
    },
    Spelling {
        url: "ssh://github.com",
        on_this_machine: false,
        https: "ssh://github.com",
        key: None,
        launched_on_the_host: "ssh://github.com",
        launched_in_a_container: "ssh://github.com",
    },
    Spelling {
        url: "git@github.com:o/r.git",
        on_this_machine: false,
        https: "https://github.com/o/r.git",
        key: Some("github.com/o/r"),
        launched_on_the_host: "git@github.com:o/r.git",
        launched_in_a_container: "https://github.com/o/r.git",
    },
    Spelling {
        url: "github.com:o/r",
        on_this_machine: false,
        https: "https://github.com/o/r",
        key: Some("github.com/o/r"),
        launched_on_the_host: "github.com:o/r",
        launched_in_a_container: "https://github.com/o/r",
    },
    Spelling {
        url: "git@git.example.com:/srv/r.git",
        on_this_machine: false,
        https: "https://git.example.com/srv/r.git",
        key: Some("git.example.com/srv/r"),
        launched_on_the_host: "git@git.example.com:/srv/r.git",
        launched_in_a_container: "https://git.example.com/srv/r.git",
    },
    Spelling {
        url: "ssh://git@[::1]/o/r.git",
        on_this_machine: false,
        https: "https://[::1]/o/r.git",
        key: None,
        launched_on_the_host: "ssh://git@[::1]/o/r.git",
        launched_in_a_container: "https://[::1]/o/r.git",
    },
    Spelling {
        url: "ssh://git@[::1]:22/o/r.git",
        on_this_machine: false,
        https: "https://[::1]/o/r.git",
        key: None,
        launched_on_the_host: "ssh://git@[::1]:22/o/r.git",
        launched_in_a_container: "https://[::1]/o/r.git",
    },
    Spelling {
        url: "git@[::1]:o/r.git",
        on_this_machine: false,
        https: "https://[::1]/o/r.git",
        key: None,
        launched_on_the_host: "git@[::1]:o/r.git",
        launched_in_a_container: "https://[::1]/o/r.git",
    },
    Spelling {
        url: "https://[::1]:8443/o/r.git",
        on_this_machine: false,
        https: "https://[::1]:8443/o/r.git",
        key: None,
        launched_on_the_host: "https://[::1]:8443/o/r.git",
        launched_in_a_container: "https://[::1]:8443/o/r.git",
    },
    Spelling {
        url: "../origin",
        on_this_machine: true,
        https: "../origin",
        key: None,
        launched_on_the_host: "../origin",
        launched_in_a_container: "../origin",
    },
    Spelling {
        url: "origin.git",
        on_this_machine: true,
        https: "origin.git",
        key: None,
        launched_on_the_host: "origin.git",
        launched_in_a_container: "origin.git",
    },
    Spelling {
        url: "https://github.com/o/../r",
        on_this_machine: false,
        https: "https://github.com/o/../r",
        key: None,
        launched_on_the_host: "https://github.com/o/../r",
        launched_in_a_container: "https://github.com/o/../r",
    },
    Spelling {
        url: "https://github.com/o/r r",
        on_this_machine: false,
        https: "https://github.com/o/r r",
        key: None,
        launched_on_the_host: "https://github.com/o/r r",
        launched_in_a_container: "https://github.com/o/r r",
    },
    Spelling {
        url: "",
        on_this_machine: true,
        https: "",
        key: None,
        launched_on_the_host: "",
        launched_in_a_container: "",
    },
];

fn on_this_machine(url: &str) -> bool {
    Remote::from_url(url).is_path_on_this_machine()
}

fn https(url: &str) -> String {
    Remote::from_url(url).https_equivalent()
}

fn key(url: &str) -> Option<String> {
    Remote::from_url(url)
        .repo_key()
        .ok()
        .map(|key| key.to_string())
}

/// The `--repo` a round of a job first requested with `url` is launched with.
fn launched<R: Runner>(url: &str) -> String {
    let request = RoundRequest {
        remote_url: url.into(),
        base: PinnedRef {
            name: "main".into(),
            sha: "a".repeat(40).parse().unwrap(),
        },
        prompt: "x".into(),
        provider: "p".into(),
        requested_at: Utc::now(),
    };
    let settings: JobSettings =
        RepoConfig::parse("provider = \"p\"\n[providers.p]\ncmd = \"true\"\n")
            .unwrap()
            .settings_for(None)
            .unwrap();
    let spec = LaunchSpec::for_round::<R>(
        &JobPaths {
            id: 1.into(),
            dir: PathBuf::from("/root/jobs/1"),
        },
        1,
        &JobRequests {
            first: request.clone(),
            latest: request.clone(),
        },
        &request.base,
        &settings,
    );
    spec.args
        .iter()
        .find_map(|arg| arg.strip_prefix("--repo="))
        .expect("every launch names its remote")
        .to_string()
}

#[test]
fn every_spelling_is_on_this_machine_or_not_as_the_table_says() {
    for s in SPELLINGS {
        assert_eq!(on_this_machine(s.url), s.on_this_machine, "{:?}", s.url);
    }
}

#[test]
fn every_spelling_has_the_https_form_the_table_says() {
    for s in SPELLINGS {
        assert_eq!(https(s.url), s.https, "{:?}", s.url);
    }
}

#[test]
fn every_spelling_has_the_key_the_table_says() {
    for s in SPELLINGS {
        assert_eq!(key(s.url).as_deref(), s.key, "{:?}", s.url);
    }
}

#[test]
fn every_spelling_is_launched_on_the_host_as_the_table_says() {
    for s in SPELLINGS {
        assert_eq!(
            launched::<LocalRunner>(s.url),
            s.launched_on_the_host,
            "{:?}",
            s.url
        );
    }
}

#[test]
fn every_spelling_is_launched_in_a_container_as_the_table_says() {
    for s in SPELLINGS {
        assert_eq!(
            launched::<DockerRunner>(s.url),
            s.launched_in_a_container,
            "{:?}",
            s.url
        );
        assert_eq!(
            launched::<KubernetesRunner>(s.url),
            s.launched_in_a_container,
            "{:?}",
            s.url
        );
    }
}

#[test]
fn a_remote_that_names_no_directory_is_refused_by_name() {
    for s in SPELLINGS.iter().filter(|s| s.key.is_none()) {
        let err = Remote::from_url(s.url).repo_key().unwrap_err();
        assert!(err.to_string().contains(s.url), "{:?}: {err}", s.url);
    }
}

/// The default port of a remote's scheme is no part of its key; any other
/// port is.
#[test]
fn a_remote_on_another_port_is_another_repository() {
    assert_eq!(key("https://h:443/o/r"), key("https://h/o/r"));
    assert_eq!(key("ssh://h:22/o/r"), key("git@h:o/r"));
    assert_ne!(key("https://h:8443/o/r"), key("https://h/o/r"));
    assert_ne!(key("ssh://h:2222/o/r"), key("ssh://h/o/r"));
}

#[test]
fn a_key_with_a_port_is_found_again_from_its_directory_name() {
    for (url, dir_name) in [
        ("https://h:8443/o/r", "h+8443~o~r"),
        ("ssh://h:2222/o/r", "h+2222~o~r"),
    ] {
        let key = Remote::from_url(url).repo_key().unwrap();
        assert_eq!(key.dir_name(), dir_name, "{url}");
        assert_eq!(RepoKey::from_dir_name(dir_name), Some(key), "{url}");
    }
}

#[test]
fn a_directory_name_with_no_port_after_its_plus_is_no_key() {
    for name in ["h+~o~r", "h+x~o~r", "+8443~o~r", "h+84+43~o~r"] {
        assert_eq!(RepoKey::from_dir_name(name), None, "{name}");
    }
}

/// Git reads a relative remote path from the repository; a job's clone runs
/// from a scratch directory, where the same path names nothing.
#[tokio::test]
async fn a_checkouts_relative_remote_path_is_resolved_against_the_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;
    git::run_allowing_failure(&repo, &["remote", "add", "origin", "../origin.git"])
        .await
        .unwrap();

    assert_eq!(
        Remote::of_checkout(&repo).await.unwrap().url(),
        repo.join("../origin.git").to_string_lossy()
    );
}

#[tokio::test]
async fn a_checkouts_remote_that_is_absolute_or_names_a_host_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;

    for url in [
        "/srv/git/origin.git",
        "file:///srv/git/origin.git",
        "git@example.com:team/repo.git",
        "https://example.com/team/repo.git",
    ] {
        git::run_allowing_failure(&repo, &["remote", "add", "origin", url])
            .await
            .unwrap();
        assert_eq!(Remote::of_checkout(&repo).await.unwrap().url(), url);
        git::run_allowing_failure(&repo, &["remote", "remove", "origin"])
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_checkout_with_no_origin_has_nothing_to_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    support::init_git_repo(&repo).await;

    let err = Remote::of_checkout(&repo).await.unwrap_err().to_string();
    assert!(err.contains("no 'origin' remote"), "{err}");
}
