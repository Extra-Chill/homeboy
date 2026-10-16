use super::*;

#[test]
fn busy_default_runner_fails_closed_for_release_gate() {
    let busy = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        true,
        false,
        1,
        &RunnerActiveJobState::Available,
        Some(1),
    );

    let err = fail_if_no_default_runner_accepts_jobs_with(
        &release_gate_lab_command("review"),
        vec![busy],
    )
    .expect_err("release gates must not become local when Lab is full");

    assert!(err.message.contains("none can accept jobs"));
    assert_eq!(
        err.details["runner_availability"]["reasons"][0],
        "capacity_reached"
    );
}

#[test]
fn explicit_connected_stale_runner_preserves_drift_diagnosis_and_recovery() {
    let status = stale_connected_status("homeboy-lab");
    let availability = RunnerAvailability::from_status_parts(
        status.runner_id.clone(),
        status.connected,
        status.stale_daemon.is_some(),
        status.active_job_count,
        &status.active_job_state,
        Some(1),
    );

    assert!(availability.connected);
    assert!(!availability.accepts_jobs);
    assert_eq!(availability.reasons, ["stale_daemon"]);

    let error = lab_runner_availability_error(
        "agent-task cook",
        Some(&availability),
        Some(&status),
        vec![availability.clone()],
    );
    assert!(error.message.contains("cannot accept jobs"));
    assert_eq!(
        error.details["runner_availability"]["reasons"],
        serde_json::json!(["stale_daemon"])
    );
    assert_eq!(error.details["runner_status"]["connected"], true);
    assert_eq!(
        error.details["runner_status"]["stale_daemon"]["active_daemon_control_plane_version"],
        "homeboy 0.228.0"
    );
    assert_eq!(
        error.details["runner_status"]["stale_daemon"]["job_command_binary_version"],
        "homeboy 0.229.11"
    );
    assert_eq!(
        error.details["stale_daemon_recovery_command"],
        serde_json::Value::Null
    );
    assert!(error.details["runner_status"]["stale_daemon"]
        .get("refresh_command")
        .is_none());
    assert!(error.details["tried"][0]
        .as_str()
        .expect("recovery hint")
        .contains("Wait for an active Lab runner job"));
}

#[test]
fn persisted_session_drift_rejects_the_real_availability_and_preflight_path() {
    let mut status = connected_status("homeboy-lab");
    status.stale_daemon = Some(
        RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "0.328.0".to_string(),
            "0.328.0".to_string(),
            Some("homeboy 0.328.0+1e63f1ae0369".to_string()),
            Some("homeboy 0.328.0+1e63f1ae0369".to_string()),
        )
        .with_persisted_session_version("homeboy-lab", "0.327.9".to_string()),
    );
    let availability = RunnerAvailability::from_status_parts(
        status.runner_id.clone(),
        status.connected,
        status.stale_daemon.is_some(),
        status.active_jobs.len(),
        &status.active_job_state,
        Some(1),
    );

    let error = lab_runner_availability_error(
        "agent-task cook",
        Some(&availability),
        Some(&status),
        vec![availability.clone()],
    );

    assert!(!availability.accepts_jobs);
    assert_eq!(
        error.details["runner_status"]["stale_daemon"]["mismatch_predicate"],
        "session_homeboy_version != job_command_binary_version"
    );
    assert_eq!(
        error.details["stale_daemon_recovery_command"],
        "homeboy runner refresh-homeboy homeboy-lab --ref 1e63f1ae0369 --reconnect"
    );
}

#[test]
fn disconnected_runner_keeps_existing_availability_diagnosis() {
    let availability = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        false,
        false,
        0,
        &RunnerActiveJobState::Available,
        Some(1),
    );

    let error = lab_runner_availability_error(
        "agent-task cook",
        Some(&availability),
        None,
        vec![availability.clone()],
    );

    assert_eq!(
        error.details["runner_availability"]["reasons"],
        serde_json::json!(["not_connected"])
    );
    assert!(error.details["runner_status"].is_null());
    assert!(error.details["stale_daemon_recovery_command"].is_null());
}
