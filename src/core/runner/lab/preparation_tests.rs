use super::super::lab_selection::{
    prepare_lab_runner_for_offload_with, LabRunnerPreparation, LabRunnerSelection,
};
use super::*;
use crate::core::runner::{
    RunnerActiveJobState, RunnerConnectReport, RunnerStatusReport, RunnerTunnelMode,
};

use super::super::session::{RunnerStaleDaemonWarning, RunnerStaleRuntimePath};

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
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
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
                    homeboy_build_identity: None,
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
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("connected runner should not reconnect"),
    )
    .expect("prepared");

    assert_eq!(prepared, LabRunnerPreparation::Ready);
}

#[test]
fn lab_runner_preparation_refreshes_stale_default_daemon_version() {
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
                stale_daemon: Some(stale_daemon_warning(runner_id)),
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |runner_id| Ok((successful_connect_report(runner_id), 0)),
    )
    .expect("prepared");

    assert_eq!(prepared, LabRunnerPreparation::Ready);
}

#[test]
fn lab_runner_preparation_falls_back_for_stale_default_runtime_paths() {
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
                stale_daemon: Some(stale_runtime_path_warning(runner_id)),
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("stale runtime daemon should not dispatch or reconnect automatically"),
    )
    .expect("prepared");

    assert_eq!(
        prepared,
        LabRunnerPreparation::FallBackLocal {
            reason: "connected runner `lab` daemon runtime is stale after runner-side rebuilds or path changes; restart the active daemon with `homeboy runner disconnect lab && homeboy runner connect lab`".to_string()
        }
    );
}

#[test]
fn lab_runner_preparation_errors_for_explicit_stale_daemon_version() {
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
                session: Some(connected_direct_session(
                    runner_id,
                    Some("http://127.0.0.1:1234"),
                )),
                stale_daemon: Some(stale_daemon_warning(runner_id)),
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |runner_id| Ok((failed_connect_report(runner_id, "daemon restart failed"), 1)),
    )
    .expect_err("explicit stale daemon refresh failure should error");

    assert!(err
        .message
        .contains("stale daemon and automatic refresh failed"));
    assert!(err
        .message
        .contains("automatic refresh failed: daemon restart failed"));
    assert!(err.message.contains("daemon is stale"));
    assert!(err.message.contains("homeboy 0.218.0"));
    assert!(err.message.contains("homeboy 0.219.0"));
    assert!(err
        .message
        .contains("homeboy runner disconnect lab && homeboy runner connect lab"));
    assert!(err
        .message
        .contains("malformed or misleading provider output"));
    assert!(err
        .details
        .get("tried")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|suggestion| suggestion
            .as_str()
            .is_some_and(|value| value
                .contains("homeboy runner disconnect lab && homeboy runner connect lab"))));
}

#[test]
fn lab_runner_preparation_refreshes_stale_explicit_daemon_version() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
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
                stale_daemon: Some(stale_daemon_warning(runner_id)),
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |runner_id| Ok((successful_connect_report(runner_id), 0)),
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
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
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
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
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
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
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
                    homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
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
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_jobs: Vec::new(),
                stale_runner_job_count: 0,
                active_job_state: RunnerActiveJobState::NotQueried,
                active_job_source: None,
                active_job_error: None,
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
                    homeboy_build_identity: None,
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
        homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
        connected_at: "2026-06-03T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
    }
}

fn successful_connect_report(runner_id: &str) -> RunnerConnectReport {
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
        homeboy_version: Some("homeboy 0.219.0".to_string()),
        homeboy_build_identity: Some("homeboy 0.219.0+new".to_string()),
        session_path: Some("/tmp/lab.json".to_string()),
        failure_kind: None,
        failure_message: None,
    }
}

fn failed_connect_report(runner_id: &str, failure_message: &str) -> RunnerConnectReport {
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
        homeboy_build_identity: None,
        session_path: Some("/tmp/lab.json".to_string()),
        failure_kind: Some(super::super::RunnerFailureKind::DaemonStartupFailure),
        failure_message: Some(failure_message.to_string()),
    }
}

fn stale_daemon_warning(runner_id: &str) -> RunnerStaleDaemonWarning {
    RunnerStaleDaemonWarning::new(
        runner_id,
        "homeboy 0.218.0".to_string(),
        "homeboy 0.219.0".to_string(),
        Some("homeboy 0.218.0+old".to_string()),
        Some("homeboy 0.219.0+new".to_string()),
    )
}

fn stale_runtime_path_warning(runner_id: &str) -> RunnerStaleDaemonWarning {
    RunnerStaleDaemonWarning::new(
        runner_id,
        "homeboy 0.219.0".to_string(),
        "homeboy 0.219.0".to_string(),
        Some("homeboy 0.219.0+same".to_string()),
        Some("homeboy 0.219.0+same".to_string()),
    )
    .with_runtime_paths(
        runner_id,
        vec![RunnerStaleRuntimePath {
            env: "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
            path: "/home/chubes/Developer/sample-runtime".to_string(),
            loaded_fingerprint: "files=10".to_string(),
            current_fingerprint: "files=11".to_string(),
        }],
        Vec::new(),
    )
}
