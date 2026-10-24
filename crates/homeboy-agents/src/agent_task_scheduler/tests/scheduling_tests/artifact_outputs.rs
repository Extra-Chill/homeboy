//! Runtime artifact normalization and durable child-run attribution.

use super::shared::*;

#[test]
fn runtime_bundle_artifacts_materialize_required_typed_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let files = temp.path().join("runtime-123/files");
    fs::create_dir_all(&files).expect("runtime files");
    let patch_path = files.join("patch.diff");
    let transcript_path = files.join("transcript.json");
    fs::write(&patch_path, "diff --git a/a.txt b/a.txt\n").expect("patch");
    fs::write(&transcript_path, "{\"events\":[]}").expect("transcript");

    let scheduler = AgentTaskScheduler::new(Arc::new(RuntimeBundleOutcomeExecutor {
        patch_path: patch_path.clone(),
        transcript_path: transcript_path.clone(),
    }));
    let mut plan = plan_with_tasks(1);
    plan.tasks[0].expected_artifacts = vec![
        "patch".to_string(),
        "agent_result".to_string(),
        "transcript".to_string(),
    ];

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status(), AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.totals.succeeded, 1);
    let outcome = &aggregate.outcomes[0];
    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    for name in ["patch", "agent_result", "transcript"] {
        assert!(
            outcome
                .typed_artifacts
                .iter()
                .any(|artifact| artifact.name == name),
            "missing typed artifact {name}"
        );
    }
    let patch = outcome
        .typed_artifacts
        .iter()
        .find(|artifact| artifact.name == "patch")
        .expect("patch typed artifact");
    assert_eq!(
        patch
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.path.as_deref()),
        Some(patch_path.to_str().expect("patch path"))
    );
    assert!(outcome.diagnostics.iter().any(|diagnostic| {
        diagnostic.class == "agent_task.required_typed_artifacts_normalized"
    }));
}

#[test]
fn binds_artifacts_to_generic_child_run_ids_for_durable_fanout() {
    let scheduler = AgentTaskScheduler::new(Arc::new(GenericChildRunExecutor));
    let mut plan = AgentTaskPlan::new(
        "fuzz/campaign-1",
        vec![request("case-a"), request("case-b")],
    );
    plan.options.max_concurrency = 2;

    let aggregate = scheduler.run(plan);

    assert_eq!(aggregate.status(), AgentTaskAggregateStatus::Succeeded);
    assert_eq!(aggregate.child_runs.len(), 2);
    let mut child_runs = aggregate.child_runs.clone();
    child_runs.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    assert_eq!(child_runs[0].task_id, "case-a");
    assert_eq!(child_runs[0].run_id, "child-case-a");
    assert_eq!(child_runs[0].provider.as_deref(), Some("generic-fuzz"));
    assert_eq!(child_runs[0].state, AgentTaskState::Succeeded);
    assert_eq!(aggregate.artifact_bindings.len(), 2);
    let mut artifact_bindings = aggregate.artifact_bindings.clone();
    artifact_bindings.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    assert_eq!(artifact_bindings[0].task_id, "case-a");
    assert_eq!(artifact_bindings[0].run_id, "child-case-a");
    assert_eq!(artifact_bindings[0].artifact_id, "artifact-case-a");
    assert_eq!(artifact_bindings[0].kind, "fuzz-report");
    assert_eq!(
        artifact_bindings[0].path.as_deref(),
        Some("artifacts/case-a/report.json")
    );
}
