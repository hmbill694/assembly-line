use assembly_line::config::RepoConfig;
use assembly_line::git::PinnedRef;
use assembly_line::job::JobId;
use assembly_line::payload::{RoundPayload, RoundRequest, commit_message};

fn request(provider: &str) -> RoundRequest<'_> {
    RoundRequest {
        job_id: 7.into(),
        prompt: "add a README\n\nwith sections",
        provider,
        start: PinnedRef {
            name: "main".into(),
            sha: "abc123".into(),
        },
        remote_name: "origin",
        remote_url: "https://example.com/o/r.git".into(),
    }
}

fn config(body: &str) -> RepoConfig {
    RepoConfig::parse(body).unwrap()
}

const RUNNABLE: &str = "provider = \"fake\"\nverify = \"cargo test\"\nmax_duration = \"20m\"\n\
[providers.fake]\ncmd = \"agent\"\nargs = [\"-p\", \"{prompt}\"]\n";

#[test]
fn a_payload_carries_everything_the_round_needs_already_resolved() {
    let payload = RoundPayload::for_round(&config(RUNNABLE), request("fake")).unwrap();

    assert_eq!(payload.branch, "al/job-7");
    assert_eq!(payload.command.program, "agent");
    assert_eq!(
        payload.command.args,
        ["-p", "add a README\n\nwith sections"]
    );
    assert_eq!(
        payload.commit_message,
        "job 7: add a README\n\nadd a README\n\nwith sections"
    );
    assert_eq!(payload.verify.as_deref(), Some("cargo test"));
    assert_eq!(payload.command_limit_secs, Some(20 * 60));
    assert_eq!(payload.start.sha, "abc123");
    assert!(
        !payload.provision_toolchain,
        "the host opts a runner in, never the default"
    );
}

#[test]
fn a_commit_message_is_the_first_line_then_the_whole_prompt() {
    let prompt = "Add auth\n\nUse sessions, not JWTs.\n";

    assert_eq!(
        commit_message(JobId::from(7), prompt),
        "job 7: Add auth\n\nAdd auth\n\nUse sessions, not JWTs."
    );
}

#[test]
fn a_blank_prompt_still_makes_a_commit_message() {
    assert_eq!(commit_message(JobId::from(7), "  \n"), "job 7: agent work");
}

#[test]
fn an_undeclared_provider_cannot_become_a_payload() {
    let err = RoundPayload::for_round(&config(RUNNABLE), request("other")).unwrap_err();
    assert!(err.to_string().contains("'other'"), "{err}");
}
