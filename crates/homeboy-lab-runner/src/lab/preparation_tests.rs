use super::super::lab_selection::{
    authoritative_status_for_preflight_with_transport,
    preflight_lab_runner_availability_from_status_with_transport,
    prepare_lab_runner_for_offload_with, reconnect_after_compatible_lease_with,
    LabRunnerPreparation, LabRunnerSelection,
};
use super::*;
use crate::{
    RunnerActiveJobState, RunnerConnectReport, RunnerStatusReport, RunnerTunnelMode,
    RunnerTunnelProcessStartIdentity,
};
use homeboy_core::daemon::{
    DaemonFreshnessReport, DaemonRecoveryEvidence, DaemonRepairStep, DaemonStaleReasonCode,
};
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
                configured_job_binary_build_identity: None,
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
    connect.remote_daemon_pid = Some(1467759);

    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };
    let (availability, admitted) = preflight_lab_runner_availability_from_status_with_transport(
        &selection,
        |_| Ok(stale_projection.clone()),
        Some(1),
        Some(&connect),
        true,
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
fn non_loopback_transport_rejects_missing_tunnel_pid() {
    let mut status = unreachable_health_status("lab", false);
    let session = status.session.as_mut().expect("session");
    session.remote_daemon_pid = Some(1467759);
    session.remote_daemon_lease_id = Some("lease-fresh".to_string());
    status.daemon_freshness = Some(fresh_daemon_freshness());
    let mut connect = connected_direct_connect_report("lab");
    connect.local_url = session.local_url.clone();
    connect.remote_daemon_pid = session.remote_daemon_pid;

    let error = authoritative_status_for_preflight_with_transport(status, Some(&connect), false)
        .expect_err("remote SSH requires an owned tunnel");

    assert!(error.message.contains("no tunnel PID for a non-loopback"));
}

#[test]
fn mixed_tunnel_evidence_is_rejected_before_endpoint_probing() {
    let mut status = unreachable_health_status("lab", false);
    let session = status.session.as_mut().expect("session");
    session.tunnel_pid = Some(std::process::id());
    session.tunnel_process_start_identity = Some(current_process_tunnel_identity());
    session.remote_daemon_pid = Some(1467759);
    session.remote_daemon_lease_id = Some("lease-fresh".to_string());
    status.daemon_freshness = Some(fresh_daemon_freshness());
    let mut connect = connected_direct_connect_report("lab");
    connect.local_url = session.local_url.clone();
    connect.remote_daemon_pid = session.remote_daemon_pid;
    connect.tunnel_pid = None;

    let error = authoritative_status_for_preflight_with_transport(status, Some(&connect), true)
        .expect_err("mixed tunnel evidence cannot establish ownership");

    assert!(error.message.contains("tunnel evidence is mixed"));
}

#[test]
fn differing_tunnel_pids_are_rejected_before_identity_verification() {
    let mut status = unreachable_health_status("lab", false);
    let session = status.session.as_mut().expect("session");
    session.tunnel_pid = Some(std::process::id());
    session.tunnel_process_start_identity = Some(current_process_tunnel_identity());
    session.remote_daemon_pid = Some(1467759);
    session.remote_daemon_lease_id = Some("lease-fresh".to_string());
    status.daemon_freshness = Some(fresh_daemon_freshness());
    let mut connect = connected_direct_connect_report("lab");
    connect.local_url = session.local_url.clone();
    connect.remote_daemon_pid = session.remote_daemon_pid;
    connect.tunnel_pid = Some(std::process::id().saturating_add(1));

    let error = authoritative_status_for_preflight_with_transport(status, Some(&connect), false)
        .expect_err("different tunnel PIDs cannot establish ownership");

    assert!(error.message.contains("tunnel evidence is mixed"));
}

#[cfg(unix)]
#[test]
fn in_process_connect_authority_keeps_its_owned_tunnel_alive_through_admission_and_cleanup() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

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
    let tunnel = Arc::new(Mutex::new(None));
    let session = Arc::new(Mutex::new(None));
    let mut connected = connected_reverse_status("lab", None);
    connected.connected = true;
    connected.state = super::super::RunnerSessionState::Connected;
    connected.daemon_freshness = Some(DaemonFreshnessReport {
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
    connected.active_job_state = RunnerActiveJobState::Available;
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };
    let initial = RunnerStatusReport {
        runner_id: "lab".to_string(),
        connected: false,
        state: super::super::RunnerSessionState::Disconnected,
        session: None,
        stale_daemon: None,
        configured_job_binary_build_identity: None,
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
    };
    let connect_tunnel = Arc::clone(&tunnel);
    let connect_session = Arc::clone(&session);
    let prepared = prepare_lab_runner_for_offload_with(
        &selection,
        |_| Ok(initial.clone()),
        |_| {
            let mut child = Command::new("sh");
            child.args(["-c", "sleep 60"]);
            unsafe {
                child.pre_exec(|| {
                    if libc::setpgid(0, 0) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
            let child = child.spawn().expect("spawn owned tunnel");
            let tunnel_pid = child.id();
            let tunnel_identity = match (0..10)
                .find_map(|_| {
                    let identity = homeboy_core::process::process_start_identity(tunnel_pid)
                        .expect("inspect tunnel");
                    if identity.is_none() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    identity
                })
                .expect("live tunnel identity")
            {
                homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks } => {
                    RunnerTunnelProcessStartIdentity::Linux { starttime_ticks }
                }
                homeboy_core::process::ProcessStartIdentity::Macos {
                    start_seconds,
                    start_microseconds,
                } => RunnerTunnelProcessStartIdentity::Macos {
                    start_seconds,
                    start_microseconds,
                },
            };
            let mut session = connected_direct_session("lab", Some(&format!("http://{address}")));
            session.local_port = Some(address.port());
            session.tunnel_pid = Some(tunnel_pid);
            session.tunnel_process_start_identity = Some(tunnel_identity);
            session.remote_daemon_pid = Some(1467759);
            session.remote_daemon_lease_id = Some("lease-fresh".to_string());
            let mut report = connected_direct_connect_report("lab");
            report.local_url = session.local_url.clone();
            report.tunnel_pid = Some(tunnel_pid);
            report.remote_daemon_pid = session.remote_daemon_pid;
            *connect_tunnel.lock().expect("tunnel lock") = Some(child);
            *connect_session.lock().expect("session lock") = Some(session);
            Ok((report, 0))
        },
    )
    .expect("in-process connect authority preparation");

    assert!(matches!(prepared, LabRunnerPreparation::Ready { .. }));
    let session = session
        .lock()
        .expect("session lock")
        .clone()
        .expect("prepared session");
    let tunnel_pid = session.tunnel_pid.expect("tunnel PID");
    let mut report = connected_direct_connect_report("lab");
    report.local_url = session.local_url.clone();
    report.tunnel_pid = Some(tunnel_pid);
    report.remote_daemon_pid = session.remote_daemon_pid;
    connected.session = Some(session.clone());
    let (_, admitted) = preflight_lab_runner_availability_from_status_with_transport(
        &selection,
        |_| Ok(connected.clone()),
        Some(1),
        Some(&report),
        false,
    )
    .expect("live owned tunnel admits after connect returns");
    assert!(homeboy_core::process::pid_is_running(tunnel_pid));
    assert_eq!(
        admitted.session.expect("session").tunnel_pid,
        Some(tunnel_pid)
    );

    crate::connection::terminate_tunnel_if_owned(&session);
    let _ = tunnel
        .lock()
        .expect("tunnel lock")
        .as_mut()
        .expect("owned tunnel")
        .wait()
        .expect("reap tunnel");
    assert!(!homeboy_core::process::pid_is_running(tunnel_pid));
    daemon.join().expect("daemon");
}

#[test]
fn stale_disconnected_projection_rejects_reused_tunnel_identity_before_health() {
    let mut status = unreachable_health_status("lab", false);
    let session = status.session.as_mut().expect("session");
    session.local_url = Some("http://127.0.0.1:55626".to_string());
    session.local_port = Some(55626);
    session.tunnel_pid = Some(std::process::id());
    session.tunnel_process_start_identity = Some(RunnerTunnelProcessStartIdentity::Macos {
        start_seconds: 1,
        start_microseconds: 2,
    });
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
    connect.local_url = Some("http://127.0.0.1:55626".to_string());
    connect.tunnel_pid = Some(std::process::id());
    connect.remote_daemon_pid = Some(1467759);

    let error = authoritative_status_for_preflight_with_transport(status, Some(&connect), false)
        .expect_err("reused tunnel identity is rejected before endpoint probing");

    assert!(error.message.contains("tunnel is no longer owned"));
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
    session.tunnel_pid = Some(std::process::id());
    session.tunnel_process_start_identity = Some(current_process_tunnel_identity());
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
    connect.tunnel_pid = Some(std::process::id());
    connect.remote_daemon_pid = Some(1467759);
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };

    let error = preflight_lab_runner_availability_from_status_with_transport(
        &selection,
        |_| Ok(status.clone()),
        Some(1),
        Some(&connect),
        false,
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
        session.tunnel_pid = Some(std::process::id());
        session.tunnel_process_start_identity = Some(current_process_tunnel_identity());
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
        connect.tunnel_pid = Some(std::process::id());
        connect.remote_daemon_pid = Some(1467759);
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::DirectSsh,
        };
        let error = preflight_lab_runner_availability_from_status_with_transport(
            &selection,
            |_| Ok(status.clone()),
            Some(1),
            Some(&connect),
            false,
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

    let admitted = authoritative_status_for_preflight_with_transport(status, None, false)
        .expect("ordinary status");

    assert!(!admitted.connected);
}

#[test]
fn stale_disconnected_projection_rejects_mismatched_connect_evidence() {
    let status = unreachable_health_status("lab", false);
    let connect = connected_direct_connect_report("lab");

    let error = authoritative_status_for_preflight_with_transport(status, Some(&connect), false)
        .expect_err("different endpoint evidence is not admitted");

    assert!(error.message.contains("did not converge"));
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
                configured_job_binary_build_identity: None,
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
                configured_job_binary_build_identity: None,
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
                configured_job_binary_build_identity: None,
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
            reason: "connected runner `lab` daemon runtime is stale after runner-side rebuilds or path changes; restart the active daemon with `homeboy runner doctor lab --scope lab-offload`".to_string()
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
                configured_job_binary_build_identity: None,
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
    // A stale report whose ownership proof is incomplete takes the terminal
    // ownership-blocker branch, which replaces the stale-daemon branch rather
    // than adding to it: no severity line, no version drift pair, and no
    // refresh command, because none of them are authorized without that proof.
    assert!(err
        .message
        .contains("typed ownership evidence is insufficient"));
    assert!(!err.message.contains("daemon is stale"));
    assert!(!err
        .message
        .contains("homeboy runner doctor lab --scope lab-offload"));
    assert!(err
        .details
        .get("tried")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .all(|suggestion| suggestion
            .as_str()
            .is_none_or(|value| !value.contains("homeboy runner doctor lab --scope lab-offload"))));
    assert!(err
        .details
        .get(homeboy_core::error::ACTIONS_DETAILS_KEY)
        .is_none());
}

#[test]
fn lab_preparation_does_not_offer_an_unavailable_reconciliation_plan() {
    let selection = LabRunnerSelection {
        runner_id: "lab".to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: RunnerTunnelMode::DirectSsh,
    };
    let mut freshness = restartable_daemon_freshness();
    freshness.stale_reason_code = Some(DaemonStaleReasonCode::LeaseCorrupt);
    freshness.recovery_evidence = Some(DaemonRecoveryEvidence::Unavailable);
    freshness.ownership_evidence = Some("ambiguous remote daemon candidates".to_string());
    freshness.repair_plan = vec![DaemonRepairStep::text(
        "runner_reconcile_leaseless_orphans",
        "homeboy runner connect lab --reconcile-leaseless-orphans --confirm-no-daemon-owner",
    )];

    let error = prepare_lab_runner_for_offload_with(
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
                configured_job_binary_build_identity: None,
                daemon_freshness: Some(freshness.clone()),
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
        |_| panic!("terminal ownership evidence must not reconnect"),
    )
    .expect_err("ambiguous remote lease ownership must block explicit offload");

    let rendered = format!(
        "{} {}",
        error.message,
        serde_json::to_string(&error.details).expect("serialize error details")
    );
    assert!(rendered.contains("typed ownership evidence is insufficient"));
    assert!(rendered.contains("ambiguous remote daemon candidates"));
    assert!(!rendered.contains("reconcile-leaseless-orphans"));
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
                            configured_job_binary_build_identity: None,
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
fn compatible_reconnect_wait_reuses_the_owner_session_without_a_second_tunnel() {
    homeboy_core::test_support::with_isolated_home(|_| {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let owner = homeboy_core::runtime_promotion::acquire("runner daemon reconnect", "lab")
            .expect("owner lease");
        let connects = Arc::new(AtomicUsize::new(0));
        connects.fetch_add(1, Ordering::SeqCst);
        let contender_connects = Arc::clone(&connects);
        let contender = std::thread::spawn(move || {
            reconnect_after_compatible_lease_with(
                "lab",
                || {
                    homeboy_core::runtime_promotion::acquire_waiting_for_compatible(
                        "runner daemon reconnect",
                        "lab",
                        std::time::Duration::from_secs(1),
                        |_| {},
                    )
                    .map(drop)
                },
                || {
                    let session = connected_direct_session("lab", Some("http://127.0.0.1:63378"));
                    Ok(RunnerStatusReport {
                        runner_id: "lab".to_string(),
                        connected: true,
                        state: super::super::RunnerSessionState::Connected,
                        session: Some(session),
                        stale_daemon: None,
                        configured_job_binary_build_identity: None,
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
                    })
                },
                || {
                    contender_connects.fetch_add(1, Ordering::SeqCst);
                    panic!("compatible owner session must be reused without a second connect")
                },
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(owner);

        let (report, exit_code) = contender
            .join()
            .expect("contender thread")
            .expect("compatible handoff");
        assert_eq!(exit_code, 0);
        assert!(report.connected);
        assert_eq!(report.local_url.as_deref(), Some("http://127.0.0.1:63378"));
        assert_eq!(connects.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn compatible_reconnect_wait_preserves_timeout_diagnostics_without_connecting() {
    let timeout = Error::new(
        ErrorCode::RuntimePromotionWaitTimeout,
        "runtime promotion wait timed out after 30s behind pid 42 operation `runner daemon reconnect` target `lab`".to_string(),
        serde_json::json!({
            "queue_state": "timed_out_waiting_for_compatible_owner",
            "holder_pid": 42,
            "holder_operation": "runner daemon reconnect",
            "target": "lab",
        }),
    );
    let error = reconnect_after_compatible_lease_with(
        "lab",
        || Err(timeout),
        || unreachable!("timed-out wait must not read or adopt a session"),
        || unreachable!("timed-out wait must not open a second tunnel"),
    )
    .expect_err("bounded lease wait propagates its diagnostic");

    assert_eq!(error.code, ErrorCode::RuntimePromotionWaitTimeout);
    assert_eq!(
        error.details["queue_state"],
        "timed_out_waiting_for_compatible_owner"
    );
    assert_eq!(error.details["holder_pid"], 42);
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
                configured_job_binary_build_identity: None,
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
                configured_job_binary_build_identity: None,
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
                configured_job_binary_build_identity: None,
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
                configured_job_binary_build_identity: None,
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
        configured_job_binary_build_identity: None,
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
        configured_job_binary_build_identity: None,
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

fn current_process_tunnel_identity() -> RunnerTunnelProcessStartIdentity {
    match homeboy_core::process::process_start_identity(std::process::id())
        .expect("inspect current process")
        .expect("current process is live")
    {
        homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks } => {
            RunnerTunnelProcessStartIdentity::Linux { starttime_ticks }
        }
        homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        } => RunnerTunnelProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        },
    }
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

fn fresh_daemon_freshness() -> DaemonFreshnessReport {
    DaemonFreshnessReport {
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
