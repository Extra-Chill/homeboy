use super::super::lab_selection::{
    prepare_lab_runner_for_offload_with, wait_for_connected_runner, LabRunnerPreparation,
    LabRunnerSelection,
};
use super::*;
use crate::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use crate::runner::{
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
                daemon_freshness: None,
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
                    connection_warning: None,
                    homeboy_version: None,
                    homeboy_build_identity: None,
                    session_path: Some("/tmp/lab.json".to_string()),
                    leaseless_recovery: None,
                    state_loss_recovery: None,
                    leaseless_recovery_evidence: None,
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
                stale_daemon: None,
                daemon_freshness: None,
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
fn lab_runner_preparation_falls_back_for_stale_default_daemon_version_without_reconnecting() {
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
                daemon_freshness: Some(restartable_daemon_freshness()),
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
        |_| panic!("stale daemon drift must not rotate a shared tunnel during handoff"),
    )
    .expect("prepared");

    assert!(matches!(
        prepared,
        LabRunnerPreparation::FallBackLocal { .. }
    ));
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
                daemon_freshness: None,
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
                daemon_freshness: Some(restartable_daemon_freshness()),
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
        |_| panic!("stale daemon drift must not reconnect during handoff"),
    )
    .expect_err("explicit stale daemon should require an explicit refresh");

    assert!(err.message.contains("connected but is not ready"));
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
fn concurrent_stale_handoffs_preserve_the_shared_tunnel() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::DirectSsh,
    };
    let barrier = Arc::new(Barrier::new(5));
    let reconnects = Arc::new(AtomicUsize::new(0));
    let handoffs: Vec<_> = (0..5)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let reconnects = Arc::clone(&reconnects);
            let selection = selection.clone();
            std::thread::spawn(move || {
                barrier.wait();
                prepare_lab_runner_for_offload_with(
                    &selection,
                    |runner_id| {
                        Ok(RunnerStatusReport {
                            runner_id: runner_id.to_string(),
                            connected: true,
                            state: super::super::RunnerSessionState::Connected,
                            session: Some(connected_direct_session(
                                runner_id,
                                Some("http://127.0.0.1:63378"),
                            )),
                            stale_daemon: Some(stale_daemon_warning(runner_id)),
                            daemon_freshness: Some(restartable_daemon_freshness()),
                            active_jobs: Vec::new(),
                            active_runner_jobs: Vec::new(),
                            active_job_count: 0,
                            stale_runner_jobs: Vec::new(),
                            stale_runner_job_count: 0,
                            active_job_state: RunnerActiveJobState::Available,
                            active_job_source: None,
                            active_job_error: None,
                            session_path: "/tmp/lab.json".to_string(),
                        })
                    },
                    |_| {
                        reconnects.fetch_add(1, Ordering::SeqCst);
                        unreachable!("stale handoff must not reconnect")
                    },
                )
            })
        })
        .collect();

    for handoff in handoffs {
        assert!(matches!(
            handoff.join().expect("handoff thread"),
            Ok(LabRunnerPreparation::FallBackLocal { .. })
        ));
    }
    assert_eq!(reconnects.load(Ordering::SeqCst), 0);
}

#[test]
fn concurrent_unreachable_health_handoffs_connect_once() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Barrier,
    };

    let selection = LabRunnerSelection {
        runner_id: "lab-unreachable-health".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };
    let barrier = Arc::new(Barrier::new(5));
    let connected = Arc::new(AtomicBool::new(false));
    let connects = Arc::new(AtomicUsize::new(0));
    let handoffs: Vec<_> = (0..5)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let connected = Arc::clone(&connected);
            let connects = Arc::clone(&connects);
            let selection = selection.clone();
            std::thread::spawn(move || {
                barrier.wait();
                prepare_lab_runner_for_offload_with(
                    &selection,
                    |runner_id| {
                        Ok(unreachable_health_status(
                            runner_id,
                            connected.load(Ordering::SeqCst),
                        ))
                    },
                    |runner_id| {
                        connects.fetch_add(1, Ordering::SeqCst);
                        connected.store(true, Ordering::SeqCst);
                        Ok((connected_direct_connect_report(runner_id), 0))
                    },
                )
            })
        })
        .collect();

    for handoff in handoffs {
        assert_eq!(
            handoff.join().expect("handoff thread").expect("handoff"),
            LabRunnerPreparation::Ready
        );
    }
    assert_eq!(connects.load(Ordering::SeqCst), 1);
}

#[test]
fn lease_contender_waits_for_owner_session_without_a_second_connect() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    let connected = Arc::new(AtomicBool::new(false));
    let connects = Arc::new(AtomicUsize::new(0));
    let owner_connected = Arc::clone(&connected);
    let owner_connects = Arc::clone(&connects);
    let owner = std::thread::spawn(move || {
        owner_connects.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(75));
        owner_connected.store(true, Ordering::SeqCst);
    });

    // This is the contender branch after RuntimePromotionLease::acquire reports
    // another process owns the reconnect transaction.
    let report = wait_for_connected_runner(std::time::Duration::from_secs(1), || {
        Ok(unreachable_health_status(
            "lab-lease-contention",
            connected.load(Ordering::SeqCst),
        ))
    })
    .expect("wait succeeds")
    .expect("owner publishes a healthy session");

    owner.join().expect("owner");
    assert!(report.connected);
    assert_eq!(connects.load(Ordering::SeqCst), 1);
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
                daemon_freshness: None,
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
                daemon_freshness: None,
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
        source: LabRunnerSelectionSource::Explicit,
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
                daemon_freshness: None,
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
                    connection_warning: None,
                    homeboy_version: Some("homeboy 0.0.0".to_string()),
                    homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
                    session_path: Some("/tmp/lab.json".to_string()),
                    leaseless_recovery: None,
                    state_loss_recovery: None,
                    leaseless_recovery_evidence: None,
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
                daemon_freshness: None,
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
                    connection_warning: None,
                    homeboy_version: None,
                    homeboy_build_identity: None,
                    session_path: Some("/tmp/lab.json".to_string()),
                    leaseless_recovery: None,
                    state_loss_recovery: None,
                    leaseless_recovery_evidence: None,
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
        remote_daemon_lease_id: Some("lease-42".to_string()),
        homeboy_version: "homeboy 0.0.0".to_string(),
        homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
        connected_at: "2026-06-03T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    }
}

fn unreachable_health_status(runner_id: &str, connected: bool) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected,
        state: if connected {
            super::super::RunnerSessionState::Connected
        } else {
            super::super::RunnerSessionState::Disconnected
        },
        session: Some(connected_direct_session(
            runner_id,
            Some("http://127.0.0.1:63378"),
        )),
        stale_daemon: None,
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Unavailable,
        active_job_source: None,
        active_job_error: None,
        session_path: "/tmp/lab-unreachable-health.json".to_string(),
    }
}

fn connected_direct_connect_report(runner_id: &str) -> RunnerConnectReport {
    RunnerConnectReport {
        runner_id: runner_id.to_string(),
        mode: Some(RunnerTunnelMode::DirectSsh),
        role: Some(super::super::RunnerSessionRole::Controller),
        connected: true,
        recorded: None,
        local_url: Some("http://127.0.0.1:63378".to_string()),
        broker_url: None,
        controller_id: None,
        remote_daemon_address: Some("127.0.0.1:5678".to_string()),
        tunnel_pid: None,
        remote_daemon_pid: Some(42),
        connection_warning: None,
        homeboy_version: Some("homeboy 0.0.0".to_string()),
        homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
        session_path: Some("/tmp/lab-unreachable-health.json".to_string()),
        leaseless_recovery: None,
        state_loss_recovery: None,
        leaseless_recovery_evidence: None,
        failure_kind: None,
        failure_message: None,
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

fn restartable_daemon_freshness() -> DaemonFreshnessReport {
    DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(DaemonStaleReasonCode::VersionMismatch),
        restartable: true,
        lease_id: Some("lease".to_string()),
        pid: None,
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        active_jobs: 0,
        termination_evidence: None,
        repair_plan: Vec::new(),
    }
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
