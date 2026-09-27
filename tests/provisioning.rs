use assembly_line::event::EventKind;
use assembly_line::frame::{Routed, StreamPosition};
use assembly_line::payload::PAYLOAD_VAR;
use assert_cmd::Command;
use support::{Harness, fake_cli};

mod support;

/// Run `job-exec` on `payload` with `fakes` first on PATH; return the
/// routed frames.
fn job_exec(
    payload: &assembly_line::payload::RoundPayload,
    fakes: &std::path::Path,
    tmp: &std::path::Path,
) -> Vec<Routed> {
    let out = Command::cargo_bin("assembly")
        .unwrap()
        .arg("job-exec")
        .env(PAYLOAD_VAR, serde_json::to_string(payload).unwrap())
        .env(
            "PATH",
            format!("{}:{}", fakes.display(), std::env::var("PATH").unwrap()),
        )
        .env("TMPDIR", tmp)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| StreamPosition::default().route(line).1)
        .collect()
}

fn outputs(routed: &[Routed]) -> String {
    routed
        .iter()
        .filter_map(|r| match r {
            Routed::Output(t) => Some(format!("{t}\n")),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_container_round_installs_the_toolchain_before_the_agent_runs() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    fake_cli(&fakes, "mise", "echo \"mise $*\"\n");
    let payload = assembly_line::payload::RoundPayload {
        provision_toolchain: true,
        ..h.payload_for("x").await
    };

    let routed = job_exec(&payload, &fakes, &h.scratch_root());
    let printed = outputs(&routed);

    let trusted = printed.find("mise trust").expect(&printed);
    let installed = printed.find("mise install").expect(&printed);
    let agent = printed.find("fake-agent: x").expect(&printed);
    assert!(trusted < installed && installed < agent, "{printed}");
}

#[tokio::test]
async fn a_failed_install_fails_the_round_before_the_agent_runs() {
    let h = Harness::new().await;
    let fakes = h.scratch_root().with_file_name("fakes");
    fake_cli(&fakes, "mise", "case \"$1\" in install) exit 1 ;; esac\n");
    let payload = assembly_line::payload::RoundPayload {
        provision_toolchain: true,
        ..h.payload_for("x").await
    };

    let routed = job_exec(&payload, &fakes, &h.scratch_root());

    assert!(
        !outputs(&routed).contains("fake-agent"),
        "the agent ran on a toolchain that failed to install"
    );
    assert!(routed.iter().any(|r| matches!(r,
        Routed::Event { event, .. } if matches!(&event.kind, EventKind::RoundFailed { reason } if reason.contains("provisioning"))
    )));
}
