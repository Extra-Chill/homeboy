use super::super::*;
use super::*;

#[test]
fn priority_actions_use_default_labels_when_unconfigured() {
    let component_ref = ComponentRef::new(
        "sample-plugin".to_string(),
        "/tmp/sample-plugin".to_string(),
        None,
        Some("https://github.com/Extra-Chill/sample-plugin.git".to_string()),
        "component:sample-plugin".to_string(),
    );
    let labels = resolve_priority_labels(&component_ref, None);
    let issues = issues_with_labels(vec![vec!["bug"], vec!["polish"]]);

    let actions = build_actions(Some(&issues), None, &labels);

    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].kind, "priority_issues");
    assert_eq!(actions[0].label, "1 priority issue");
}

#[test]
fn component_priority_labels_override_global_labels() {
    let component_ref = ComponentRef::new(
        "sample-plugin".to_string(),
        "/tmp/sample-plugin".to_string(),
        None,
        Some("https://github.com/Extra-Chill/sample-plugin.git".to_string()),
        "component:sample-plugin".to_string(),
    )
    .with_priority_labels(Some(vec!["urgent".to_string()]));
    let global = vec!["bug".to_string()];
    let labels = resolve_priority_labels(&component_ref, Some(&global));
    let issues = issues_with_labels(vec![vec!["bug"], vec!["urgent"]]);

    let actions = build_actions(Some(&issues), None, &labels);

    assert_eq!(labels, vec!["urgent".to_string()]);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].label, "1 priority issue");
}

#[test]
fn global_priority_labels_apply_when_component_and_fleet_unset() {
    let component_ref = ComponentRef::new(
        "sample-plugin".to_string(),
        "/tmp/sample-plugin".to_string(),
        None,
        Some("https://github.com/Extra-Chill/sample-plugin.git".to_string()),
        "component:sample-plugin".to_string(),
    );
    let global = vec!["critical".to_string()];
    let labels = resolve_priority_labels(&component_ref, Some(&global));
    let issues = issues_with_labels(vec![vec!["bug"], vec!["critical"]]);

    let actions = build_actions(Some(&issues), None, &labels);

    assert_eq!(labels, vec!["critical".to_string()]);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].label, "1 priority issue");
}

#[test]
fn fleet_priority_labels_apply_to_fleet_components() {
    crate::test_support::with_isolated_home(|home| {
        let component_dir = home.path().join(".config/homeboy/components");
        let project_dir = home.path().join(".config/homeboy/projects/site");
        let fleet_dir = home.path().join(".config/homeboy/fleets");
        std::fs::create_dir_all(&component_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(&fleet_dir).unwrap();
        std::fs::write(
            component_dir.join("sample-plugin.json"),
            r#"{
                    "local_path": "/tmp/sample-plugin",
                    "remote_url": "https://github.com/Extra-Chill/sample-plugin.git"
                }"#,
        )
        .unwrap();
        std::fs::write(
            project_dir.join("site.json"),
            r#"{
                    "components": [
                        {"id": "sample-plugin", "local_path": "/tmp/sample-plugin"}
                    ]
                }"#,
        )
        .unwrap();
        std::fs::write(
            fleet_dir.join("growth.json"),
            r#"{
                    "project_ids": ["site"],
                    "priority_labels": ["release-blocker"]
                }"#,
        )
        .unwrap();

        let refs = resolve_target_components(&TriageTarget::Fleet("growth".into())).unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].priority_labels,
            Some(vec!["release-blocker".to_string()])
        );
    });
}

#[test]
fn summarize_counts_component_actions() {
    let component = TriageComponentReport {
        component_id: "sample-plugin".to_string(),
        local_path: "/tmp/sample-plugin".to_string(),
        sources: vec!["component:sample-plugin".to_string()],
        usage: vec![],
        repo: TriageRepo {
            provider: "github",
            owner: "Extra-Chill".to_string(),
            name: "sample-plugin".to_string(),
            url: "https://github.com/Extra-Chill/sample-plugin".to_string(),
            source_repo: None,
            triage_remote_url: None,
        },
        issues: Some(TriageIssueBucket {
            open: 2,
            items: vec![
                TriageIssueItem {
                    number: 1,
                    title: "Bug".to_string(),
                    url: "https://github.com/o/r/issues/1".to_string(),
                    state: "OPEN".to_string(),
                    labels: vec!["P1".to_string()],
                    assignees: vec![],
                    updated_at: None,
                    comments_count: None,
                    last_comment_at: None,
                    stale: false,
                    linked_prs: Vec::new(),
                },
                TriageIssueItem {
                    number: 3,
                    title: "Needs triage".to_string(),
                    url: "https://github.com/o/r/issues/3".to_string(),
                    state: "OPEN".to_string(),
                    labels: vec![],
                    assignees: vec![],
                    updated_at: None,
                    comments_count: None,
                    last_comment_at: None,
                    stale: false,
                    linked_prs: Vec::new(),
                },
            ],
        }),
        pull_requests: Some(TriagePrBucket {
            open: 1,
            items: vec![TriagePrItem {
                number: 2,
                title: "Fix".to_string(),
                url: "https://github.com/o/r/pull/2".to_string(),
                state: "OPEN".to_string(),
                draft: false,
                signals: TriagePullRequestSignals {
                    checks: Some("FAILURE".to_string()),
                    review_decision: Some("REVIEW_REQUIRED".to_string()),
                    next_action: Some("checks_failed".to_string()),
                    ..TriagePullRequestSignals::default()
                },
                ci_readiness: None,
                check_failures: Vec::new(),
                labels: vec![],
                assignees: vec![],
                author: None,
                updated_at: None,
                stale: false,
            }],
        }),
        actions: vec![TriageAction {
            kind: "checks_failed".to_string(),
            severity: "high".to_string(),
            label: "1 PR has failed checks".to_string(),
        }],
        error: None,
    };

    let summary = summarize(&[component], &[]);
    assert_eq!(summary.components, 1);
    assert_eq!(summary.open_issues, 2);
    assert_eq!(summary.open_prs, 1);
    assert_eq!(summary.needs_review, 1);
    assert_eq!(summary.failing_checks, 1);
    assert_eq!(summary.actions, 1);
}
