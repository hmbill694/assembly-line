use assembly_line::config::{JobSettings, RepoConfig};
use assembly_line::git::PinnedRef;
use assembly_line::job::JobId;
use assembly_line::payload::{RoundBeforeSettings, RoundPayload, commit_message};

const START_SHA: &str = "abc1230000000000000000000000000000000000";

fn round() -> RoundBeforeSettings<'static> {
    RoundBeforeSettings {
        job_id: 7.into(),
        prompt: "add a README\n\nwith sections",
        start: PinnedRef {
            name: "main".into(),
            sha: START_SHA.parse().unwrap(),
        },
        provision_toolchain: false,
    }
}

fn settings(body: &str) -> JobSettings {
    RepoConfig::parse(body).unwrap().settings_for(None).unwrap()
}

const RUNNABLE: &str = "provider = \"fake\"\nverify = \"cargo test\"\nmax_duration = \"20m\"\n\
[providers.fake]\ncmd = \"agent\"\nargs = [\"-p\", \"{prompt}\"]\n";

#[test]
fn a_payload_carries_everything_the_round_needs_already_resolved() {
    let payload = RoundPayload::for_round(&settings(RUNNABLE), round());

    assert_eq!(payload.job_id, JobId::from(7));
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
    assert_eq!(payload.start.sha.as_str(), START_SHA);
}

#[test]
fn the_toolchain_flag_reaches_the_payload_as_given() {
    [true, false].into_iter().for_each(|provision_toolchain| {
        let payload = RoundPayload::for_round(
            &settings(RUNNABLE),
            RoundBeforeSettings {
                provision_toolchain,
                ..round()
            },
        );
        assert_eq!(payload.provision_toolchain, provision_toolchain);
    });
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
