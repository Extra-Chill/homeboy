use super::*;
use homeboy_lifecycle_contract::RunExecutionState;
use serde_json::json;

fn record(state: RunExecutionState, metadata: Value) -> AgentTaskRunRecord {
    let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1", "run_id": "run", "plan_id": "plan",
        "state": state, "submitted_at": "2026-01-01T00:00:00Z", "plan_path": "/plan",
    }))
    .expect("record");
    record.metadata = metadata;
    record
}

fn promotion(status: &str, gates: Value) -> Value {
    json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": status,
        "task_id": "task-1",
        "source": { "kind": "aggregate", "task_id": "task-1", "run_id": "run-1" },
        "to_worktree": "homeboy@cook",
        "target": { "worktree": "homeboy@cook", "path": "/work/homeboy", "branch": "cook/fix", "head": "abc123" },
        "candidate": { "base_sha": "base", "candidate_sha": "patch-1", "tree_sha": "patchsha" },
        "changed_files": ["src/lib.rs"],
        "verified_base": { "base": "origin/main", "sha": "base-sha" },
        "deterministic_gates": gates,
        "provenance": {
            "post_apply": true,
            "worktree_path": "/work/homeboy",
            "candidate": { "kind": "git", "fingerprint": { "sha256": "fingerprint-sha", "head": "abc123" } }
        }
    })
}

fn delivery(artifacts: Value) -> Value {
    crate::agent_task_service::cook_delivery_metadata_for_test(artifacts)
}

#[test]
fn gate_failure_names_failing_tests_and_why_no_pr() {
    let metadata = delivery(json!({
        "promotion": promotion("gate_failed", json!([{
            "id": "cargo-test",
            "status": "failed",
            "command": ["cargo", "test"],
            "exit_code": 101,
            "failure_evidence": {
                "summary": "2 tests failed", "command": "cargo test", "exit_code": 101,
                "agent_feedback": "",
                "failed_tests": [{ "id": "a::one" }, { "id": "b::two" }]
            }
        }])),
    }));
    let summary = cook_summary(&record(RunExecutionState::CandidateRecoverable, metadata));
    summary.validate().expect("valid summary");
    assert!(summary.terminal);
    assert_eq!(summary.head_sha.as_deref(), Some("patch-1"));
    assert_eq!(summary.base_sha.as_deref(), Some("base-sha"));
    assert_eq!(summary.gates.len(), 1);
    assert_eq!(summary.gates[0].name, "cargo-test");
    assert_eq!(summary.gates[0].state, "failed");
    assert_eq!(summary.gates[0].failing_tests, vec!["a::one", "b::two"]);
    assert_eq!(summary.pr_url, None);
    let why = summary.why_no_pr.expect("why_no_pr");
    assert!(
        why.starts_with(&format!("{}: gate_failed", summary.status)),
        "{why}"
    );
}

#[test]
fn published_run_reports_pr_url_without_a_reason() {
    let metadata = delivery(json!({
        "promotion": promotion("applied", json!([{
            "id": "cargo-test", "status": "succeeded", "command": ["cargo", "test"], "exit_code": 0
        }])),
        "finalization": {
            "status": "review_ready",
            "pr_number": 42,
            "pr_url": "https://github.com/Extra-Chill/homeboy/pull/42",
            "publication_proof": { "binding": { "candidate_sha": "commit-sha", "remote_sha": "remote-sha" } }
        },
    }));
    let summary = cook_summary(&record(RunExecutionState::Succeeded, metadata));
    summary.validate().expect("valid summary");
    assert_eq!(
        summary.pr_url.as_deref(),
        Some("https://github.com/Extra-Chill/homeboy/pull/42")
    );
    assert_eq!(summary.why_no_pr, None);
    assert_eq!(summary.head_sha.as_deref(), Some("remote-sha"));
    assert_eq!(summary.gates[0].state, "succeeded");
    assert!(summary.gates[0].failing_tests.is_empty());
}

#[test]
fn undelivered_items_settle_succeeded_partial_with_the_list() {
    let metadata = delivery(json!({
        "promotion": promotion("applied", json!([{
            "id": "cargo-test", "status": "succeeded", "command": ["cargo", "test"], "exit_code": 0
        }])),
        "finalization": {
            "status": "succeeded_partial",
            "pr_number": 43,
            "pr_url": "https://github.com/Extra-Chill/homeboy/pull/43",
            "pr_draft": true,
            "review_dossier": { "delivery": [
                { "item": "Parse the flag", "status": "delivered" },
                { "item": "Migrate records", "status": "not_delivered", "reason": "needs a decision" }
            ] },
            "publication_proof": { "binding": { "candidate_sha": "commit-sha", "remote_sha": "remote-sha" } }
        },
    }));
    let summary = cook_summary(&record(RunExecutionState::Succeeded, metadata));
    summary.validate().expect("valid summary");
    assert_eq!(summary.status, "succeeded_partial");
    assert!(summary.has_undelivered_items());
    assert_eq!(summary.delivery.len(), 2);
    assert_eq!(summary.delivery[0].item, "Migrate records");
    assert_eq!(
        summary.delivery[0].reason.as_deref(),
        Some("needs a decision")
    );
    assert_eq!(
        summary.pr_url.as_deref(),
        Some("https://github.com/Extra-Chill/homeboy/pull/43")
    );
}

#[test]
fn every_terminal_phase_without_a_pr_names_why() {
    for state in [
        RunExecutionState::CandidateRecoverable,
        RunExecutionState::Succeeded,
        RunExecutionState::PartialFailure,
        RunExecutionState::Failed,
        RunExecutionState::Cancelled,
        RunExecutionState::Unknown,
    ] {
        let summary = cook_summary(&record(state, json!({})));
        summary.validate().expect("valid summary");
        assert!(summary.terminal, "{state:?}");
        assert!(summary.why_no_pr.is_some(), "{state:?}");
        assert!(
            summary
                .why_no_pr
                .as_deref()
                .unwrap()
                .starts_with(&format!("{}: ", summary.status)),
            "{state:?}"
        );
    }
}

#[test]
fn in_flight_runs_have_no_reason_yet() {
    for state in [RunExecutionState::Queued, RunExecutionState::Running] {
        let summary = cook_summary(&record(state, json!({})));
        summary.validate().expect("valid summary");
        assert!(!summary.terminal);
        assert_eq!(summary.why_no_pr, None);
        assert_eq!(summary.pr_url, None);
    }
}

/// #18869: a failed bench gate links its compare artifact and aggregate
/// summary from the Cook summary.
#[test]
fn bench_gate_links_its_artifact_and_aggregate_summary() {
    let metadata = delivery(json!({
        "promotion": promotion("gate_failed", json!([{
            "id": "bench",
            "status": "failed",
            "command": ["sh", "-c", "homeboy bench compare --cook-gate --base base-sha --head WORKTREE --path ."],
            "exit_code": 1,
            "bench_compare": {
                "schema": homeboy_core::bench_compare::GATE_EVIDENCE_SCHEMA,
                "compare_id": "bench-compare-1",
                "passed": false,
                "fail_on": "new-blockers",
                "summary": {
                    "routes": 4, "improved": 1, "regressed": 1, "unchanged": 2,
                    "added_on_head": 0, "missing_on_head": 0, "output_changed": 2,
                    "new_blockers": 1, "resolved_blockers": 0,
                    "new_findings": 0, "resolved_findings": 0
                },
                "violations": ["head introduced 1 new evaluator blocker(s)"],
                "artifact_path": "/data/bench-compare/bench-compare-1/bench-compare.json"
            }
        }])),
    }));
    let summary = cook_summary(&record(RunExecutionState::CandidateRecoverable, metadata));
    summary.validate().expect("valid summary");
    let bench = summary.gates[0]
        .bench_compare
        .as_ref()
        .expect("bench summary");
    assert!(!bench.passed);
    assert_eq!(bench.fail_on, "new-blockers");
    assert_eq!(
        (bench.routes, bench.regressed, bench.new_blockers),
        (4, 1, 1)
    );
    assert_eq!(
        bench.artifact_path,
        "/data/bench-compare/bench-compare-1/bench-compare.json"
    );
}

/// #18888: the summary accounts for every attempt, not just the last one.
#[test]
fn summary_lists_every_attempt_with_sha_gates_and_disposition() {
    let superseded = record(
        RunExecutionState::CandidateRecoverable,
        delivery(json!({
            "promotion": promotion("gate_failed", json!([{
                "id": "cargo-test", "status": "failed", "command": ["cargo", "test"], "exit_code": 101
            }])),
        })),
    );
    let published = record(
        RunExecutionState::Succeeded,
        delivery(json!({
            "promotion": promotion("applied", json!([{
                "id": "cargo-test", "status": "succeeded", "command": ["cargo", "test"], "exit_code": 0
            }])),
            "finalization": {
                "status": "review_ready",
                "pr_number": 42,
                "pr_url": "https://github.com/Extra-Chill/homeboy/pull/42",
                "publication_proof": { "binding": { "candidate_sha": "commit-sha", "remote_sha": "remote-sha" } }
            },
        })),
    );
    let discarded = record(RunExecutionState::Failed, json!({}));
    let summary = cook_summary_with_attempts(
        &published,
        &[(1, superseded), (2, discarded), (3, published.clone())],
    );
    summary.validate().expect("valid summary");
    assert_eq!(
        summary.attempts,
        vec![
            "attempt 1: patch-1 · gates cargo-test=failed · superseded",
            "attempt 2: none · gates none · discarded",
            "attempt 3: patch-1 · gates cargo-test=succeeded · published",
        ]
    );

    // Without attempt history the record is its Cook's only attempt.
    let single = cook_summary(&published);
    assert_eq!(single.attempts.len(), 1);
    assert!(single.attempts[0].starts_with("attempt 1: "));
}
