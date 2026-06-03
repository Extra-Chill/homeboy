use super::super::lab_selection::{
    prepare_lab_runner_for_offload_with, LabRunnerPreparation, LabRunnerSelection,
};
use super::*;
use crate::core::runner::{RunnerConnectReport, RunnerStatusReport, RunnerTunnelMode};

#[test]
fn lab_runner_preparation_falls_back_for_unreachable_default_runner() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: false,
                state: super::super::RunnerSessionState::Disconnected,
                session: None,
                stale_daemon: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |runner_id| {
            Ok((
                RunnerConnectReport {
                    runner_id: runner_id.to_string(),
                    mode: None,
                    role: None,
                    connected: false,
                    recorded: None,
                    local_url: None,
                    broker_url: None,
                    controller_id: None,
                    remote_daemon_address: None,
                    tunnel_pid: None,
                    remote_daemon_pid: None,
                    homeboy_version: None,
                    session_path: Some("/tmp/lab.json".to_string()),
                    failure_kind: Some(super::super::RunnerFailureKind::SshFailure),
                    failure_message: Some("SSH connectivity check failed".to_string()),
                },
                20,
            ))
        },
    )
    .expect("prepared");

    assert_eq!(
        prepared,
        LabRunnerPreparation::FallBackLocal {
            reason: "SSH connectivity check failed".to_string()
        }
    );
}

#[test]
fn lab_runner_preparation_uses_already_connected_runner() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: true,
                state: super::super::RunnerSessionState::Connected,
                session: Some(connected_direct_session(
                    runner_id,
                    Some("http://127.0.0.1:1234"),
                )),
                stale_daemon: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("connected runner should not reconnect"),
    )
    .expect("prepared");

    assert_eq!(prepared, LabRunnerPreparation::Ready);
}

#[test]
fn lab_runner_preparation_falls_back_for_stale_default_direct_session_without_daemon_url() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: true,
                state: super::super::RunnerSessionState::Connected,
                session: Some(connected_direct_session(runner_id, None)),
                stale_daemon: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("stale connected session should not reconnect during automatic preflight"),
    )
    .expect("prepared");

    assert_eq!(
        prepared,
        LabRunnerPreparation::FallBackLocal {
            reason: "direct SSH runner `lab` has no local daemon URL; reconnect it with `homeboy runner connect lab`".to_string()
        }
    );
}

#[test]
fn lab_runner_preparation_errors_for_explicit_direct_session_without_daemon_url() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let err = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: true,
                state: super::super::RunnerSessionState::Connected,
                session: Some(connected_direct_session(runner_id, None)),
                stale_daemon: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("stale connected session should fail before reconnect"),
    )
    .expect_err("explicit stale session should error");

    assert!(err.message.contains("connected but is not ready"));
    assert!(err.message.contains("no local daemon URL"));
    assert!(err
        .details
        .get("tried")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|suggestion| suggestion
            .as_str()
            .is_some_and(|value| value.contains("homeboy runner connect lab"))));
}

#[test]
fn lab_runner_preparation_connects_disconnected_runner() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: false,
                state: super::super::RunnerSessionState::Disconnected,
                session: None,
                stale_daemon: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |runner_id| {
            Ok((
                RunnerConnectReport {
                    runner_id: runner_id.to_string(),
                    mode: Some(RunnerTunnelMode::DirectSsh),
                    role: Some(super::super::RunnerSessionRole::Controller),
                    connected: true,
                    recorded: None,
                    local_url: Some("http://127.0.0.1:1234".to_string()),
                    broker_url: None,
                    controller_id: None,
                    remote_daemon_address: Some("127.0.0.1:5678".to_string()),
                    tunnel_pid: None,
                    remote_daemon_pid: Some(42),
                    homeboy_version: Some("homeboy 0.0.0".to_string()),
                    session_path: Some("/tmp/lab.json".to_string()),
                    failure_kind: None,
                    failure_message: None,
                },
                0,
            ))
        },
    )
    .expect("prepared");

    assert_eq!(prepared, LabRunnerPreparation::Ready);
}

#[test]
fn lab_runner_preparation_errors_for_unreachable_explicit_runner() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let err = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(RunnerStatusReport {
                runner_id: runner_id.to_string(),
                connected: false,
                state: super::super::RunnerSessionState::Disconnected,
                session: None,
                stale_daemon: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |runner_id| {
            Ok((
                RunnerConnectReport {
                    runner_id: runner_id.to_string(),
                    mode: None,
                    role: None,
                    connected: false,
                    recorded: None,
                    local_url: None,
                    broker_url: None,
                    controller_id: None,
                    remote_daemon_address: None,
                    tunnel_pid: None,
                    remote_daemon_pid: None,
                    homeboy_version: None,
                    session_path: Some("/tmp/lab.json".to_string()),
                    failure_kind: Some(super::super::RunnerFailureKind::SshFailure),
                    failure_message: Some("SSH connectivity check failed".to_string()),
                },
                20,
            ))
        },
    )
    .expect_err("explicit runner should error");

    assert!(err.message.contains("could not connect runner"));
}

fn connected_direct_session(
    runner_id: &str,
    local_url: Option<&str>,
) -> super::super::RunnerSession {
    super::super::RunnerSession {
        runner_id: runner_id.to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: super::super::RunnerSessionRole::Controller,
        server_id: Some(runner_id.to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:5678".to_string()),
        local_port: Some(1234),
        local_url: local_url.map(str::to_string),
        tunnel_pid: None,
        remote_daemon_pid: Some(42),
        homeboy_version: "homeboy 0.0.0".to_string(),
        connected_at: "2026-06-03T00:00:00Z".to_string(),
    }
}
