//! Contract test for the caller-facing Cook summary (#18817): status, watch,
//! diagnose and the launch output all emit the same versioned object under
//! `cook_summary`, so `jq '.cook_summary.status'` reads every surface alike.

use super::*;
use crate::commands::agent_task::cook_summary;
use homeboy_control_plane_contract::{CookSummary, COOK_SUMMARY_KEY, COOK_SUMMARY_SCHEMA};

fn record(state: &str, metadata: Value) -> AgentTaskRunRecord {
    let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1", "run_id": "run-1", "plan_id": "plan",
        "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
    }))
    .expect("record");
    record.metadata = metadata;
    record
}

fn gate_failed_record() -> AgentTaskRunRecord {
    let metadata = homeboy::agents::agent_task_service::cook_delivery_metadata_for_test(json!({
        "promotion": {
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "gate_failed",
            "task_id": "task-1",
            "source": { "kind": "aggregate", "task_id": "task-1", "run_id": "run-1" },
            "to_worktree": "homeboy@cook",
            "target": { "worktree": "homeboy@cook", "path": "/work/homeboy", "branch": "cook/fix", "head": "abc123" },
            "candidate": { "base_sha": "base", "candidate_sha": "patch-1", "tree_sha": "tree-1" },
            "changed_files": ["src/lib.rs"],
            "verified_base": { "base": "origin/main", "sha": "base-sha" },
            "deterministic_gates": [{
                "id": "cargo-test",
                "status": "failed",
                "command": ["cargo", "test"],
                "exit_code": 101,
                "failure_evidence": {
                    "summary": "1 test failed", "command": "cargo test", "exit_code": 101,
                    "agent_feedback": "",
                    "failed_tests": [{ "id": "suite::case" }]
                }
            }],
            "provenance": {
                "post_apply": true,
                "worktree_path": "/work/homeboy",
                "candidate": { "kind": "git", "fingerprint": { "sha256": "fp", "head": "abc123" } }
            }
        }
    }));
    record("candidate_recoverable", metadata)
}

fn assert_contract(surface: &str, value: &Value) -> CookSummary {
    let summary: CookSummary = serde_json::from_value(value[COOK_SUMMARY_KEY].clone())
        .unwrap_or_else(|error| panic!("{surface} cook_summary: {error}"));
    summary
        .validate()
        .unwrap_or_else(|error| panic!("{surface} cook_summary: {error}"));
    assert_eq!(summary.schema, COOK_SUMMARY_SCHEMA, "{surface}");
    summary
}

/// Every Cook read surface, built from one run, emits one identical summary.
#[test]
fn every_cook_read_surface_emits_the_same_cook_summary() {
    let record = gate_failed_record();
    let expected = cook_summary::of_record(&record);
    expected
        .validate()
        .expect("projection honours the contract");
    assert_eq!(expected.gates[0].failing_tests, vec!["suite::case"]);
    assert_eq!(expected.head_sha.as_deref(), Some("patch-1"));
    assert_eq!(expected.base_sha.as_deref(), Some("base-sha"));
    assert!(expected.pr_url.is_none());
    assert!(expected
        .why_no_pr
        .as_deref()
        .is_some_and(|why| why.contains("gate_failed")));

    // `agent-task status` (compact and --full).
    let mut status = compact_control_plane_status(json!({
        "run": "run-1", "state": "candidate_recoverable",
    }));
    cook_summary::attach(&mut status, Some(expected.clone()));
    let mut status_full = json!({ "run": "run-1", "state": "candidate_recoverable" });
    cook_summary::attach(&mut status_full, Some(expected.clone()));

    // `agent-task status --watch`, observing the status snapshot.
    let (watch, _) = watch_status_output(
        &StatusArgs {
            run_id: "run-1".to_string(),
            watch: true,
            interval: "1s".to_string(),
            timeout: "1m".to_string(),
            ..Default::default()
        },
        WatchResult {
            item: (status.clone(), 0),
            conclusion: WatchConclusion::Terminal,
            poll_count: 1,
            waited: Duration::from_secs(1),
        },
        StatusWatchProgress::default(),
    );

    // `agent-task diagnose` attaches the projection of its durable record.
    let mut diagnose = json!({
        "schema": "homeboy/agent-task-diagnose/v1",
        "run_id": "run-1",
        "summary": "agent_task.gate_failed: cargo-test failed",
    });
    cook_summary::attach(&mut diagnose, Some(cook_summary::of_record(&record)));

    // Cook launch output, compact and --full.
    let report = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": "cook-1",
        "latest_run_id": "run-1",
        "status": "gate_failed",
        "attempts": [],
    });
    let launch = compact_cook_report_with_summary(report.clone(), false, Some(expected.clone()));
    let launch_full = compact_cook_report_with_summary(report, true, Some(expected.clone()));

    for (surface, value) in [
        ("status", &status),
        ("status --full", &status_full),
        ("status --watch", &watch),
        ("diagnose", &diagnose),
        ("launch", &launch),
        ("launch --full", &launch_full),
    ] {
        assert_eq!(assert_contract(surface, value), expected, "{surface}");
        assert_eq!(
            value["cook_summary"]["status"], expected.status,
            "{surface}: .cook_summary.status reads the same everywhere"
        );
    }
    // Diagnose keeps its human-readable `summary` string beside the object.
    assert!(diagnose["summary"].is_string());
}

/// Surfaces without a durable record still honour the contract.
#[test]
fn surfaces_without_a_record_emit_a_valid_fallback_summary() {
    let mut recipe_only = json!({
        "schema": "homeboy/agent-task-cook/v1",
        "status": "recipe_only_recovery_required",
    });
    cook_summary::ensure(&mut recipe_only);
    let summary = assert_contract("recipe-only status", &recipe_only);
    assert!(summary.why_no_pr.is_some());

    let mut ci = json!({
        "cook_id": "cook-1",
        "pr_url": "https://github.com/o/r/pull/1",
        "status": "awaiting_ci",
    });
    cook_summary::ensure(&mut ci);
    let summary = assert_contract("ci status", &ci);
    assert_eq!(
        summary.pr_url.as_deref(),
        Some("https://github.com/o/r/pull/1")
    );
    assert_eq!(summary.why_no_pr, None);

    let launch = compact_cook_report_with_summary(
        json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-1",
            "status": "failed",
            "stop_reason": "provider exited",
            "attempts": [],
        }),
        false,
        None,
    );
    let summary = assert_contract("launch without record", &launch);
    assert_eq!(
        summary.why_no_pr.as_deref(),
        Some("failed: provider exited")
    );

    let (watch, _) = watch_status_output(
        &StatusArgs {
            run_id: "run-1".to_string(),
            ..Default::default()
        },
        WatchResult {
            item: (json!({ "run_id": "run-1", "state": "running" }), 0),
            conclusion: WatchConclusion::TimedOut,
            poll_count: 1,
            waited: Duration::from_secs(1),
        },
        StatusWatchProgress::default(),
    );
    let summary = assert_contract("watch without record", &watch);
    assert!(!summary.terminal);
    assert_eq!(summary.why_no_pr, None);
}
