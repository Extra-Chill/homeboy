use super::*;

#[test]
fn deferred_staging_emits_only_durable_run_commands() {
    const RUN: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";
    let mission = homeboy_control_plane_contract::resolve(
        homeboy_control_plane_contract::IdentityKind::RunId,
        RUN,
    )
    .expect("run")
    .mission
    .expect("mission");
    let receipt = crate::controller_fallback_projection::DeferredControllerReceipt {
        schema: "homeboy/controller-fallback-projection/v3".to_string(),
        mission_id: mission,
        runner_receipt: crate::runner_staging_operation::RemoteRunnerStagingReceipt {
            schema: "homeboy/remote-runner-staging-receipt/v1".to_string(),
            handoff: crate::direct_lab_handoff::DirectLabHandoffReceipt {
                schema: "homeboy/direct-lab-handoff-receipt/v1".to_string(),
                run_id: "run-1".to_string(),
                runner_id: "runner-1".to_string(),
                runner_job_id: "00000000-0000-0000-0000-000000000001".to_string(),
                idempotency_key: "run-1".to_string(),
                controller_identity: "controller-1".to_string(),
                acceptance_state: "accepted".to_string(),
                controller_projection: "deferred".to_string(),
                status_command: "homeboy agent-task status run-1".to_string(),
                cancel_command: "homeboy agent-task cancel run-1".to_string(),
                evidence_command: "homeboy runs evidence run-1 --full".to_string(),
            },
            artifacts: crate::runner_staging_operation::RunnerStagingArtifacts {
                lifecycle_id: "staging-run-1".to_string(),
                source_artifact_id: "source-1".to_string(),
                workspace_artifact_id: "workspace-1".to_string(),
                source_artifact: Some(
                    crate::runner_staging_operation::SourceArtifactTransfer::from_bytes(
                        "source-run-1",
                        b"sealed source package",
                    )
                    .descriptor(),
                ),
            },
        },
        controller_projection: "deferred".to_string(),
    };
    let output = detached_staging_submission_output(
        crate::lab_staging_controller::DetachedStagingSubmission::Deferred(Box::new(receipt)),
        "run-1",
    );

    assert_eq!(output["status"], "staged");
    assert_eq!(
        output["runner_job_id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        output["runner_staging_id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(output.get("controller_job_id").is_none());
    assert!(output["retrieval_commands"].get("controller_job").is_none());
    assert_eq!(
        output["retrieval_commands"]["status"],
        "homeboy agent-task status run-1"
    );
    assert_eq!(
        output["retrieval_commands"]["logs"],
        "homeboy runs logs run-1"
    );
}

#[test]
fn emit_durable_run_id_before_execution_publishes_followup_commands() {
    // The durable run id must be discoverable from --output immediately,
    // before the long-running provider execution that may exceed the local
    // shell timeout (#5684).
    let dir = tempfile::tempdir().expect("temp dir");
    let output_path = dir.path().join("cook-output.json");
    let output_str = output_path.to_str().expect("utf8 path");

    for output in [Some(output_str), None] {
        let mut messages = Vec::new();
        emit_durable_run_id_before_execution(
            "agent-task-run-5684",
            "homeboy-lab",
            output,
            &mut messages,
        );

        let joined = messages.join("\n");
        assert!(joined.contains("homeboy agent-task status agent-task-run-5684"));
        assert!(joined.contains("homeboy runs logs agent-task-run-5684"));
    }

    let written = std::fs::read_to_string(&output_path).expect("read pre-written output");
    let json: serde_json::Value = serde_json::from_str(&written).expect("valid JSON envelope");
    assert_eq!(json["data"]["durable_run_id"], "agent-task-run-5684");
    assert_eq!(json["data"]["status"], "dispatched_pending_execution");
    assert_eq!(
        json["data"]["retrieval_commands"]["status"],
        "homeboy agent-task status agent-task-run-5684"
    );
}

#[test]
fn write_local_output_file_atomically_replaces_and_appends_newline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output_path = dir.path().join("run.json");
    std::fs::write(&output_path, "stale").expect("seed output");

    write_local_output_file_atomically(
        output_path.to_str().expect("utf8 path"),
        "{\"durable_run_id\":\"run-123\"}",
    )
    .expect("atomic write");

    let written = std::fs::read_to_string(&output_path).expect("read output");
    assert_eq!(written, "{\"durable_run_id\":\"run-123\"}\n");
    // No leftover temp file should remain after the atomic rename.
    assert!(std::fs::read_dir(dir.path())
        .expect("read dir")
        .all(|entry| !entry
            .expect("dir entry")
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
}

#[test]
fn component_not_found_adds_runner_registry_repair_hint_only_for_release_gates() {
    for (command, stdout, command_stderr, expect_hint) in [
        (
            release_gate_lab_command("lint"),
            "",
            r#"{"success":false,"error":{"code":"component.not_found"}}"#,
            true,
        ),
        (
            portable_lab_command("bench"),
            "component.not_found",
            "",
            false,
        ),
    ] {
        let mut stderr = String::new();
        append_runner_component_registry_repair_hint(
            &mut stderr,
            &command,
            "homeboy-lab",
            "/home/lab/Developer/frontend-agent-chat",
            stdout,
            command_stderr,
        );

        if expect_hint {
            assert!(stderr.contains(
                "homeboy runner exec homeboy-lab -- homeboy component create --local-path /home/lab/Developer/frontend-agent-chat"
            ));
        } else {
            assert!(stderr.is_empty());
        }
    }
}

#[test]
fn review_runner_rejection_includes_full_gate_fallbacks_only_when_scoped() {
    let scoped = [
        "homeboy",
        "review",
        "homeboy",
        "--changed-since",
        "origin/main",
        "--extension",
        "rust",
    ]
    .map(str::to_string)
    .to_vec();
    let hints = unsupported_runner_hints("homeboy-lab", &scoped, "support".to_string());
    assert!(hints.iter().any(|hint| hint.contains(
        "`homeboy review lint --runner homeboy-lab --extension rust homeboy`; `homeboy review test --runner homeboy-lab --extension rust homeboy`"
    )));

    let unscoped = ["homeboy", "review"].map(str::to_string).to_vec();
    let hints = unsupported_runner_hints("homeboy-lab", &unscoped, "support".to_string());
    assert_eq!(hints, vec!["support".to_string()]);
}
