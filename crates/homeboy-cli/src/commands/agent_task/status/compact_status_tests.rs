use super::{compact_control_plane_status, deduplicate_control_plane_evidence};
use serde_json::json;

#[test]
fn recovery_step_outcome_survives_compact_status() {
    let step = json!({
        "step": "regate", "intent_id": "intent-1", "job_id": "job-1",
        "state": "failed", "exit_code": 2,
        "failure": {"code": "agent_task.recovery_step_failed", "message": "free-space reserve"}
    });
    let compact = compact_control_plane_status(json!({
        "schema": "homeboy/control-plane-run/v1", "run": "run",
        "state": "candidate_recoverable", "recovery_step": step,
        "blocker": step["failure"],
    }));
    assert_eq!(compact["recovery_step"]["job_id"], "job-1");
    assert_eq!(compact["recovery_step"]["state"], "failed");
    assert_eq!(compact["blocker"]["message"], "free-space reserve");
}

/// An interrupted owner without a recorded stop reason still reports
/// truthfully whether a provider ever started.
#[test]
fn interrupted_owner_fallback_reports_zero_execution_before_provider_start() {
    crate::test_support::with_isolated_home(|_| {
        use homeboy::agents::agent_task_lifecycle::{
            exact_record, rewrite_record_for_test, seed_status_purity_fixtures_for_test,
        };
        seed_status_purity_fixtures_for_test().unwrap();
        let run_id = "purity-queued";
        for (consumed, expected) in [
            (
                0,
                "local Cook observer was interrupted before the provider started; no work was produced",
            ),
            (
                1,
                "local Cook observer was interrupted during provider execution",
            ),
        ] {
            rewrite_record_for_test(run_id, |record| {
                record.metadata["interrupted_owner"] =
                    json!({ "provider_executions_consumed": consumed });
            })
            .unwrap();
            let diagnostic =
                super::persisted_cook_failure_diagnostic(&exact_record(run_id).unwrap())
                    .expect("interrupted owner diagnostic");
            assert_eq!(diagnostic.class, "interrupted_owner");
            assert_eq!(diagnostic.message, expected);
        }
    });
}

#[test]
fn cleanup_warning_survives_status_and_diagnose_projection() {
    crate::test_support::with_isolated_home(|_| {
        use homeboy::agents::agent_task_lifecycle::{
            rewrite_record_for_test, seed_status_purity_fixtures_for_test,
        };
        seed_status_purity_fixtures_for_test().unwrap();
        let run_id = "purity-cook-recoverable-attempt";
        let warning = json!({
            "root_pid": 123, "process_group": 123,
            "failed_step": "process_tree_drain_and_reap"
        });
        rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_delivery"] = json!({
                "schema": "homeboy/cook-delivery/v2",
                "artifacts": {"promotion": {"deterministic_gates": [{
                    "id": "gate-1", "status": "succeeded", "cleanup_warning": warning
                }]}}
            });
        })
        .unwrap();
        let record = super::bounded_exact_record(run_id).unwrap();
        // Status and diagnose share this projection, including successful gates.
        let mut value = json!({});
        super::attach_gate_cleanup_warnings(&mut value, &record);
        assert_eq!(
            value["gate_cleanup_warnings"][0]["cleanup_warning"],
            warning
        );
        let compact = super::compact_control_plane_status(value);
        assert_eq!(
            compact["gate_cleanup_warnings"][0]["cleanup_warning"],
            warning
        );
    });
}

#[test]
fn terminal_cook_status_exposes_record_outcome_by_cook_and_run_id() {
    crate::test_support::with_isolated_home(|_| {
        use homeboy::agents::agent_task_lifecycle::{
            rewrite_record_for_test, seed_status_purity_fixtures_for_test,
        };
        seed_status_purity_fixtures_for_test().unwrap();
        let run_id = "purity-cook-recoverable-attempt";
        rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_delivery"] = json!({
                "schema": "homeboy/cook-delivery/v2",
                "delivery": {
                    "state": "published",
                    "publication": {
                        "status": "created", "published": true, "draft": true,
                        "pr_url": "https://github.com/o/r/pull/7"
                    }
                },
                "artifacts": {}
            });
        })
        .unwrap();
        for id in ["purity-cook-recoverable", run_id] {
            for full in [false, true] {
                let (data, _) = super::status(super::StatusArgs {
                    run_id: id.to_string(),
                    full,
                    exact: false,
                    strict_subject_exit: false,
                    watch: false,
                    interval: "5s".to_string(),
                    timeout: "30m".to_string(),
                })
                .unwrap();
                assert_eq!(
                    data["terminal_outcome"]["kind"], "published",
                    "{id}, full={full}"
                );
                // A published Cook leaves `recover` nothing to do (#18740).
                assert_eq!(data["recoverable"], false, "{id}, full={full}");
            }
        }
    });
}

/// #18271: a Cook's outcome is the gate verdict on its candidate, not the
/// later no-change retry that status follows for continuation.
#[test]
fn cook_outcome_is_the_selected_attempts_gate_verdict() {
    crate::test_support::with_isolated_home(|_| {
        homeboy::agents::agent_task_lifecycle::seed_status_purity_fixtures_for_test().unwrap();
        let status = |run_id: &str, exact| {
            super::status(super::StatusArgs {
                run_id: run_id.to_string(),
                full: false,
                exact,
                strict_subject_exit: false,
                watch: false,
                interval: "5s".to_string(),
                timeout: "30m".to_string(),
            })
            .unwrap()
            .0
        };
        let cook = status("purity-cook-gate-refused", false);
        assert_eq!(
            cook["terminal_outcome"],
            json!({"kind": "gate_refused", "gate": "gate-1"}),
            "{cook}"
        );
        let retry = status("purity-cook-gate-refused-retry", true);
        assert_eq!(retry["terminal_outcome"]["kind"], "no_change", "{retry}");
    });
}

#[test]
fn compact_status_names_the_bound_model_next_to_the_state() {
    let value = json!({
        "schema": "homeboy/control-plane-run/v1",
        "run": "retry-1",
        "state": "failed",
        "provider": {
            "id": "anthropic",
            "model": "airline/claude-sonnet-5"
        },
        "served_models": ["claude-sonnet-5", "gpt-6.1-sol"],
        "action_eligibility": { "actions": [] }
    });
    let compact = compact_control_plane_status(value);
    assert_eq!(
        compact["provider"]["model"],
        json!("airline/claude-sonnet-5")
    );
    assert_eq!(
        compact["served_models"],
        json!(["claude-sonnet-5", "gpt-6.1-sol"])
    );

    // No provider summary yet means no invented projection: the compact
    // output stays without a provider field (#15854).
    let value = json!({
        "schema": "homeboy/control-plane-run/v1",
        "run": "retry-1",
        "state": "pending",
        "action_eligibility": { "actions": [] }
    });
    let compact = compact_control_plane_status(value);
    assert!(compact.get("provider").is_none());
}

#[test]
fn compact_status_bounds_duplicate_evidence_and_uses_canonical_safe_action() {
    let duplicate_artifacts = (0..100)
        .map(|index| {
            json!({
                "id": format!("artifact-{}", index / 2),
                "kind": "patch",
                "uri": format!("homeboy://{}-{index}", "u".repeat(8 * 1024)),
                "content": "large evidence body",
            })
        })
        .collect::<Vec<_>>();
    let value = json!({
        "schema": "homeboy/control-plane-run/v1",
        "run": "retry-1",
        "state": "failed",
        "phase": "provider_start",
        "blocker": { "code": "runner.transport", "message": "upload failed" },
        "blocker_summary": format!("provider failed: {}", "x".repeat(8 * 1024)),
        "candidate": { "state": "unavailable" },
        "gates": (0..20).map(|index| json!({"id": format!("gate-{index}"), "state": "passed"})).collect::<Vec<_>>(),
        "action_eligibility": { "actions": [
            { "action": "retry", "availability": "unavailable", "reason": "not retryable" },
            { "action": "resume", "availability": "unavailable", "reason": "terminal" }
        ]},
        "artifacts": duplicate_artifacts,
        "evidence": [{"id":"e1","kind":"log","uri":"homeboy://e1"}]
    });
    let compact = compact_control_plane_status(value.clone());
    assert_eq!(compact["state"], "failed");
    assert_eq!(compact["phase"], "provider_start");
    assert_eq!(compact["blocker"]["code"], "runner.transport");
    assert_eq!(
        compact["blocker_summary"].as_str().unwrap().chars().count(),
        513
    );
    assert!(compact["blocker_summary"]
        .as_str()
        .unwrap()
        .starts_with("provider failed:"));
    assert!(compact["blocker_summary"].as_str().unwrap().ends_with('…'));
    assert!(serde_json::to_vec(&compact).unwrap().len() <= super::COMPACT_STATUS_BYTE_LIMIT);
    assert_eq!(compact["candidate"]["state"], "unavailable");
    assert_eq!(
        compact["gates"].as_array().unwrap().len(),
        super::COMPACT_REF_LIMIT
    );
    assert_eq!(compact["gates_omitted"], 20 - super::COMPACT_REF_LIMIT);
    assert_eq!(compact["artifacts_count"], 50);
    assert_eq!(compact["artifacts_refs"].as_array().unwrap().len(), 3);
    assert_eq!(compact["evidence_refs"].as_array().unwrap().len(), 1);
    assert_eq!(
        compact["next_action"]["command"],
        "homeboy runs evidence retry-1"
    );
    assert_eq!(compact["action_eligibility"]["available_action_count"], 0);
    assert_eq!(
        compact["action_eligibility"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(serde_json::to_vec(&compact).unwrap().len() <= super::COMPACT_STATUS_BYTE_LIMIT);

    let oversized = compact_control_plane_status(json!({
        "schema": "homeboy/control-plane-run/v1",
        "run": "durable-oversized-fixture",
        "state": "failed",
        "phase": "provider_start",
        "blocker": { "code": "provider.failed", "message": format!("cause: {}", "界".repeat(40_000)),
            "metadata": { "nested": (0..200).map(|index| json!({"k": index, "text": "private-data".repeat(80)})).collect::<Vec<_>>() } },
        "owner": { "kind": "controller", "id": "owner-1" },
        "gates": (0..200).map(|index| json!({"id": format!("gate-{index}"), "state": "passed", "metadata": "g".repeat(2_000)})).collect::<Vec<_>>(),
        "lab_staging_diagnostic": { "message": "diagnostic", "details": { "nested": "secret".repeat(4_000) } },
        "action_eligibility": { "actions": [] }
    }));
    let encoded = serde_json::to_vec(&oversized).unwrap();
    assert!(encoded.len() <= super::COMPACT_STATUS_BYTE_LIMIT);
    assert_eq!(oversized["run"], "durable-oversized-fixture");
    assert_eq!(oversized["state"], "failed");
    assert_eq!(oversized["phase"], "provider_start");
    assert_eq!(oversized["blocker"]["code"], "provider.failed");
    assert!(oversized
        .to_string()
        .contains("homeboy runs evidence durable-oversized-fixture"));

    let retryable = compact_control_plane_status(json!({
        "run": "retry-eligible",
        "state": "failed",
        "action_eligibility": { "actions": [
            { "action": "retry", "availability": "available", "required_inputs": [] },
            { "action": "promote", "availability": "unavailable", "required_inputs": ["to_worktree"] }
        ]}
    }));
    assert_eq!(
        retryable["next_action"]["command"],
        "homeboy agent-task retry retry-eligible"
    );
    assert_eq!(
        retryable["action_eligibility"]["actions"][0]["action"],
        "retry"
    );

    let needs_input = compact_control_plane_status(json!({
        "run": "candidate-run",
        "state": "candidate_recoverable",
        "action_eligibility": { "actions": [{
            "action": "promote",
            "availability": "available",
            "required_inputs": ["to_worktree"]
        }]}
    }));
    assert_eq!(
        needs_input["next_action"]["command"],
        "homeboy runs evidence candidate-run"
    );
    assert_eq!(
        needs_input["action_eligibility"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let mut full = value;
    deduplicate_control_plane_evidence(&mut full);
    assert_eq!(full["artifacts"].as_array().unwrap().len(), 100);
    assert_eq!(full["artifacts"][0]["content"], "large evidence body");
    assert_eq!(full["artifacts"][1]["uri"], duplicate_artifacts_uri(1));
    assert_eq!(
        full["action_eligibility"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        full["action_eligibility"]["actions"][0]["reason"],
        "not retryable"
    );
}

fn duplicate_artifacts_uri(index: usize) -> String {
    format!("homeboy://{}-{index}", "u".repeat(8 * 1024))
}
