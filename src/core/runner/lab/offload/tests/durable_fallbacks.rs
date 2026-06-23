use super::*;

#[test]
fn emit_durable_run_id_writes_json_to_output_before_execution() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output_path = dir.path().join("cook-output.json");
    let output_str = output_path.to_str().expect("utf8 path");
    let mut messages = Vec::new();

    emit_durable_run_id_before_execution(
        "agent-task-run-5684",
        "homeboy-lab",
        Some(output_str),
        &mut messages,
    );

    // The durable run id must be discoverable from --output immediately,
    // before the long-running provider execution that may exceed the local
    // shell timeout (#5684).
    let written = std::fs::read_to_string(&output_path).expect("read pre-written output");
    let json: serde_json::Value = serde_json::from_str(&written).expect("valid JSON envelope");
    assert_eq!(json["data"]["durable_run_id"], "agent-task-run-5684");
    assert_eq!(json["data"]["run_id"], "agent-task-run-5684");
    assert_eq!(json["data"]["status"], "dispatched_pending_execution");
    assert_eq!(
        json["data"]["retrieval_commands"]["status"],
        "homeboy agent-task status agent-task-run-5684"
    );
    assert_eq!(
        json["data"]["retrieval_commands"]["logs"],
        "homeboy agent-task logs agent-task-run-5684"
    );

    // The operator-facing messages must carry both follow-up commands.
    let joined = messages.join("\n");
    assert!(joined.contains("homeboy agent-task status agent-task-run-5684"));
    assert!(joined.contains("homeboy agent-task logs agent-task-run-5684"));
}

#[test]
fn emit_durable_run_id_without_output_still_records_followup_commands() {
    let mut messages = Vec::new();

    emit_durable_run_id_before_execution(
        "agent-task-run-no-output",
        "homeboy-lab",
        None,
        &mut messages,
    );

    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert!(message.contains("agent-task-run-no-output"));
    assert!(message.contains("homeboy agent-task status agent-task-run-no-output"));
    assert!(message.contains("homeboy agent-task logs agent-task-run-no-output"));
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
fn release_gate_component_not_found_adds_runner_registry_repair_hint() {
    let mut stderr = String::new();
    append_runner_component_registry_repair_hint(
        &mut stderr,
        &release_gate_lab_command("lint"),
        "homeboy-lab",
        "/home/lab/Developer/frontend-agent-chat",
        "",
        r#"{"success":false,"error":{"code":"component.not_found"}}"#,
    );

    assert!(stderr.contains("Lab runner registry repair"));
    assert!(stderr.contains(
        "homeboy runner exec homeboy-lab -- homeboy component create --local-path /home/lab/Developer/frontend-agent-chat"
    ));
    assert!(stderr.contains("homeboy runner exec homeboy-lab -- homeboy component list"));
    assert!(!stderr.contains("--force-hot"));
    assert!(!stderr.contains("--allow-local-hot"));
}

#[test]
fn non_release_gate_component_not_found_does_not_add_registry_repair_hint() {
    let mut stderr = String::new();
    append_runner_component_registry_repair_hint(
        &mut stderr,
        &portable_lab_command("bench"),
        "homeboy-lab",
        "/home/lab/Developer/frontend-agent-chat",
        "component.not_found",
        "",
    );

    assert!(stderr.is_empty());
}

#[test]
fn scoped_review_runner_rejection_includes_full_gate_fallbacks() {
    let args = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "homeboy".to_string(),
        "--changed-since".to_string(),
        "origin/main".to_string(),
        "--extension".to_string(),
        "rust".to_string(),
    ];

    let hints = unsupported_runner_hints("homeboy-lab", &args, "support".to_string());

    assert!(hints.iter().any(|hint| hint.contains(
        "`homeboy audit --runner homeboy-lab --extension rust homeboy`; `homeboy lint --runner homeboy-lab --extension rust homeboy`; `homeboy test --runner homeboy-lab --extension rust homeboy`"
    )));
}

#[test]
fn unscoped_review_runner_rejection_does_not_include_fallbacks() {
    let args = vec!["homeboy".to_string(), "review".to_string()];

    let hints = unsupported_runner_hints("homeboy-lab", &args, "support".to_string());

    assert_eq!(hints, vec!["support".to_string()]);
}
