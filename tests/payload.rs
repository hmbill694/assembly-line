use assembly_line::config::RepoConfig;
use assembly_line::git::PinnedRef;
use assembly_line::payload::{RoundPayload, RoundRequest, revised_prompt};
use std::path::Path;

fn request(provider: &str) -> RoundRequest<'_> {
    RoundRequest {
        job_id: 7,
        round: 1,
        prompt: "add a README\n\nwith sections",
        provider,
        start: PinnedRef {
            name: "main".into(),
            sha: "abc123".into(),
        },
        remote_name: "origin",
        remote_url: "https://example.com/o/r.git".into(),
        seed_from: Path::new("/repo"),
    }
}

fn config(body: &str) -> RepoConfig {
    RepoConfig::parse(body).unwrap()
}

const RUNNABLE: &str = "provider = \"fake\"\nverify = \"cargo test\"\nmax_duration = \"20m\"\ncopy = [\".env\"]\n\
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
    assert_eq!(payload.commit_message, "job 7: add a README");
    assert_eq!(payload.verify.as_deref(), Some("cargo test"));
    assert_eq!(payload.command_limit_secs, Some(20 * 60));
    assert_eq!(payload.copy, [".env"]);
    assert_eq!(payload.start.sha, "abc123");
    assert!(
        !payload.provision_toolchain,
        "the host opts a runner in, never the default"
    );
}

#[test]
fn a_payload_round_trips_through_json() {
    let payload = RoundPayload::for_round(&config(RUNNABLE), request("fake")).unwrap();
    let json = serde_json::to_string(&payload).unwrap();
    assert_eq!(
        serde_json::from_str::<RoundPayload>(&json).unwrap(),
        payload
    );
}

#[test]
fn job_exec_reads_the_payload_its_runner_passed() {
    let payload = RoundPayload::for_round(&config(RUNNABLE), request("fake")).unwrap();
    let json = serde_json::to_string(&payload).unwrap();
    assert_eq!(RoundPayload::from_variable(Some(&json)).unwrap(), payload);
}

#[test]
fn job_exec_without_a_payload_says_a_runner_starts_it() {
    let err = RoundPayload::from_variable(None).unwrap_err().to_string();
    assert!(err.contains("ASSEMBLY_JOB is not set"), "{err}");
    assert!(err.contains("started by a runner"), "{err}");
}

#[test]
fn a_payload_variable_holding_something_else_is_refused() {
    let err = RoundPayload::from_variable(Some("{\"job_id\": 1}"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("ASSEMBLY_JOB is not a job payload"), "{err}");
}

#[test]
fn an_undeclared_provider_cannot_become_a_payload() {
    let err = RoundPayload::for_round(&config(RUNNABLE), request("other")).unwrap_err();
    assert!(err.to_string().contains("'other'"), "{err}");
}

#[test]
fn a_revised_prompt_carries_the_original_and_the_feedback() {
    let prompt = revised_prompt("add auth", "use sessions");
    assert!(prompt.starts_with("add auth"));
    assert!(prompt.contains("use sessions"));
}
