use assembly_line::job::JobId;

#[test]
fn a_branch_name_identifies_the_job_that_produced_it() {
    assert_eq!(JobId::from(42).branch_name(), "al/job-42");
    assert_ne!(
        JobId::from(42).branch_name(),
        JobId::from(43).branch_name(),
        "two jobs must not share a branch name"
    );
}

#[test]
fn a_jobs_branch_name_gives_back_its_id_and_no_other_branch_does() {
    assert_eq!(
        JobId::from_branch_name(&JobId::from(42).branch_name()),
        Some(JobId::from(42))
    );
    assert_eq!(JobId::from_branch_name("main"), None);
    assert_eq!(JobId::from_branch_name("al/job-"), None);
    assert_eq!(JobId::from_branch_name("al/job-7x"), None);
    assert_eq!(JobId::from_branch_name("al/job-007"), None);
    assert_eq!(JobId::from_branch_name("al/job-+7"), None);
}

#[test]
fn the_branch_pattern_matches_every_job_branch_name() {
    let prefix = JobId::BRANCH_PATTERN.strip_suffix('*').unwrap();
    assert!(!prefix.contains(['*', '?', '[']), "{prefix}");
    assert!(JobId::from(0).branch_name().starts_with(prefix));
    assert!(JobId::from(u64::MAX).branch_name().starts_with(prefix));
}

#[test]
fn a_job_id_is_a_bare_number_on_the_wire() {
    assert_eq!(serde_json::to_string(&JobId::from(42)).unwrap(), "42");
    assert_eq!(
        serde_json::from_str::<JobId>("42").unwrap(),
        JobId::from(42)
    );
}
