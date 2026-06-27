use super::super::*;
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
fn summarize_ci_readiness_groups_requirement_states_and_actions() {
    let checks: Vec<Value> = serde_json::from_str(
        r#"[
                {
                  "name": "required queued",
                  "required": true,
                  "status": "QUEUED",
                  "conclusion": null,
                  "startedAt": "2026-04-26T10:00:00Z"
                },
                {
                  "name": "required running",
                  "isRequired": true,
                  "status": "IN_PROGRESS",
                  "conclusion": null,
                  "startedAt": "2026-04-26T10:10:00Z"
                },
                {
                  "name": "required passed",
                  "required": true,
                  "status": "COMPLETED",
                  "conclusion": "SUCCESS"
                },
                {
                  "name": "optional failed",
                  "required": false,
                  "status": "COMPLETED",
                  "conclusion": "FAILURE",
                  "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2"
                },
                {
                  "name": "unknown skipped",
                  "status": "COMPLETED",
                  "conclusion": "SKIPPED"
                }
            ]"#,
    )
    .unwrap();
    let now = DateTime::parse_from_rfc3339("2026-04-26T10:45:00Z")
        .unwrap()
        .with_timezone(&Utc);

    let readiness = summarize_ci_readiness(&checks, now).unwrap();

    assert_eq!(readiness.checks.required.queued, 1);
    assert_eq!(readiness.checks.required.running, 1);
    assert_eq!(readiness.checks.required.passed, 1);
    assert_eq!(readiness.checks.optional.failed, 1);
    assert_eq!(readiness.checks.unknown_requirement.skipped, 1);
    assert_eq!(
        readiness.oldest_pending_started_at.as_deref(),
        Some("2026-04-26T10:00:00+00:00")
    );
    assert_eq!(readiness.oldest_pending_duration_seconds, Some(2700));
    assert_eq!(
        readiness.failure_urls,
        vec!["https://github.com/o/r/actions/runs/1/job/2"]
    );
    assert!(readiness.next_steps.iter().any(|step| step.contains(
        "Wait for required checks to finish; oldest pending check has been active for 45m."
    )));
}

#[test]
fn parse_prs_marks_pending_required_checks_as_next_action() {
    let raw = r#"[
            {
              "number": 11,
              "title": "Waiting on required CI",
              "url": "https://github.com/o/r/pull/11",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [
                {"name":"required", "required": true, "status":"IN_PROGRESS", "conclusion":null},
                {"name":"optional", "required": false, "status":"COMPLETED", "conclusion":"SUCCESS"}
              ],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

    let items = parse_prs(raw, None, false).unwrap();

    assert_eq!(
        items[0].signals.next_action.as_deref(),
        Some("required_checks_pending")
    );
    let readiness = items[0].ci_readiness.as_ref().unwrap();
    assert_eq!(readiness.checks.required.running, 1);
    assert_eq!(readiness.checks.optional.passed, 1);
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
fn landing_classifier_covers_ready_and_blocked_states() {
    assert_eq!(
        classify_landing_pr("MERGED", Some("2026-06-01T00:00:00Z"), None, None),
        TriageLandingClassification::Merged
    );
    assert_eq!(
        classify_landing_pr("OPEN", None, Some("SUCCESS"), Some("CLEAN")),
        TriageLandingClassification::CleanMergeable
    );
    assert_eq!(
        classify_landing_pr("OPEN", None, Some("SUCCESS"), Some("DIRTY")),
        TriageLandingClassification::ConflictRepairNeeded
    );
    assert_eq!(
        classify_landing_pr("OPEN", None, Some("PENDING"), Some("CLEAN")),
        TriageLandingClassification::ChecksPending
    );
    assert_eq!(
        classify_landing_pr("OPEN", None, Some("FAILURE"), Some("CLEAN")),
        TriageLandingClassification::CandidateRed
    );
    assert_eq!(
        classify_landing_pr("OPEN", None, None, Some("UNKNOWN")),
        TriageLandingClassification::BaselineRedInconclusive
    );
    assert_eq!(
        landing_mergeability_state(Some("CLEAN")),
        TriageLandingMergeabilityState::Clean
    );
    assert_eq!(
        landing_mergeability_state(Some("DIRTY")),
        TriageLandingMergeabilityState::Conflicting
    );
    assert_eq!(
        landing_mergeability_state(Some("UNKNOWN")),
        TriageLandingMergeabilityState::Unknown
    );
    assert_eq!(
        landing_mergeability_state(Some("UNSTABLE")),
        TriageLandingMergeabilityState::Unstable
    );
    assert_eq!(
        landing_check_state(Some("PENDING")),
        TriageLandingCheckState::Pending
    );
    assert_eq!(
        landing_check_state(Some("FAILURE")),
        TriageLandingCheckState::Failed
    );
}

#[test]
fn parse_landing_pr_adds_classification_and_command() {
    let repo = GitHubRepo {
        host: "github.com".to_string(),
        owner: "Extra-Chill".to_string(),
        repo: "homeboy".to_string(),
    };
    let raw = r#"{
          "number": 42,
          "title": "Ready",
          "url": "https://github.com/Extra-Chill/homeboy/pull/42",
          "state": "OPEN",
          "isDraft": false,
          "reviewDecision": "APPROVED",
          "mergeStateStatus": "CLEAN",
          "statusCheckRollup": [{"status":"COMPLETED","conclusion":"SUCCESS"}],
          "baseRefName": "main",
          "headRefName": "cook/ready",
          "headRepository": {"name":"homeboy"},
          "headRepositoryOwner": {"login":"Extra-Chill"},
          "mergedAt": null,
          "comments": [],
          "reviews": [],
          "updatedAt": "2026-06-01T00:00:00Z"
        }"#;

    let item = parse_landing_pr(raw, &repo, false).unwrap();

    assert_eq!(
        item.classification,
        TriageLandingClassification::CleanMergeable
    );
    assert_eq!(
        item.mergeability_state,
        TriageLandingMergeabilityState::Clean
    );
    assert_eq!(item.check_state, TriageLandingCheckState::Clean);
    assert_eq!(item.head_branch.as_deref(), Some("cook/ready"));
    assert_eq!(item.base_branch.as_deref(), Some("main"));
    assert_eq!(item.head_repo.as_deref(), Some("Extra-Chill/homeboy"));
    assert_eq!(
        item.suggested_next_command,
        "homeboy triage --watch Extra-Chill/homeboy#42 --until green-mergeable"
    );
    assert!(item.dependent_rebase.is_none());
}

#[test]
fn ordered_landing_preserves_input_order_and_dedupes() {
    let mut items = vec![
        landing_pr(
            2,
            "Extra-Chill/homeboy",
            "main",
            "feature/two",
            "Extra-Chill/homeboy",
        ),
        landing_pr(
            1,
            "Extra-Chill/homeboy",
            "main",
            "feature/one",
            "Extra-Chill/homeboy",
        ),
        landing_pr(
            2,
            "Extra-Chill/homeboy",
            "main",
            "feature/two",
            "Extra-Chill/homeboy",
        ),
    ];

    dedupe_landing_prs_preserving_order(&mut items);

    assert_eq!(
        items.iter().map(|item| item.number).collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[test]
fn ordered_landing_generates_same_repo_dependent_rebase_command() {
    let mut items = vec![
        landing_pr(
            10,
            "Extra-Chill/homeboy",
            "main",
            "feature/base",
            "Extra-Chill/homeboy",
        ),
        landing_pr(
            11,
            "Extra-Chill/homeboy",
            "main",
            "feature/dependent",
            "Extra-Chill/homeboy",
        ),
    ];

    annotate_ordered_dependent_rebases(&mut items);

    assert!(items[0].dependent_rebase.is_none());
    let plan = items[1].dependent_rebase.as_ref().unwrap();
    assert_eq!(plan.after_pr, 10);
    assert!(plan.safe_to_update);
    assert_eq!(plan.reason, "same_repo_head_branch");
    assert_eq!(
            plan.command.as_deref(),
            Some("gh pr checkout 11 -R Extra-Chill/homeboy && git fetch origin main && git rebase origin/main && git push --force-with-lease origin HEAD:feature/dependent")
        );
}

#[test]
fn ordered_landing_marks_forked_heads_manual() {
    let item = landing_pr(
        11,
        "Extra-Chill/homeboy",
        "main",
        "feature/dependent",
        "someone/homeboy",
    );

    let plan = dependent_rebase_plan(&item, 10);

    assert!(!plan.safe_to_update);
    assert_eq!(plan.reason, "head_branch_not_in_base_repo");
    assert!(plan.command.is_none());
}

#[test]
fn landing_pr_ref_accepts_number_repo_ref_and_url() {
    let repo = GitHubRepo {
        host: "github.com".to_string(),
        owner: "Extra-Chill".to_string(),
        repo: "homeboy".to_string(),
    };

    assert_eq!(
        parse_landing_pr_ref("42", &repo).unwrap().unwrap(),
        LandingPrRef {
            owner: "Extra-Chill".to_string(),
            repo: "homeboy".to_string(),
            number: 42,
        }
    );
    assert_eq!(
        parse_landing_pr_ref("Extra-Chill/homeboy#43", &repo)
            .unwrap()
            .unwrap()
            .number,
        43
    );
    assert_eq!(
        parse_landing_pr_ref("https://github.com/Extra-Chill/homeboy/pull/44", &repo)
            .unwrap()
            .unwrap()
            .number,
        44
    );
}

#[test]
fn bare_pr_number_detection_only_matches_numbers() {
    assert!(is_bare_pr_number("42"));
    assert!(is_bare_pr_number("#42"));
    assert!(!is_bare_pr_number("Extra-Chill/homeboy#42"));
    assert!(!is_bare_pr_number(
        "https://github.com/Extra-Chill/homeboy/pull/42"
    ));
}

#[test]
fn branch_matcher_supports_simple_globs_and_contains() {
    assert!(branch_matches("cook/*", "cook/landing"));
    assert!(branch_matches("*landing", "cook/landing"));
    assert!(branch_matches("*land*", "cook/landing"));
    assert!(branch_matches("land", "cook/landing"));
    assert!(!branch_matches("fix/*", "cook/landing"));
}

#[test]
fn parse_prs_flags_clean_with_zero_checks_as_not_reported() {
    // Reproduces the #4872 force-push window: GitHub reports mergeStateStatus
    // CLEAN with an empty statusCheckRollup before CI registers on the new head.
    // This must surface a distinct "checks not reported" action rather than
    // silently reading as clean_and_ready (which would let merge automation
    // merge a commit whose CI has never run).
    let raw = r#"[
            {
              "number": 1,
              "title": "Just force-pushed",
              "url": "https://github.com/o/r/pull/1",
              "state": "OPEN",
              "isDraft": false,
              "reviewDecision": "APPROVED",
              "mergeStateStatus": "CLEAN",
              "statusCheckRollup": [],
              "labels": [],
              "assignees": [],
              "author": {"login":"example-org"},
              "updatedAt": "2026-04-26T00:00:00Z"
            }
        ]"#;

    let items = parse_prs(raw, None, false).unwrap();
    assert_eq!(
        items[0].signals.next_action.as_deref(),
        Some("clean_but_checks_not_reported")
    );
    // The zero-check rollup must never be summarized as a successful state.
    assert!(items[0].signals.checks.is_none());
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

fn landing_pr(
    number: u64,
    repo: &str,
    base_branch: &str,
    head_branch: &str,
    head_repo: &str,
) -> TriageLandingPr {
    TriageLandingPr {
        repo: repo.to_string(),
        number,
        title: format!("PR {number}"),
        url: format!("https://github.com/{repo}/pull/{number}"),
        state: "OPEN".to_string(),
        base_branch: Some(base_branch.to_string()),
        head_branch: Some(head_branch.to_string()),
        head_repo: Some(head_repo.to_string()),
        mergeability_state: TriageLandingMergeabilityState::Clean,
        check_state: TriageLandingCheckState::Clean,
        classification: TriageLandingClassification::CleanMergeable,
        suggested_next_command: format!(
            "homeboy triage --watch {repo}#{number} --until green-mergeable"
        ),
        dependent_rebase: None,
        signals: TriagePullRequestSignals::default(),
        check_failures: Vec::new(),
    }
}
