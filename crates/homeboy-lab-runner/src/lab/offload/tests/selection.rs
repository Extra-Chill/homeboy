use super::*;
use crate::lab_selection::allows_detached_reverse_capacity_queue;

fn select(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    placement: homeboy_cli_contract::Placement,
    deny_local_bench: bool,
    release_gate_local_allowed: bool,
    default_runner: Option<String>,
) -> Result<Option<LabRunnerSelection>> {
    resolve_lab_runner_selection_from_placement(
        command,
        explicit_runner,
        placement,
        deny_local_bench,
        release_gate_local_allowed,
        default_runner,
    )
}

#[test]
fn explicit_runner_selects_lab_with_auto_placement() {
    let selection = select(
        &portable_lab_command("test"),
        Some("lab-explicit"),
        homeboy_cli_contract::Placement::Auto,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("explicit runner selected");

    assert_eq!(selection.runner_id, "lab-explicit");
    assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
}

#[test]
fn auto_uses_the_ready_default_runner_for_supported_commands() {
    let selection = select(
        &portable_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::Auto,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("default runner selected");

    assert_eq!(selection.runner_id, "lab-default");
    assert_eq!(selection.source, LabRunnerSelectionSource::Default);
}

#[test]
fn local_placement_skips_default_runner_selection() {
    assert!(select(
        &portable_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::Local,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .is_none());
}

#[test]
fn lab_placement_requires_a_portable_command_and_runner() {
    let error = select(
        &portable_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::Lab,
        false,
        false,
        None,
    )
    .expect_err("Lab placement must fail closed without a runner");

    assert_eq!(error.code.as_str(), "validation.invalid_argument");
    assert!(error.message.contains("--placement lab requires"));
}

#[test]
fn lab_or_local_prefers_a_default_runner_without_requiring_one() {
    let selection = select(
        &portable_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::LabOrLocal,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("default runner selected");
    assert_eq!(selection.runner_id, "lab-default");

    assert!(select(
        &portable_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::LabOrLocal,
        false,
        false,
        None,
    )
    .expect("no default runner allows local fallback")
    .is_none());
}

#[test]
fn local_placement_obeys_bench_and_release_gates() {
    let bench_error = select(
        &portable_lab_command("bench"),
        None,
        homeboy_cli_contract::Placement::Local,
        true,
        false,
        None,
    )
    .expect_err("bench policy blocks local placement");
    assert!(bench_error.message.contains("/bench/local_execution"));

    let release_error = select(
        &release_gate_lab_command("lint"),
        None,
        homeboy_cli_contract::Placement::Local,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect_err("release gate blocks local placement");
    assert!(release_error.message.contains("Release gate `lint`"));
}

#[test]
fn release_gate_can_explicitly_allow_local_placement() {
    assert!(select(
        &release_gate_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::Local,
        false,
        true,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .is_none());
}

#[test]
fn busy_default_runner_allows_normal_auto_work_to_stay_local() {
    let command = portable_lab_command("test");
    assert!(select(
        &command,
        None,
        homeboy_cli_contract::Placement::Auto,
        false,
        false,
        None,
    )
    .expect("no available default runner leaves ordinary auto work local")
    .is_none());

    let busy = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        true,
        false,
        1,
        &RunnerActiveJobState::Available,
        Some(1),
    );
    assert!(fail_if_no_default_runner_accepts_jobs_with(&command, vec![busy]).is_ok());
}

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
    let status = stale_reverse_status("homeboy-lab");
    let availability = RunnerAvailability::from_status_parts(
        status.runner_id.clone(),
        status.connected,
        status.stale_daemon.is_some(),
        status.active_jobs.len(),
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
    let refresh = status
        .stale_daemon
        .as_ref()
        .expect("stale daemon warning")
        .refresh_command
        .clone();

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
    assert_eq!(error.details["stale_daemon_recovery_command"], refresh);
    assert!(error.details["tried"][0]
        .as_str()
        .expect("recovery hint")
        .contains(&refresh));
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

#[test]
fn explicit_lab_never_allows_missing_or_busy_default_runner_to_run_local() {
    let error = select(
        &portable_lab_command("test"),
        None,
        homeboy_cli_contract::Placement::Lab,
        false,
        false,
        None,
    )
    .expect_err("explicit Lab placement fails closed");

    assert!(error.message.contains("--placement lab requires"));
}

#[test]
fn capacity_queue_admission_requires_detached_durable_reverse_capacity_only() {
    let reverse = LabRunnerSelection {
        runner_id: "homeboy-lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::Reverse,
    };
    let direct = LabRunnerSelection {
        mode: RunnerTunnelMode::DirectSsh,
        ..reverse.clone()
    };
    let capacity = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        true,
        false,
        1,
        &RunnerActiveJobState::Available,
        Some(1),
    );
    let disconnected = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        false,
        false,
        1,
        &RunnerActiveJobState::Available,
        Some(1),
    );
    let stale = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        true,
        true,
        1,
        &RunnerActiveJobState::Available,
        Some(1),
    );
    let unknown = RunnerAvailability::from_status_parts(
        "homeboy-lab",
        true,
        false,
        1,
        &RunnerActiveJobState::Unavailable,
        None,
    );

    assert!(allows_detached_reverse_capacity_queue(
        true, true, &reverse, &capacity
    ));
    assert!(!allows_detached_reverse_capacity_queue(
        false, true, &reverse, &capacity
    ));
    assert!(!allows_detached_reverse_capacity_queue(
        true, false, &reverse, &capacity
    ));
    assert!(!allows_detached_reverse_capacity_queue(
        true, true, &direct, &capacity
    ));
    assert!(!allows_detached_reverse_capacity_queue(
        true,
        true,
        &reverse,
        &disconnected
    ));
    assert!(!allows_detached_reverse_capacity_queue(
        true, true, &reverse, &stale
    ));
    assert!(!allows_detached_reverse_capacity_queue(
        true, true, &reverse, &unknown
    ));
}
