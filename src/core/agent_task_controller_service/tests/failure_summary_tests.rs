use super::super::*;
use super::*;

#[test]
fn run_failure_summary_normalizes_nested_provider_failure() {
    // Mirrors a real run-from-spec envelope: the root cause is buried under
    // results[*].failure_summary + a nested provider diagnostics array.
    let results = vec![serde_json::json!({
        "schema": ACTION_RESULT_SCHEMA,
        "status": "failed",
        "action_id": "spawn-impl",
        "failure_summary": {
            "action_id": "spawn-impl",
            "provider": "sample-runtime",
            "failure_phase": "plugin_activation",
            "run_id": "run-42",
            "diagnostic": "PHP fatal: Uncaught Error: Class 'Foo' not found",
        },
        "execution": {
            "runner_id": "homeboy-lab",
            "runner_job_id": "job-77",
            "diagnostics": [
                { "message": "PHP fatal: Uncaught Error: Class 'Foo' not found" }
            ],
            "artifacts": [
                { "kind": "log_bundle", "uri": "file:///runs/run-42/sandbox.log", "label": "sandbox log" }
            ],
        },
    })];
    let status = serde_json::json!({
        "controller": { "phase": "implement" },
    });

    let summary = build_run_failure_summary("loop-9", "action_failed", &results, &status);

    assert_eq!(summary.schema, CONTROLLER_RUN_FAILURE_SUMMARY_SCHEMA);
    assert_eq!(summary.stopped_reason, "action_failed");
    assert_eq!(summary.phase.as_deref(), Some("implement"));
    assert_eq!(summary.owner_surface, "selected_runtime");
    assert_eq!(
        summary.root_blocker,
        "PHP fatal: Uncaught Error: Class 'Foo' not found"
    );
    assert_eq!(summary.action_id.as_deref(), Some("spawn-impl"));
    assert_eq!(summary.provider.as_deref(), Some("sample-runtime"));
    assert_eq!(summary.failure_phase.as_deref(), Some("plugin_activation"));
    assert!(summary.next_command.contains("loop-9"));
    assert_homeboy_command_parses(&summary.next_command);

    // Durable evidence refs: persisted run evidence, runner job log, per-run
    // evidence, and the declared provider artifact bundle.
    let kinds: Vec<&str> = summary
        .evidence_refs
        .iter()
        .map(|reference| reference.kind.as_str())
        .collect();
    assert!(kinds.contains(&"runner_job_log"), "kinds={kinds:?}");
    assert!(kinds.contains(&"run_evidence"), "kinds={kinds:?}");
    assert!(kinds.contains(&"artifact_bundle"), "kinds={kinds:?}");
    assert!(summary
        .evidence_refs
        .iter()
        .any(|reference| reference.uri == "homeboy runner job logs homeboy-lab job-77"));
    assert!(summary
        .evidence_refs
        .iter()
        .any(|reference| reference.uri == "file:///runs/run-42/sandbox.log"));
    assert_emitted_homeboy_evidence_commands_parse(&summary);
}

#[test]
fn run_failure_summary_handles_runner_block_without_diagnostic_message() {
    let results = vec![serde_json::json!({
        "schema": ACTION_RESULT_SCHEMA,
        "status": "blocked_runner_unavailable",
        "action_id": "gate-run",
        "failure_summary": {
            "action_id": "gate-run",
            "diagnostic": "runner `lab-1` is not available for controller action execution",
        },
    })];
    let status = serde_json::json!({ "controller": { "phase": "verify" } });

    let summary = build_run_failure_summary("loop-7", "action_failed", &results, &status);

    assert_eq!(summary.owner_surface, "lab_runner");
    assert!(summary.root_blocker.contains("runner"));
    assert!(summary.next_command.contains("--resume"));
    assert_homeboy_command_parses(&summary.next_command);
    // Still always surfaces the persisted run-evidence ref.
    assert!(summary
        .evidence_refs
        .iter()
        .any(|reference| reference.kind == "run_evidence"));
    assert_emitted_homeboy_evidence_commands_parse(&summary);
}

#[test]
fn run_failure_summary_falls_back_to_stopped_reason() {
    // No per-action failure_summary and no diagnostics: the summary still names
    // a sensible blocker derived from the stopped reason.
    let results: Vec<Value> = Vec::new();
    let status = serde_json::json!({ "phase": "plan" });

    let summary = build_run_failure_summary("loop-3", "max_actions_reached", &results, &status);

    assert_eq!(summary.stopped_reason, "max_actions_reached");
    assert_eq!(summary.owner_surface, "homeboy");
    assert!(summary.root_blocker.contains("max-actions"));
    assert_eq!(summary.phase.as_deref(), Some("plan"));
    assert!(!summary.evidence_refs.is_empty());
    assert_homeboy_command_parses(&summary.next_command);
    assert_emitted_homeboy_evidence_commands_parse(&summary);
}
