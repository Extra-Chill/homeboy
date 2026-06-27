use super::super::*;
use super::*;

#[test]
fn parse_stale_days_accepts_plain_or_d_suffix() {
    assert_eq!(parse_stale_days("14").unwrap(), 14);
    assert_eq!(parse_stale_days("14d").unwrap(), 14);
    assert!(parse_stale_days("0d").is_err());
    assert!(parse_stale_days("two-weeks").is_err());
}

#[test]
fn dedupe_refs_by_repo_merges_sources_and_usage() {
    let mut project_ref = ComponentRef::new(
        "intelligence".to_string(),
        "/tmp/intelligence".to_string(),
        Some("https://github.com/example-org/intelligence.git".to_string()),
        None,
        "project:intelligence-example-org".to_string(),
    );
    project_ref
        .usage
        .insert("intelligence-example-org".to_string());

    let mut rig_ref = ComponentRef::new(
        "intelligence-dev".to_string(),
        "/tmp/intelligence-dev".to_string(),
        Some("git@github.com:example-org/intelligence.git".to_string()),
        None,
        "rig:intelligence-example-org".to_string(),
    );
    rig_ref.usage.insert("intelligence-example-org".to_string());

    let component_ref = ComponentRef::new(
        "standalone".to_string(),
        "/tmp/standalone".to_string(),
        Some("https://github.com/Extra-Chill/standalone.git".to_string()),
        None,
        "component:standalone".to_string(),
    );

    let refs = dedupe_refs_by_repo(vec![project_ref, rig_ref, component_ref]);

    assert_eq!(refs.len(), 2);
    let intelligence = refs
        .iter()
        .find(|component_ref| component_ref.component_id == "intelligence")
        .expect("first ref for the repo should be retained");
    assert_eq!(
        intelligence.sources.iter().cloned().collect::<Vec<_>>(),
        vec![
            "project:intelligence-example-org".to_string(),
            "rig:intelligence-example-org".to_string(),
        ]
    );
    assert_eq!(
        intelligence.usage.iter().cloned().collect::<Vec<_>>(),
        vec!["intelligence-example-org".to_string()]
    );
}

#[test]
fn dedupe_refs_by_repo_keeps_unresolved_entries_separate() {
    let resolved = ComponentRef::new(
        "sample-plugin".to_string(),
        "/tmp/sample-plugin".to_string(),
        Some("https://github.com/Extra-Chill/sample-plugin.git".to_string()),
        None,
        "component:sample-plugin".to_string(),
    );
    let unresolved = ComponentRef::new(
        "local-only".to_string(),
        "".to_string(),
        None,
        None,
        "component:local-only".to_string(),
    );

    let refs = dedupe_refs_by_repo(vec![unresolved, resolved]);

    assert_eq!(refs.len(), 2);
    assert!(refs.iter().any(|r| r.component_id == "sample-plugin"));
    assert!(refs.iter().any(|r| r.component_id == "local-only"));
}

#[test]
fn unresolved_summary_is_visible_when_targets_fail_to_resolve() {
    let unresolved = vec![TriageUnresolved {
        component_id: "missing".to_string(),
        local_path: "/tmp/missing".to_string(),
        reason: "local path does not exist".to_string(),
        sources: vec!["workspace".to_string()],
    }];

    assert_eq!(
        summarize_unresolved(&unresolved).as_deref(),
        Some(
            "1 unresolved component target(s): missing (/tmp/missing) - local path does not exist;"
        )
    );
}

#[test]
fn parse_issues_marks_stale_and_extracts_labels() {
    let raw = r#"[
            {
              "number": 7,
              "title": "Fix auth",
              "url": "https://github.com/o/r/issues/7",
              "state": "OPEN",
              "labels": [{"name":"P1"}],
              "assignees": [{"login":"example-org"}],
              "updatedAt": "2026-01-01T00:00:00Z"
            }
        ]"#;
    let cutoff = Some(
        DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    );
    let items = parse_issues(raw, cutoff).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].labels, vec!["P1"]);
    assert_eq!(items[0].assignees, vec!["example-org"]);
    assert!(items[0].stale);
    assert!(items[0].linked_prs.is_empty());
}

#[test]
fn parse_issue_accepts_single_issue_view_payload() {
    let raw = r#"{
          "number": 8,
          "title": "Closed bug",
          "url": "https://github.com/o/r/issues/8",
              "state": "CLOSED",
              "labels": [],
              "assignees": [],
              "comments": [
                {"createdAt":"2026-04-02T00:00:00Z","updatedAt":"2026-04-03T00:00:00Z"},
                {"createdAt":"2026-04-04T00:00:00Z","updatedAt":null}
              ],
              "updatedAt": "2026-04-01T00:00:00Z"
        }"#;

    let item = parse_issue(raw, None).unwrap();

    assert_eq!(item.number, 8);
    assert_eq!(item.state, "CLOSED");
    assert_eq!(item.comments_count, Some(2));
    assert_eq!(
        item.last_comment_at.as_deref(),
        Some("2026-04-04T00:00:00Z")
    );
    assert!(item.linked_prs.is_empty());
}

#[test]
fn issue_bucket_counts_only_open_targeted_issues() {
    let bucket = issue_bucket(vec![
        TriageIssueItem {
            number: 1,
            title: "Open".to_string(),
            url: "https://github.com/o/r/issues/1".to_string(),
            state: "OPEN".to_string(),
            labels: vec![],
            assignees: vec![],
            updated_at: None,
            comments_count: None,
            last_comment_at: None,
            stale: false,
            linked_prs: Vec::new(),
        },
        TriageIssueItem {
            number: 2,
            title: "Closed".to_string(),
            url: "https://github.com/o/r/issues/2".to_string(),
            state: "CLOSED".to_string(),
            labels: vec![],
            assignees: vec![],
            updated_at: None,
            comments_count: None,
            last_comment_at: None,
            stale: false,
            linked_prs: Vec::new(),
        },
    ]);

    assert_eq!(bucket.open, 1);
    assert_eq!(bucket.items.len(), 2);
}

#[test]
fn issue_actions_ignore_closed_targeted_issues() {
    let issues = TriageIssueBucket {
        open: 0,
        items: vec![TriageIssueItem {
            number: 1,
            title: "Closed".to_string(),
            url: "https://github.com/o/r/issues/1".to_string(),
            state: "CLOSED".to_string(),
            labels: vec!["P1".to_string()],
            assignees: vec![],
            updated_at: None,
            comments_count: None,
            last_comment_at: None,
            stale: true,
            linked_prs: Vec::new(),
        }],
    };

    let actions = build_actions(Some(&issues), None, &default_priority_labels_vec());

    assert!(actions.is_empty());
}

#[test]
fn parse_linked_prs_extracts_merge_timestamp() {
    let raw = r#"[
            {
              "number": 12,
              "title": "Fix auth",
              "url": "https://github.com/o/r/pull/12",
              "state": "MERGED",
              "mergedAt": "2026-04-03T00:00:00Z"
            },
            {
              "number": 13,
              "title": "Follow-up",
              "url": "https://github.com/o/r/pull/13",
              "state": "OPEN",
              "mergedAt": null
            }
        ]"#;

    let items = parse_linked_prs(raw).unwrap();

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].number, 12);
    assert_eq!(items[0].merged_at.as_deref(), Some("2026-04-03T00:00:00Z"));
    assert!(items[1].merged_at.is_none());
}

#[test]
fn parse_issue_numbers_allows_hash_prefix_and_comments() {
    let parsed = parse_issue_numbers("# first comment\n1531\n#1538\n\n1501\n").unwrap();

    assert_eq!(parsed, vec![1531, 1538, 1501]);
    assert!(parse_issue_numbers("1531\nabc\n").is_err());
}

#[test]
fn parse_pr_target_accepts_url_or_number_with_repo() {
    let from_url =
        parse_pr_target("https://github.com/Extra-Chill/homeboy/pull/5808", None).unwrap();
    assert_eq!(from_url.repo.owner, "Extra-Chill");
    assert_eq!(from_url.repo.repo, "homeboy");
    assert_eq!(from_url.number, 5808);

    let from_number = parse_pr_target("5808", Some("Extra-Chill/homeboy")).unwrap();
    assert_eq!(from_number.repo.owner, "Extra-Chill");
    assert_eq!(from_number.repo.repo, "homeboy");
    assert_eq!(from_number.number, 5808);

    assert!(parse_pr_target("5808", None).is_err());
}

#[test]
fn ci_failure_helpers_classify_and_extract_concise_snippets() {
    let log = "running cargo test\nthread 'core' panicked\nassertion failed\ntest result: FAILED\nnext line";

    let snippets = extract_failure_snippets(log, 3);
    assert_eq!(snippets.len(), 2);
    assert!(snippets[0].text.contains("panicked"));
    assert_eq!(
        classify_failure(&["unit", snippets[0].text.as_str()]),
        "unit-test"
    );

    assert_eq!(
        classify_failure(&["cargo fmt --check", "Diff in src/main.rs"]),
        "fmt"
    );
    assert_eq!(
        detect_baseline_vs_head(&["baseline red but head green"]).as_deref(),
        Some("baseline-vs-head")
    );
    assert_eq!(
        extract_actions_job_id("https://github.com/o/r/actions/runs/10/job/20"),
        Some(20)
    );
}
