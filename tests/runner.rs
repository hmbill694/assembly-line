use assembly_line::payload::{GIT_TOKEN_VAR, https_equivalent};
use assembly_line::runner::docker::docker_run_args;
use assembly_line::runner::{JobSecrets, RunnerProblem, reasons_a_container_cannot_run};

#[test]
fn ssh_remotes_become_the_https_url_a_token_can_authenticate() {
    assert_eq!(
        https_equivalent("git@github.com:o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        https_equivalent("ssh://git@github.com/o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        https_equivalent("ssh://git@github.com:22/o/r.git"),
        "https://github.com/o/r.git"
    );
    assert_eq!(
        https_equivalent("git@git.example.com:/srv/r.git"),
        "https://git.example.com/srv/r.git"
    );
}

/// An IPv6 literal's own colons are not the port's, nor scp-like's.
#[test]
fn an_ipv6_host_keeps_its_brackets() {
    assert_eq!(
        https_equivalent("ssh://git@[::1]/o/r.git"),
        "https://[::1]/o/r.git"
    );
    assert_eq!(
        https_equivalent("ssh://git@[::1]:22/o/r.git"),
        "https://[::1]/o/r.git"
    );
    assert_eq!(
        https_equivalent("git@[::1]:o/r.git"),
        "https://[::1]/o/r.git"
    );
}

#[test]
fn urls_a_token_already_works_with_are_left_alone() {
    for url in [
        "https://github.com/o/r.git",
        "/tmp/origin.git",
        "file:///tmp/origin.git",
        "../origin",
    ] {
        assert_eq!(https_equivalent(url), url);
    }
}

#[test]
fn a_container_always_receives_the_git_token_and_only_the_named_extras() {
    let host = |name: &str| match name {
        "ASSEMBLY_GIT_TOKEN" => Some("t0ken".to_string()),
        "ANTHROPIC_API_KEY" => Some("sk".to_string()),
        "AWS_SECRET_ACCESS_KEY" => Some("never".to_string()),
        _ => None,
    };

    let (secrets, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], host);

    assert!(problems.is_empty(), "{problems:?}");
    // Sorted: `names` is a set.
    assert_eq!(
        secrets.names().into_iter().collect::<Vec<_>>(),
        ["ANTHROPIC_API_KEY", GIT_TOKEN_VAR]
    );
}

#[test]
fn every_missing_variable_is_reported_at_once() {
    let (_, problems) = JobSecrets::from_lookup(&["ANTHROPIC_API_KEY".into()], |_| None);
    assert_eq!(
        problems,
        [
            RunnerProblem::MissingEnvironment(GIT_TOKEN_VAR.into()),
            RunnerProblem::MissingEnvironment("ANTHROPIC_API_KEY".into()),
        ]
    );
}

#[test]
fn a_repository_that_declares_copy_cannot_run_in_a_container() {
    assert_eq!(
        reasons_a_container_cannot_run(&[".env".into()]),
        [RunnerProblem::CopyNeedsLocalRunner]
    );
    assert!(reasons_a_container_cannot_run(&[]).is_empty());
}

#[test]
fn every_runner_problem_says_what_to_do_about_it() {
    let problems = [
        RunnerProblem::Unreachable {
            runner: "docker",
            detail: "no daemon".into(),
        },
        RunnerProblem::CopyNeedsLocalRunner,
        RunnerProblem::MissingEnvironment("X".into()),
    ];
    for problem in problems {
        let message = problem.to_string();
        assert!(message.contains(" — "), "no remedy in: {message}");
    }
}

#[test]
fn docker_run_names_its_secrets_and_runs_job_exec() {
    let args = docker_run_args(
        "img:1",
        "al-1-1-abc",
        &["ASSEMBLY_JOB", "ASSEMBLY_GIT_TOKEN"],
    );
    let joined = args.join(" ");

    assert!(joined.contains("-e ASSEMBLY_JOB"), "{joined}");
    assert!(joined.contains("-e ASSEMBLY_GIT_TOKEN"), "{joined}");
    assert!(joined.ends_with("img:1 assembly job-exec"), "{joined}");
}
