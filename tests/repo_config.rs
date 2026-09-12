//! Reading a repository's declaration of how the factory builds it — from a
//! ref, never from a checkout.

use assembly_line::config::RepoConfig;

mod support;

#[tokio::test]
async fn a_repo_declares_how_the_factory_builds_it() {
    let repo = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(repo.path().join(".assembly")).unwrap();
    std::fs::write(
        repo.path().join(".assembly/config.toml"),
        r#"
provider = "claude"
verify = "cargo test"
base = "main"
copy = [".env"]

[providers.claude]
cmd = "claude"
args = ["-p", "{prompt}"]
"#,
    )
    .unwrap();
    assembly_line::git::commit_all(repo.path(), "add config")
        .await
        .unwrap();

    let config = RepoConfig::from_ref(repo.path(), "HEAD").await.unwrap();

    assert_eq!(config.provider.as_deref(), Some("claude"));
    assert_eq!(config.verify.as_deref(), Some("cargo test"));
    assert_eq!(config.base.as_deref(), Some("main"));
    assert_eq!(config.copy, vec![".env".to_string()]);
    assert!(config.providers.contains_key("claude"));
}

#[tokio::test]
async fn a_repo_with_no_config_is_not_opted_in() {
    let repo = support::repo_with_initial_commit().await;

    let err = RepoConfig::from_ref(repo.path(), "HEAD").await.unwrap_err();

    assert!(
        err.to_string().contains(".assembly/config.toml"),
        "the error should name the file to create, got: {err}"
    );
}

/// The reason configuration is read from a ref at all: an agent's branch can
/// say anything, and the settings that govern the job must not be its to
/// rewrite.
#[tokio::test]
async fn the_working_tree_cannot_change_the_settings_that_govern_a_job() {
    let repo = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(repo.path().join(".assembly")).unwrap();
    std::fs::write(
        repo.path().join(".assembly/config.toml"),
        "provider = \"declared\"\n[providers.declared]\ncmd = \"true\"\n",
    )
    .unwrap();
    assembly_line::git::commit_all(repo.path(), "add config")
        .await
        .unwrap();

    // An uncommitted edit — what an agent's checkout would look like.
    std::fs::write(
        repo.path().join(".assembly/config.toml"),
        "provider = \"smuggled\"\n[providers.smuggled]\ncmd = \"curl\"\n",
    )
    .unwrap();

    let config = RepoConfig::from_ref(repo.path(), "HEAD").await.unwrap();
    assert_eq!(config.provider.as_deref(), Some("declared"));
    assert!(!config.providers.contains_key("smuggled"));
}

#[tokio::test]
async fn a_malformed_config_names_the_file_it_could_not_parse() {
    let repo = support::repo_with_initial_commit().await;
    std::fs::create_dir_all(repo.path().join(".assembly")).unwrap();
    std::fs::write(repo.path().join(".assembly/config.toml"), "provider = [\n").unwrap();
    assembly_line::git::commit_all(repo.path(), "add config")
        .await
        .unwrap();

    let err = RepoConfig::from_ref(repo.path(), "HEAD")
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains(".assembly/config.toml"), "{err}");
}
