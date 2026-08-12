use super::super::lab_selection::{
    authoritative_status_for_preflight, contended_runner_unavailable_error,
    preflight_lab_runner_availability_from_status, prepare_lab_runner_for_offload_with,
    wait_for_contended_runner, wait_for_live_session_with, LabRunnerPreparation,
    LabRunnerSelection,
};
use super::*;
use crate::{RunnerActiveJobState, RunnerConnectReport, RunnerStatusReport, RunnerTunnelMode};
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use homeboy_core::{Error, ErrorCode};

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
                active_job_recovery_evidence: None,
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
                    failure_evidence: None,
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
fn successful_connect_session_converges_after_transient_disconnection() {
    let probes = std::cell::Cell::new(0);
    let pauses = std::cell::Cell::new(0);
    let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
    let started = std::time::Instant::now();

    let session =
        wait_for_live_session_with(
            std::time::Duration::from_millis(150),
            |_| {
                let probe = probes.get();
                probes.set(probe + 1);
                Ok((probe == 2)
                    .then(|| connected_direct_session("lab", Some("http://127.0.0.1:1234"))))
            },
            || started + elapsed.get(),
            |duration| {
                pauses.set(pauses.get() + 1);
                elapsed.set(elapsed.get() + duration);
            },
        )
        .expect("session convergence");

    assert!(session.is_some());
    assert_eq!(probes.get(), 3);
    assert_eq!(pauses.get(), 2);
}

#[test]
fn successful_auto_connect_authorizes_the_stale_disconnected_projection_for_this_preflight() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("daemon listener");
    let address = listener.local_addr().expect("daemon address");
    let daemon = std::thread::spawn(move || {
        for (path, body) in [
            (
                "/health",
                r#"{"freshness":{"fresh":true,"stale_reason_code":null,"restartable":false,"lease_id":"lease-fresh","pid":1467759,"recovery_evidence":null,"ownership_evidence":null,"adoption_command":null,"binary_hash":null,"daemon_version":null,"daemon_build_identity":null,"runtime_paths":null,"active_jobs":0,"termination_evidence":null,"repair_plan":[]},"pid":1467759,"build_identity":{"display":"homeboy 0.0.0+test"}}"#,
            ),
            (
                "/jobs",
                r#"{"success":true,"data":{"body":{"active_runner_jobs":[],"stale_runner_jobs":[]}}}"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().expect("daemon request");
            let mut request = [0; 4096];
            let length = stream.read(&mut request).expect("read request");
            assert!(String::from_utf8_lossy(&request[..length])
                .starts_with(&format!("GET {path} HTTP/1.1")));
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}"
                    )
                    .as_bytes(),
                )
                .expect("daemon response");
        }
    });
    let mut stale_projection = connected_reverse_status("lab", None);
    stale_projection.connected = false;
    stale_projection.state = super::super::RunnerSessionState::Disconnected;
    stale_projection.session = Some(connected_direct_session(
        "lab",
        Some(&format!("http://{address}")),
    ));
    let session = stale_projection.session.as_mut().expect("direct session");
    session.tunnel_pid = Some(22420);
    session.local_port = Some(address.port());
    session.remote_daemon_pid = Some(1467759);
    session.remote_daemon_lease_id = Some("lease-fresh".to_string());
    stale_projection.daemon_freshness = Some(DaemonFreshnessReport {
        fresh: true,
        stale_reason_code: None,
        restartable: false,
        lease_id: Some("lease-fresh".to_string()),
        pid: Some(1467759),
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        // Status is stale; admission must use the matching fresh health count.
        active_jobs: 1,
        termination_evidence: None,
        repair_plan: Vec::new(),
    });
    stale_projection.active_job_state = RunnerActiveJobState::NotQueried;
    assert_eq!(
        stale_projection.active_job_state,
        RunnerActiveJobState::NotQueried
    );
    let mut connect = connected_direct_connect_report("lab");
    connect.local_url = Some(format!("http://{address}"));
    connect.tunnel_pid = Some(22420);
    connect.remote_daemon_pid = Some(1467759);

    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };
    let (availability, admitted) = preflight_lab_runner_availability_from_status(
        &selection,
        |_| Ok(stale_projection.clone()),
        Some(1),
        Some(&connect),
    )
    .expect("successful connect remains authoritative for this preflight");

    daemon.join().expect("daemon");
    assert!(admitted.connected);
    assert_eq!(admitted.active_job_state, RunnerActiveJobState::Available);
    assert!(
        availability.accepts_jobs,
        "typed /jobs probe admits the workload"
    );
    assert_eq!(
        admitted.session.expect("session").local_url.as_deref(),
        Some(format!("http://{address}").as_str())
    );
}

#[test]
fn stale_disconnected_projection_rejects_mismatched_typed_health() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("daemon listener");
    let address = listener.local_addr().expect("daemon address");
    let daemon = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("health request");
        let mut request = [0; 4096];
        let length = stream.read(&mut request).expect("read request");
        assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /health HTTP/1.1"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"freshness\":{\"fresh\":true,\"stale_reason_code\":null,\"restartable\":false,\"lease_id\":\"lease-other\",\"pid\":1467759,\"recovery_evidence\":null,\"ownership_evidence\":null,\"adoption_command\":null,\"binary_hash\":null,\"daemon_version\":null,\"daemon_build_identity\":null,\"runtime_paths\":null,\"active_jobs\":0,\"termination_evidence\":null,\"repair_plan\":[]},\"pid\":1467759}")
            .expect("health response");
    });
    let mut status = connected_reverse_status("lab", None);
    status.connected = false;
    status.state = super::super::RunnerSessionState::Disconnected;
    status.session = Some(connected_direct_session(
        "lab",
        Some(&format!("http://{address}")),
    ));
    let session = status.session.as_mut().expect("session");
    session.local_port = Some(address.port());
    session.tunnel_pid = Some(22420);
    session.remote_daemon_pid = Some(1467759);
    session.remote_daemon_lease_id = Some("lease-fresh".to_string());
    status.daemon_freshness = Some(DaemonFreshnessReport {
        fresh: true,
        stale_reason_code: None,
        restartable: false,
        lease_id: Some("lease-fresh".to_string()),
        pid: Some(1467759),
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
    });
    let mut connect = connected_direct_connect_report("lab");
    connect.local_url = Some(format!("http://{address}"));
    connect.tunnel_pid = Some(22420);
    connect.remote_daemon_pid = Some(1467759);
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let error = preflight_lab_runner_availability_from_status(
        &selection,
        |_| Ok(status.clone()),
        Some(1),
        Some(&connect),
    )
    .expect_err("mismatched health is rejected");

    daemon.join().expect("daemon");
    assert!(error.message.contains("health did not match"));
}

#[test]
fn stale_disconnected_projection_rejects_omitted_recorded_health_coordinates() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    for (coordinate, health) in [
        (
            "lease",
            r#"{"freshness":{"fresh":true,"stale_reason_code":null,"restartable":false,"lease_id":null,"pid":1467759,"recovery_evidence":null,"ownership_evidence":null,"adoption_command":null,"binary_hash":null,"daemon_version":null,"daemon_build_identity":null,"runtime_paths":null,"active_jobs":0,"termination_evidence":null,"repair_plan":[]},"pid":1467759,"build_identity":{"display":"homeboy 0.0.0+test"}}"#,
        ),
        (
            "pid",
            r#"{"freshness":{"fresh":true,"stale_reason_code":null,"restartable":false,"lease_id":"lease-fresh","pid":null,"recovery_evidence":null,"ownership_evidence":null,"adoption_command":null,"binary_hash":null,"daemon_version":null,"daemon_build_identity":null,"runtime_paths":null,"active_jobs":0,"termination_evidence":null,"repair_plan":[]},"pid":null,"build_identity":{"display":"homeboy 0.0.0+test"}}"#,
        ),
        (
            "build identity",
            r#"{"freshness":{"fresh":true,"stale_reason_code":null,"restartable":false,"lease_id":"lease-fresh","pid":1467759,"recovery_evidence":null,"ownership_evidence":null,"adoption_command":null,"binary_hash":null,"daemon_version":null,"daemon_build_identity":null,"runtime_paths":null,"active_jobs":0,"termination_evidence":null,"repair_plan":[]},"pid":1467759}"#,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("daemon listener");
        let address = listener.local_addr().expect("daemon address");
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request");
            let mut request = [0; 4096];
            let length = stream.read(&mut request).expect("read request");
            assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET /health HTTP/1.1"));
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{health}").as_bytes()).expect("health response");
        });
        let mut status = connected_reverse_status("lab", None);
        status.connected = false;
        status.state = super::super::RunnerSessionState::Disconnected;
        status.session = Some(connected_direct_session(
            "lab",
            Some(&format!("http://{address}")),
        ));
        let session = status.session.as_mut().expect("session");
        session.local_port = Some(address.port());
        session.tunnel_pid = Some(22420);
        session.remote_daemon_pid = Some(1467759);
        session.remote_daemon_lease_id = Some("lease-fresh".to_string());
        status.daemon_freshness = Some(DaemonFreshnessReport {
            fresh: true,
            stale_reason_code: None,
            restartable: false,
            lease_id: Some("lease-fresh".to_string()),
            pid: Some(1467759),
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
        });
        let mut connect = connected_direct_connect_report("lab");
        connect.local_url = Some(format!("http://{address}"));
        connect.tunnel_pid = Some(22420);
        connect.remote_daemon_pid = Some(1467759);
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::DirectSsh,
        };
        let error = preflight_lab_runner_availability_from_status(
            &selection,
            |_| Ok(status.clone()),
            Some(1),
            Some(&connect),
        )
        .expect_err("omitted coordinate is rejected");
        daemon.join().expect("daemon");
        assert!(
            error.message.contains("health did not match"),
            "{coordinate}"
        );
    }
}

#[test]
fn true_disconnected_projection_is_not_authorized_without_matching_connect_evidence() {
    let status = unreachable_health_status("lab", false);

    let admitted = authoritative_status_for_preflight(status, None).expect("ordinary status");

    assert!(!admitted.connected);
}

#[test]
fn stale_disconnected_projection_rejects_mismatched_connect_evidence() {
    let status = unreachable_health_status("lab", false);
    let connect = connected_direct_connect_report("lab");

    let error = authoritative_status_for_preflight(status, Some(&connect))
        .expect_err("different endpoint evidence is not admitted");

    assert!(error.message.contains("did not converge"));
}

#[test]
fn successful_connect_session_convergence_exhausts_its_deadline() {
    let probes = std::cell::Cell::new(0);
    let pauses = std::cell::Cell::new(0);
    let elapsed = std::cell::Cell::new(std::time::Duration::ZERO);
    let started = std::time::Instant::now();

    let session = wait_for_live_session_with(
        std::time::Duration::from_millis(150),
        |_| {
            probes.set(probes.get() + 1);
            Ok(None)
        },
        || started + elapsed.get(),
        |duration| {
            pauses.set(pauses.get() + 1);
            elapsed.set(elapsed.get() + duration);
        },
    )
    .expect("session convergence");

    assert!(session.is_none());
    assert_eq!(probes.get(), 3);
    assert_eq!(pauses.get(), 3);
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
                active_job_recovery_evidence: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("connected runner should not reconnect"),
    )
    .expect("prepared");

    assert!(matches!(prepared, LabRunnerPreparation::Ready { .. }));
}

#[test]
fn lab_runner_preparation_accepts_explicit_connected_reverse_runner_with_unavailable_verification()
{
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::Reverse,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(connected_reverse_status(
                runner_id,
                Some(RunnerStaleDaemonWarning::verification_unavailable(
                    runner_id,
                    "homeboy 0.0.0".to_string(),
                    Some("homeboy 0.0.0+test".to_string()),
                    "reverse_runner_identity_unavailable",
                    "reverse runner identity cannot be verified".to_string(),
                )),
            ))
        },
        |_| panic!("connected reverse runner should not reconnect"),
    )
    .expect("prepared");

    assert!(matches!(prepared, LabRunnerPreparation::Ready { .. }));
}

#[test]
fn lab_runner_preparation_accepts_default_connected_reverse_runner_with_unavailable_verification() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Default,
        mode: RunnerTunnelMode::Reverse,
    };

    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(connected_reverse_status(
                runner_id,
                Some(RunnerStaleDaemonWarning::verification_unavailable(
                    runner_id,
                    "homeboy 0.0.0".to_string(),
                    Some("homeboy 0.0.0+test".to_string()),
                    "reverse_runner_identity_unavailable",
                    "reverse runner identity cannot be verified".to_string(),
                )),
            ))
        },
        |_| panic!("connected reverse runner should not reconnect"),
    )
    .expect("prepared");

    assert!(matches!(prepared, LabRunnerPreparation::Ready { .. }));
}

#[test]
fn lab_runner_preparation_blocks_connected_reverse_runner_with_compared_mismatch() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::Reverse,
    };

    let error = prepare_lab_runner_for_offload_with(
        &selection,
        |runner_id| {
            Ok(connected_reverse_status(
                runner_id,
                Some(stale_daemon_warning(runner_id)),
            ))
        },
        |_| panic!("stale reverse runner should not reconnect"),
    )
    .expect_err("compared daemon mismatch should remain blocked");

    assert!(error.message.contains("daemon is stale"));
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
                active_job_recovery_evidence: None,
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
                active_job_recovery_evidence: None,
                session_path: "/tmp/lab.json".to_string(),
            })
        },
        |_| panic!("stale runtime daemon should not dispatch or reconnect automatically"),
    )
    .expect("prepared");

    assert_eq!(
        prepared,
        LabRunnerPreparation::FallBackLocal {
            reason: "connected runner `lab` daemon runtime is stale after runner-side rebuilds or path changes; restart the active daemon with `homeboy runner refresh-homeboy lab --ref bbbbbbbbbbbb --reconnect`".to_string()
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
                active_job_recovery_evidence: None,
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
        .contains("homeboy runner refresh-homeboy lab --ref "));
    assert!(err
        .details
        .get("tried")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|suggestion| suggestion
            .as_str()
            .is_some_and(|value| value.contains("homeboy runner refresh-homeboy lab --ref "))));
    let action = &err.details[homeboy_core::error::ACTIONS_DETAILS_KEY][0];
    assert_eq!(action["id"], "runner.refresh_homeboy");
    assert_eq!(action["program"], "homeboy");
    assert_eq!(action["args"][0], "runner");
    assert_eq!(action["args"][1], "refresh-homeboy");
    assert_eq!(action["args"][2], "lab");
    assert_eq!(action["args"][3], "--ref");
    assert_eq!(action["args"][4], "bbbbbbbbbbbb");
    assert_eq!(action["args"][5], "--reconnect");
    assert_eq!(
        action["evidence"]["recovery_plan"][0],
        err.details["tried"][0]
    );
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
                            active_job_recovery_evidence: None,
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
        assert!(matches!(
            handoff.join().expect("handoff thread").expect("handoff"),
            LabRunnerPreparation::Ready { .. }
        ));
    }
    assert_eq!(connects.load(Ordering::SeqCst), 1);
}

#[test]
fn lease_contender_waits_for_owner_session_without_a_second_connect() {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Barrier,
    };

    let connected = Arc::new(AtomicBool::new(false));
    let connects = Arc::new(AtomicUsize::new(0));
    let handoff_start = Arc::new(Barrier::new(2));
    let owner_connected = Arc::clone(&connected);
    let owner_connects = Arc::clone(&connects);
    let owner_start = Arc::clone(&handoff_start);
    let owner = std::thread::spawn(move || {
        owner_start.wait();
        owner_connects.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(75));
        owner_connected.store(true, Ordering::SeqCst);
    });

    let contender_connected = Arc::clone(&connected);
    let contender_start = Arc::clone(&handoff_start);
    let contender = std::thread::spawn(move || {
        contender_start.wait();
        // This is the contender handoff after RuntimePromotionLease::acquire
        // reports that the owner is reconnecting the shared runner.
        let session = wait_for_contended_runner(
            contention_error(),
            std::time::Duration::from_secs(1),
            |_| {
                Ok(contender_connected.load(Ordering::SeqCst).then(|| {
                    connected_direct_session("lab-lease-contention", Some("http://127.0.0.1:63378"))
                }))
            },
        )
        .expect("wait succeeds")
        .expect("owner publishes a healthy session");

        session
    });

    owner.join().expect("owner");
    let session = contender.join().expect("contender handoff");
    assert_eq!(session.runner_id, "lab-lease-contention");
    assert_eq!(connects.load(Ordering::SeqCst), 1);
}

#[test]
fn non_contention_lease_failure_is_not_retried() {
    let error = Error::internal_io(
        "permission denied",
        Some("read promotion lease".to_string()),
    );
    let returned = wait_for_contended_runner(error, std::time::Duration::from_secs(1), |_| {
        panic!("non-contention errors must not poll or reconnect")
    })
    .expect_err("non-contention failure propagates");

    assert_eq!(returned.code, ErrorCode::InternalIoError);
    assert_eq!(returned.details["context"], "read promotion lease");
}

#[test]
fn contended_session_wait_obeys_the_deadline() {
    let started = std::time::Instant::now();
    let result = wait_for_contended_runner(
        contention_error(),
        std::time::Duration::from_millis(250),
        |remaining| {
            std::thread::sleep(remaining);
            Ok(None)
        },
    )
    .expect("contention wait completes");

    assert!(result.is_none());
    assert!(started.elapsed() < std::time::Duration::from_millis(750));
}

#[test]
fn contended_handoff_failure_preserves_reconnect_lease_evidence() {
    let error = contended_runner_unavailable_error("lab", contention_error());

    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert_eq!(error.details["reconnect_lease"]["holder_pid"], 42);
    assert!(error
        .message
        .contains("another controller owned its reconnect lease"));
    assert!(error.details["tried"]
        .as_array()
        .expect("remediation list")
        .iter()
        .any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Waited 30s"))));
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
                active_job_recovery_evidence: None,
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
                active_job_recovery_evidence: None,
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
                active_job_recovery_evidence: None,
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
                    failure_evidence: None,
                    failure_kind: None,
                    failure_message: None,
                },
                0,
            ))
        },
    )
    .expect("prepared");

    assert!(matches!(prepared, LabRunnerPreparation::Ready { .. }));
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
                active_job_recovery_evidence: None,
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
                    failure_evidence: None,
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
        tunnel_process_start_identity: None,
        proxy_forward: None,
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

fn connected_reverse_status(
    runner_id: &str,
    stale_daemon: Option<RunnerStaleDaemonWarning>,
) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: true,
        state: super::super::RunnerSessionState::Connected,
        session: Some(super::super::RunnerSession {
            runner_id: runner_id.to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: super::super::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("controller".to_string()),
            broker_url: Some("http://127.0.0.1:9876".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "homeboy 0.0.0".to_string(),
            homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
            connected_at: "2026-06-03T00:00:00Z".to_string(),
            worker_identity: Some("worker-1".to_string()),
            worker_pid: Some(1234),
            last_seen_at: Some(chrono::Utc::now().to_rfc3339()),
            leaseless_recovery_evidence: None,
        }),
        stale_daemon,
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: "/tmp/lab.json".to_string(),
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
        active_job_recovery_evidence: None,
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
        failure_evidence: None,
    }
}

fn contention_error() -> Error {
    Error::new(
        ErrorCode::RuntimePromotionContended,
        "runtime promotion is held by pid 42".to_string(),
        serde_json::json!({ "holder_pid": 42 }),
    )
}

fn stale_daemon_warning(runner_id: &str) -> RunnerStaleDaemonWarning {
    RunnerStaleDaemonWarning::new(
        runner_id,
        "homeboy 0.218.0".to_string(),
        "homeboy 0.219.0".to_string(),
        Some("homeboy 0.218.0+aaaaaaaaaaaa".to_string()),
        Some("homeboy 0.219.0+bbbbbbbbbbbb".to_string()),
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
        Some("homeboy 0.219.0+aaaaaaaaaaaa".to_string()),
        Some("homeboy 0.219.0+bbbbbbbbbbbb".to_string()),
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
