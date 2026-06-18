use super::*;

mod parsing {
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
}

mod pull_requests {
    use super::*;

    #[test]
    fn summarize_checks_prefers_failures_over_pending() {
        let checks: Vec<Value> = serde_json::from_str(
            r#"[
                {"status":"IN_PROGRESS","conclusion":null},
                {"status":"COMPLETED","conclusion":"FAILURE"}
            ]"#,
        )
        .unwrap();
        assert_eq!(summarize_checks(&checks).as_deref(), Some("FAILURE"));
    }

    #[test]
    fn summarize_checks_reports_pending_and_success() {
        let pending: Vec<Value> =
            serde_json::from_str(r#"[{"status":"IN_PROGRESS","conclusion":null}]"#).unwrap();
        assert_eq!(summarize_checks(&pending).as_deref(), Some("PENDING"));

        let success: Vec<Value> =
            serde_json::from_str(r#"[{"status":"COMPLETED","conclusion":"SUCCESS"}]"#).unwrap();
        assert_eq!(summarize_checks(&success).as_deref(), Some("SUCCESS"));
    }

    #[test]
    fn parse_prs_omits_empty_optional_fields() {
        let raw = r#"[
            {
              "number": 9,
              "title": "Docs",
              "url": "https://github.com/o/r/pull/9",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "",
              "statusCheckRollup": [],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "comments": [{"createdAt":"2026-04-27T00:00:00Z","updatedAt":null}],
              "reviews": [{"submittedAt":"2026-04-28T00:00:00Z"}],
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;
        let items = parse_prs(raw, None, false).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].author.as_deref(), Some("example-org"));
        assert!(items[0].signals.review_decision.is_none());
        assert!(items[0].signals.merge_state.is_none());
        assert!(items[0].check_failures.is_empty());
        assert!(items[0].signals.next_action.is_none());
        assert_eq!(items[0].signals.comments_count, Some(1));
        assert_eq!(items[0].signals.reviews_count, Some(1));
        assert_eq!(
            items[0].signals.last_comment_at.as_deref(),
            Some("2026-04-27T00:00:00Z")
        );
        assert_eq!(
            items[0].signals.last_review_at.as_deref(),
            Some("2026-04-28T00:00:00Z")
        );
    }

    #[test]
    fn parse_prs_adds_compact_check_failure_drilldown_only_when_requested() {
        let raw = r#"[
            {
              "number": 10,
              "title": "Fix tests",
              "url": "https://github.com/o/r/pull/10",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": null,
              "mergeStateStatus": "DIRTY",
              "statusCheckRollup": [
                {
                  "__typename": "CheckRun",
                  "name": "test / unit",
                  "workflowName": "CI",
                  "status": "COMPLETED",
                  "conclusion": "FAILURE",
                  "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2"
                },
                {
                  "__typename": "StatusContext",
                  "context": "lint",
                  "status": "COMPLETED",
                  "conclusion": "SUCCESS",
                  "targetUrl": "https://example.test/lint"
                },
                {
                  "__typename": "CheckRun",
                  "workflowName": "CI",
                  "status": "COMPLETED",
                  "conclusion": "TIMED_OUT",
                  "detailsUrl": ""
                }
              ],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

        let without_drilldown = parse_prs(raw, None, false).unwrap();
        assert_eq!(
            without_drilldown[0].signals.checks.as_deref(),
            Some("FAILURE")
        );
        assert!(without_drilldown[0].check_failures.is_empty());

        let with_drilldown = parse_prs(raw, None, true).unwrap();
        assert_eq!(with_drilldown[0].check_failures.len(), 2);
        assert_eq!(
            with_drilldown[0].check_failures[0].workflow.as_deref(),
            Some("CI")
        );
        assert_eq!(with_drilldown[0].check_failures[0].name, "test / unit");
        assert_eq!(
            with_drilldown[0].check_failures[0].url.as_deref(),
            Some("https://github.com/o/r/actions/runs/1/job/2")
        );
        assert_eq!(with_drilldown[0].check_failures[1].name, "unknown check");
        assert!(with_drilldown[0].check_failures[1].url.is_none());
    }

    #[test]
    fn parse_prs_derives_next_action_labels() {
        let raw = r#"[
            {
              "number": 1,
              "title": "Broken checks",
              "url": "https://github.com/o/r/pull/1",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"FAILURE"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 2,
              "title": "Approved dirty",
              "url": "https://github.com/o/r/pull/2",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "DIRTY",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 3,
              "title": "Ready",
              "url": "https://github.com/o/r/pull/3",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 4,
              "title": "Needs eyes",
              "url": "https://github.com/o/r/pull/4",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "REVIEW_REQUIRED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 5,
              "title": "Pending",
              "url": "https://github.com/o/r/pull/5",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [{"status":"IN_PROGRESS","conclusion":null}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

        let items = parse_prs(raw, None, false).unwrap();
        let actions: Vec<_> = items
            .iter()
            .map(|item| item.signals.next_action.as_deref().unwrap())
            .collect();
        assert_eq!(
            actions,
            vec![
                "checks_failed",
                "approved_but_dirty",
                "clean_and_ready",
                "review_required",
                "approved_but_pending_checks",
            ]
        );
    }

    #[test]
    fn parse_prs_marks_behind_and_dirty_as_needs_rebase() {
        let raw = r#"[
            {
              "number": 1,
              "title": "Behind",
              "url": "https://github.com/o/r/pull/1",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "BEHIND",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 2,
              "title": "Dirty",
              "url": "https://github.com/o/r/pull/2",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "DIRTY",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            },
            {
              "number": 3,
              "title": "Unstable",
              "url": "https://github.com/o/r/pull/3",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "",
              "mergeStateStatus": "UNSTABLE",
              "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

        let items = parse_prs(raw, None, false).unwrap();
        assert_eq!(
            items[0].signals.next_action.as_deref(),
            Some("needs_rebase")
        );
        assert_eq!(
            items[1].signals.next_action.as_deref(),
            Some("needs_rebase")
        );
        assert!(items[2].signals.next_action.is_none());

        let actions = build_actions(
            None,
            Some(&TriagePrBucket {
                open: items.len(),
                items,
            }),
            &[],
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, "needs_rebase");
        assert_eq!(actions[0].severity, "medium");
        assert_eq!(actions[0].label, "2 PRs need rebase");
    }

    #[test]
    fn build_actions_prioritizes_pr_next_actions() {
        let prs = TriagePrBucket {
            open: 4,
            items: vec![
                triage_pr_with_action("clean_and_ready"),
                triage_pr_with_action("checks_failed"),
                triage_pr_with_action("review_required"),
                triage_pr_with_action("checks_failed"),
            ],
        };

        let priority_labels = default_priority_labels_vec();
        let actions = build_actions(None, Some(&prs), &priority_labels);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].kind, "checks_failed");
        assert_eq!(actions[0].severity, "high");
        assert_eq!(actions[0].label, "2 PRs have failed checks");
        assert_eq!(actions[1].kind, "review_required");
        assert_eq!(actions[2].kind, "clean_and_ready");
    }
}

mod priority_and_summary {
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
}

mod observations {
    use super::*;

    #[test]
    fn compare_triage_observations_reports_new_resolved_and_changed_items() {
        let previous = vec![
            stored_triage_item(1, "Old issue", None),
            stored_triage_item(2, "Resolved issue", None),
            stored_triage_item(3, "Changed PR", Some("review_required")),
        ];
        let current = vec![
            new_triage_item("current-run", 1, "Old issue", None),
            new_triage_item("current-run", 3, "Changed PR", Some("checks_failed")),
            new_triage_item("current-run", 4, "New issue", None),
        ];

        let comparison = compare_triage_observations("previous-run", &previous, &current);

        assert_eq!(comparison.previous_run_id, "previous-run");
        assert_eq!(comparison.previous_item_count, 3);
        assert_eq!(comparison.new_items.len(), 1);
        assert_eq!(comparison.new_items[0].number, 4);
        assert_eq!(comparison.resolved_items.len(), 1);
        assert_eq!(comparison.resolved_items[0].number, 2);
        assert_eq!(comparison.changed_items.len(), 1);
        assert_eq!(comparison.changed_items[0].item.number, 3);
        assert_eq!(
            comparison.changed_items[0].changed_fields,
            vec!["next_action"]
        );
    }

    #[test]
    fn triage_observation_metadata_distinguishes_personal_and_firehose_runs() {
        let personal = TriageOptions {
            mine: true,
            ..TriageOptions::default()
        };
        let firehose = TriageOptions {
            mine: false,
            ..TriageOptions::default()
        };

        assert_ne!(
            triage_observation_metadata(&TriageTarget::Workspace, &personal),
            triage_observation_metadata(&TriageTarget::Workspace, &firehose)
        );
    }

    #[test]
    fn compare_triage_observations_ignores_unknown_merge_state_flaps() {
        let mut previous = stored_triage_item(1, "Flappy PR", Some("checks_failed"));
        previous.signals.merge_state = Some("UNKNOWN".to_string());
        let mut current = new_triage_item("current-run", 1, "Flappy PR", Some("checks_failed"));
        current.signals.merge_state = Some("DIRTY".to_string());

        let comparison = compare_triage_observations("previous-run", &[previous], &[current]);

        assert!(comparison.changed_items.is_empty());
    }
}

fn triage_pr_with_action(action: &str) -> TriagePrItem {
    TriagePrItem {
        number: 1,
        title: "PR".to_string(),
        url: "https://github.com/o/r/pull/1".to_string(),
        state: "OPEN".to_string(),
        draft: false,
        signals: TriagePullRequestSignals {
            next_action: Some(action.to_string()),
            ..TriagePullRequestSignals::default()
        },
        check_failures: Vec::new(),
        labels: vec![],
        assignees: vec![],
        author: None,
        updated_at: None,
        stale: false,
    }
}

fn stored_triage_item(number: u64, title: &str, next_action: Option<&str>) -> TriageItemRecord {
    TriageItemRecord {
        id: format!("item-{number}"),
        run_id: "previous-run".to_string(),
        provider: "github".to_string(),
        repo_owner: "Extra-Chill".to_string(),
        repo_name: "homeboy".to_string(),
        item_type: "pull_request".to_string(),
        number,
        state: "OPEN".to_string(),
        title: title.to_string(),
        url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
        signals: TriagePullRequestSignals {
            next_action: next_action.map(str::to_string),
            ..TriagePullRequestSignals::default()
        },
        updated_at: None,
        metadata_json: serde_json::json!({}),
        observed_at: "2026-05-08T12:00:00Z".to_string(),
    }
}

fn new_triage_item(
    run_id: &str,
    number: u64,
    title: &str,
    next_action: Option<&str>,
) -> NewTriageItemRecord {
    NewTriageItemRecord {
        run_id: run_id.to_string(),
        provider: "github".to_string(),
        repo_owner: "Extra-Chill".to_string(),
        repo_name: "homeboy".to_string(),
        item_type: "pull_request".to_string(),
        number,
        state: "OPEN".to_string(),
        title: title.to_string(),
        url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
        signals: TriagePullRequestSignals {
            next_action: next_action.map(str::to_string),
            ..TriagePullRequestSignals::default()
        },
        updated_at: None,
        metadata_json: serde_json::json!({}),
    }
}

fn default_priority_labels_vec() -> Vec<String> {
    DEFAULT_PRIORITY_LABELS
        .iter()
        .map(|label| label.to_string())
        .collect()
}

fn issues_with_labels(labels: Vec<Vec<&str>>) -> TriageIssueBucket {
    TriageIssueBucket {
        open: labels.len(),
        items: labels
            .into_iter()
            .enumerate()
            .map(|(index, labels)| TriageIssueItem {
                number: index as u64 + 1,
                title: format!("Issue {}", index + 1),
                url: format!("https://github.com/o/r/issues/{}", index + 1),
                state: "OPEN".to_string(),
                labels: labels.into_iter().map(str::to_string).collect(),
                assignees: vec![],
                updated_at: None,
                comments_count: None,
                last_comment_at: None,
                stale: false,
                linked_prs: Vec::new(),
            })
            .collect(),
    }
}
mod targets {
    use super::*;

    #[test]
    fn resolve_repo_prefers_triage_remote_without_losing_source_repo() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/playground".to_string(),
            Some("https://github.com/example-org/wordpress-playground.git".to_string()),
            Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
            "component:playground".to_string(),
        );

        let resolved = resolve_repo(&component_ref).unwrap();

        assert_eq!(resolved.repo.owner, "WordPress");
        assert_eq!(resolved.repo.repo, "wordpress-playground");
        assert_eq!(
            resolved.triage_remote_url.as_deref(),
            Some("https://github.com/WordPress/wordpress-playground.git")
        );
        let source = resolved.source_repo.expect("source repo differs");
        assert_eq!(source.owner, "example-org");
        assert_eq!(source.repo, "wordpress-playground");
    }

    #[test]
    fn resolve_repo_allows_triage_remote_without_git_source_remote() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/not-a-git-repo".to_string(),
            None,
            Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
            "rig:studio".to_string(),
        );

        let resolved = resolve_repo(&component_ref).unwrap();

        assert_eq!(resolved.repo.owner, "WordPress");
        assert_eq!(resolved.repo.repo, "wordpress-playground");
        assert!(resolved.source_repo.is_none());
    }

    #[test]
    fn resolve_repo_uses_parent_repo_for_fork_without_triage_remote() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/playground".to_string(),
            Some("https://github.com/example-org/wordpress-playground.git".to_string()),
            None,
            "component:playground".to_string(),
        );

        let resolved = resolve_repo_with_parent_resolver(&component_ref, |repo| {
            assert_eq!(repo.owner, "example-org");
            assert_eq!(repo.repo, "wordpress-playground");
            Ok(Some(GitHubRepo {
                host: "github.com".to_string(),
                owner: "WordPress".to_string(),
                repo: "wordpress-playground".to_string(),
            }))
        })
        .unwrap();

        assert_eq!(resolved.repo.owner, "WordPress");
        assert_eq!(resolved.repo.repo, "wordpress-playground");
        assert!(resolved.triage_remote_url.is_none());
        let source = resolved.source_repo.expect("source repo is fork");
        assert_eq!(source.owner, "example-org");
        assert_eq!(source.repo, "wordpress-playground");
    }

    #[test]
    fn parse_github_parent_repo_returns_parent_for_fork() {
        let parent = parse_github_parent_repo(
            r#"{
                "isFork": true,
                "parent": {
                    "name": "wordpress-playground",
                    "owner": { "login": "WordPress" }
                }
            }"#,
        )
        .unwrap()
        .expect("fork parent");

        assert_eq!(parent.owner, "WordPress");
        assert_eq!(parent.repo, "wordpress-playground");
    }

    #[test]
    fn parse_github_parent_repo_ignores_non_forks() {
        let parent = parse_github_parent_repo(
            r#"{
                "isFork": false,
                "parent": null
            }"#,
        )
        .unwrap();

        assert!(parent.is_none());
    }

    #[test]
    fn fetch_component_report_surfaces_source_repo_when_triage_differs() {
        let component_ref = ComponentRef::new(
            "playground".to_string(),
            "/tmp/playground".to_string(),
            Some("https://github.com/example-org/wordpress-playground.git".to_string()),
            Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
            "rig:studio".to_string(),
        );
        let resolved = resolve_repo(&component_ref).unwrap();

        let report = fetch_component_report(
            &component_ref,
            resolved,
            &TriageOptions {
                include_issues: false,
                include_prs: false,
                ..Default::default()
            },
            None,
        );

        assert_eq!(report.repo.owner, "WordPress");
        assert_eq!(report.repo.name, "wordpress-playground");
        assert_eq!(
            report.repo.triage_remote_url.as_deref(),
            Some("https://github.com/WordPress/wordpress-playground.git")
        );
        assert_eq!(
            report.repo.source_repo,
            Some(TriageRepoRef {
                owner: "example-org".to_string(),
                name: "wordpress-playground".to_string(),
                url: "https://github.com/example-org/wordpress-playground".to_string(),
            })
        );
    }

    #[test]
    fn component_target_threads_registered_triage_remote_override() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("playground");
            std::fs::create_dir_all(&checkout).unwrap();
            let component_dir = home.path().join(".config/homeboy/components");
            std::fs::create_dir_all(&component_dir).unwrap();
            std::fs::write(
                component_dir.join("playground.json"),
                format!(
                    r#"{{
                    "local_path": "{}",
                    "remote_url": "https://github.com/example-org/wordpress-playground.git",
                    "triage_remote_url": "https://github.com/WordPress/wordpress-playground.git"
                }}"#,
                    checkout.display()
                ),
            )
            .unwrap();

            let refs =
                resolve_target_components(&TriageTarget::Component("playground".into())).unwrap();

            assert_eq!(refs.len(), 1);
            assert_eq!(
                refs[0].triage_remote_url.as_deref(),
                Some("https://github.com/WordPress/wordpress-playground.git")
            );
            assert_eq!(
                resolve_repo(&refs[0]).unwrap().repo.owner,
                "WordPress".to_string()
            );
        });
    }

    #[test]
    fn rig_target_threads_rig_component_triage_remote_override() {
        crate::test_support::with_isolated_home(|home| {
            let rig_dir = home.path().join(".config/homeboy/rigs");
            std::fs::create_dir_all(&rig_dir).unwrap();
            std::fs::write(
                rig_dir.join("studio.json"),
                r#"{
                    "id": "studio",
                    "components": {
                        "playground": {
                            "path": "/tmp/playground",
                            "remote_url": "https://github.com/example-org/wordpress-playground.git",
                            "triage_remote_url": "https://github.com/WordPress/wordpress-playground.git"
                        }
                    }
                }"#,
            )
            .unwrap();

            let refs = resolve_target_components(&TriageTarget::Rig("studio".into())).unwrap();

            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].component_id, "playground");
            assert_eq!(
                refs[0].triage_remote_url.as_deref(),
                Some("https://github.com/WordPress/wordpress-playground.git")
            );
            assert_eq!(
                resolve_repo(&refs[0]).unwrap().repo.owner,
                "WordPress".to_string()
            );
        });
    }

    #[test]
    fn path_target_synthesizes_component_from_git_origin() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("ad-hoc-checkout");
            std::fs::create_dir_all(&checkout).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());
            let status = std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/Extra-Chill/homeboy.git",
                ])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: None,
            };
            let refs = resolve_target_components(&target).unwrap();
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].component_id, "ad-hoc-checkout");
            assert_eq!(
                refs[0].remote_url.as_deref(),
                Some("https://github.com/Extra-Chill/homeboy.git")
            );
            let repo = resolve_repo(&refs[0]).unwrap().repo;
            assert_eq!(repo.owner, "Extra-Chill");
            assert_eq!(repo.repo, "homeboy");
        });
    }

    #[test]
    fn path_target_uses_explicit_component_id_when_provided() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("checkout-dir");
            std::fs::create_dir_all(&checkout).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());
            let status = std::process::Command::new("git")
                .args([
                    "remote",
                    "add",
                    "origin",
                    "git@github.com:Extra-Chill/homeboy.git",
                ])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: Some("homeboy".into()),
            };
            let refs = resolve_target_components(&target).unwrap();
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].component_id, "homeboy");
            let repo = resolve_repo(&refs[0]).unwrap().repo;
            assert_eq!(repo.owner, "Extra-Chill");
            assert_eq!(repo.repo, "homeboy");
        });
    }

    #[test]
    fn path_target_surfaces_remote_url_is_not_github_for_non_github_origin() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("non-github");
            std::fs::create_dir_all(&checkout).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());
            let status = std::process::Command::new("git")
                .args(["remote", "add", "origin", "https://gitlab.com/foo/bar.git"])
                .current_dir(&checkout)
                .status()
                .unwrap();
            assert!(status.success());

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: None,
            };
            let refs = resolve_target_components(&target).unwrap();
            let err = resolve_repo(&refs[0]).unwrap_err();
            assert_eq!(err, "remote_url_is_not_github");
        });
    }

    #[test]
    fn path_target_rejects_missing_directory() {
        let target = TriageTarget::Path {
            path: "/definitely/does/not/exist/triage-path-test".into(),
            component_id: None,
        };
        let err = resolve_target_components(&target).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn path_target_rejects_non_git_directory() {
        crate::test_support::with_isolated_home(|home| {
            let checkout = home.path().join("not-a-git-repo");
            std::fs::create_dir_all(&checkout).unwrap();

            let target = TriageTarget::Path {
                path: checkout.to_string_lossy().into_owned(),
                component_id: None,
            };
            let err = resolve_target_components(&target).unwrap_err();
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
        });
    }
}
