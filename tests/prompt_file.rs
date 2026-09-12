use assembly_line::config::{ValidationError, load_graph, parse_graph, validate};
use std::fs;

/// A graph directory with a graph file and any prompt files beside it.
fn graph_dir(graph_body: &str, prompts: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("graph.toml"), graph_body).unwrap();
    prompts.iter().for_each(|(name, body)| {
        let path = tmp.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    });
    tmp
}

#[test]
fn a_prompt_file_is_read_into_the_prompt() {
    let tmp = graph_dir(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"impl-auth\"\nprovider=\"p\"\n\
         prompt_file=\"prompts/auth.md\"\nverify=\"true\"\n",
        &[("prompts/auth.md", "Implement JWT auth.\nUse argon2.\n")],
    );

    let graph = load_graph(&tmp.path().join("graph.toml")).unwrap();
    assert_eq!(
        graph.tasks[0].prompt.as_deref(),
        Some("Implement JWT auth.\nUse argon2.\n")
    );
}

#[test]
fn prompt_file_paths_resolve_relative_to_the_graph_not_the_cwd() {
    let tmp = graph_dir(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprovider=\"p\"\n\
         prompt_file=\"p.md\"\nverify=\"true\"\n",
        &[("p.md", "from the graph's directory")],
    );

    // Load by absolute path while the process cwd is somewhere else entirely.
    let graph = load_graph(&tmp.path().join("graph.toml")).unwrap();
    assert_eq!(
        graph.tasks[0].prompt.as_deref(),
        Some("from the graph's directory")
    );
}

#[test]
fn setting_both_prompt_and_prompt_file_is_an_error() {
    let tmp = graph_dir(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"a\"\nprovider=\"p\"\n\
         prompt=\"inline\"\nprompt_file=\"p.md\"\nverify=\"true\"\n",
        &[("p.md", "from a file")],
    );

    let err = load_graph(&tmp.path().join("graph.toml")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("both"), "{message}");
    assert!(message.contains("'a'"), "{message}");
}

#[test]
fn a_missing_prompt_file_names_the_task_and_the_path() {
    let tmp = graph_dir(
        "[providers.p]\ncmd=\"true\"\n\
         [[task]]\nid=\"impl-auth\"\nprovider=\"p\"\n\
         prompt_file=\"prompts/gone.md\"\nverify=\"true\"\n",
        &[],
    );

    let message = load_graph(&tmp.path().join("graph.toml"))
        .unwrap_err()
        .to_string();
    assert!(message.contains("impl-auth"), "{message}");
    assert!(message.contains("gone.md"), "{message}");
}

#[test]
fn an_agent_with_only_a_prompt_file_passes_validation() {
    let src = "[providers.p]\ncmd=\"true\"\n\
               [[task]]\nid=\"a\"\nprovider=\"p\"\n\
               prompt_file=\"p.md\"\nverify=\"true\"\n";
    let graph = parse_graph(src).unwrap();
    assert!(validate(&graph).errors.is_empty());
}

#[test]
fn an_agent_with_neither_prompt_nor_prompt_file_is_rejected() {
    let src = "[[task]]\nid=\"a\"\n";
    let graph = parse_graph(src).unwrap();
    let errs = validate(&graph).errors;
    assert!(
        errs.contains(&ValidationError::AgentMissingPrompt("a".into())),
        "{errs:?}"
    );
}

#[test]
fn a_task_with_no_prompt_at_all_is_left_alone_by_load_graph() {
    let tmp = graph_dir("[[task]]\nid=\"a\"\n", &[]);
    let graph = load_graph(&tmp.path().join("graph.toml")).unwrap();
    assert!(graph.tasks[0].prompt.is_none());
}
