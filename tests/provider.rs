use assembly_line::config::Provider;
use assembly_line::provider::render_command;

fn provider(cmd: &str, args: &[&str]) -> Provider {
    Provider {
        cmd: cmd.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        adapter: None,
    }
}

#[test]
fn substitutes_the_prompt_into_the_argument_that_contains_it() {
    let spec = render_command(
        &provider(
            "claude",
            &["-p", "{prompt}", "--permission-mode", "acceptEdits"],
        ),
        "build the thing",
    );

    assert_eq!(spec.program, "claude");
    assert_eq!(
        spec.args,
        vec!["-p", "build the thing", "--permission-mode", "acceptEdits"]
    );
}

#[test]
fn a_prompt_with_shell_metacharacters_stays_one_argument() {
    let nasty = "fix `rm -rf /`; and \"quote\" $HOME\nsecond line";
    let spec = render_command(&provider("agent", &["--message", "{prompt}"]), nasty);

    assert_eq!(spec.args.len(), 2);
    assert_eq!(spec.args[1], nasty);
}

#[test]
fn the_placeholder_is_replaced_wherever_it_appears_in_an_argument() {
    let spec = render_command(&provider("agent", &["--task=({prompt})"]), "hi");
    assert_eq!(spec.args, vec!["--task=(hi)"]);
}

#[test]
fn every_occurrence_is_replaced() {
    let spec = render_command(&provider("agent", &["{prompt}", "{prompt}"]), "x");
    assert_eq!(spec.args, vec!["x", "x"]);
}

#[test]
fn a_provider_without_the_placeholder_is_rendered_unchanged() {
    let spec = render_command(&provider("agent", &["--stdin"]), "ignored");
    assert_eq!(spec.args, vec!["--stdin"]);
}
