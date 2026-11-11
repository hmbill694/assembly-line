use assembly_line::event::read_events;
use assembly_line::git::{CommitId, PinnedRef};
use assembly_line::job::JobId;
use assembly_line::report::JobReport;

const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";

fn sha256() -> String {
    "89abcdef".repeat(8)
}

#[test]
fn a_full_sha1_or_sha256_is_a_commit_id() {
    assert_eq!(SHA1.parse::<CommitId>().unwrap().as_str(), SHA1);
    assert_eq!(sha256().parse::<CommitId>().unwrap().as_str(), sha256());
}

#[test]
fn a_sha_of_any_other_length_is_refused_with_what_to_do() {
    let err = "abc123".parse::<CommitId>().unwrap_err();
    assert_eq!(
        err.to_string(),
        "'abc123' is not a commit id — give the full 40- or 64-character hex sha, as `git \
         rev-parse` prints it"
    );
    assert!(format!("{SHA1}0").parse::<CommitId>().is_err());
    assert!("".parse::<CommitId>().is_err());
}

#[test]
fn a_sha_with_a_character_that_is_not_hex_is_refused() {
    let not_hex = format!("{}g", &SHA1[..39]);
    assert!(not_hex.parse::<CommitId>().is_err());
}

#[test]
fn a_commit_ids_short_form_is_its_first_twelve_characters() {
    assert_eq!(SHA1.parse::<CommitId>().unwrap().short(), "0123456789ab");
}

#[test]
fn a_pinned_ref_is_written_as_ref_at_sha_and_parses_back() {
    let pinned = PinnedRef {
        name: "main".into(),
        sha: SHA1.parse().unwrap(),
    };

    assert_eq!(pinned.to_string(), format!("main@{SHA1}"));
    assert_eq!(pinned.to_string().parse::<PinnedRef>().unwrap(), pinned);
}

#[test]
fn a_pinned_ref_whose_name_holds_an_at_sign_parses_back_whole() {
    let pinned = PinnedRef {
        name: "release@2026".into(),
        sha: sha256().parse().unwrap(),
    };

    assert_eq!(pinned.to_string().parse::<PinnedRef>().unwrap(), pinned);
}

#[test]
fn text_that_does_not_end_in_a_commit_id_is_not_a_pin() {
    for unpinned in [
        "main",
        "release@2026",
        "main@abc123",
        "main@{upstream}",
        &format!("@{SHA1}"),
    ] {
        assert!(
            unpinned.parse::<PinnedRef>().is_err(),
            "{unpinned} parsed as a pin"
        );
    }
    assert_eq!(
        format!("@{SHA1}")
            .parse::<PinnedRef>()
            .unwrap_err()
            .to_string(),
        format!(
            "'@{SHA1}' is not REF@SHA with a ref name and a full commit id — write it that way \
             to pin a ref"
        )
    );
}

/// Serde checks nothing, so a log written before commit ids were checked
/// still reads in full.
#[test]
fn an_event_whose_sha_is_not_a_full_commit_id_still_reads_and_folds() {
    let raw = concat!(
        r#"{"at":"2026-08-15T00:00:00Z","t":"round_requested","remote_url":"origin","#,
        r#""base":{"name":"main","sha":"abc"},"prompt":"p","provider":"fake"}"#,
        "\n",
    );

    let events = read_events(raw.as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    let report = JobReport::from_events(JobId::from(1), &events);
    let base = report.requests.expect("the request folded").first.base;
    assert_eq!(base.sha.as_str(), "abc");
    assert_eq!(base.sha.short(), "abc");
}
