#![cfg(test)]

use clap::Parser;
use std::collections::HashMap;
use std::io::{Read, Write};

use super::*;

#[test]
fn first_connect_routes_to_idempotent_ensure_start_when_no_daemon_exists() {
    let status = RemoteDaemonStatus {
        daemon: None,
        stale_reason: None,
        stale_reason_code: None,
        fresh: false,
        reachable: false,
        active_jobs: 0,
        endpoint_probe_error: None,
        termination_evidence: None,
    };

    assert_eq!(
        remote_daemon_connect_action(None, &status).expect("ensure start"),
        RemoteDaemonConnectAction::Start
    );
}

#[test]
fn missing_daemon_state_with_active_jobs_refuses_ensure_running() {
    let status = RemoteDaemonStatus {
        daemon: None,
        stale_reason: Some("daemon state is unavailable".to_string()),
        stale_reason_code: Some(DaemonStaleReasonCode::LeaseMissing),
        fresh: false,
        reachable: false,
        active_jobs: 1,
        endpoint_probe_error: None,
        termination_evidence: None,
    };

    let error = remote_daemon_connect_action(None, &status)
        .expect_err("active jobs require explicit recovery");

    assert!(error.contains("1 active job(s)"));
    assert!(error.contains("refusing ensure-running"));
    assert!(error.contains("active-job recovery guidance"));
}

#[test]
fn refuses_to_replace_live_daemon_with_a_different_persisted_lease() {
    let session = direct_ssh_session("lease-recorded");
    let status = remote_daemon_status_for_test(true, true, 0, "lease-live", 4646);

    let err = remote_daemon_connect_action(Some(&session), &status).expect_err("lease mismatch");

    assert!(err.contains("does not match persisted session lease"));
    assert!(err.contains("refusing to replace"));
}

#[test]
fn legacy_session_adopts_consistent_live_daemon_identity() {
    let mut session = direct_ssh_session("lease-old");
    session.remote_daemon_lease_id = None;
    let status = remote_daemon_status_for_test(true, true, 1, "lease-live", 4242);

    assert_eq!(
        remote_daemon_connect_action(Some(&session), &status).expect("adopt live lease"),
        RemoteDaemonConnectAction::Reattach
    );
}

#[test]
fn legacy_session_refuses_pid_reuse_with_different_daemon_identity() {
    let mut session = direct_ssh_session("lease-old");
    session.remote_daemon_lease_id = None;
    let status = remote_daemon_status_for_test(true, true, 0, "lease-live", 5555);

    assert!(remote_daemon_connect_action(Some(&session), &status)
        .expect_err("identity mismatch")
        .contains("does not match the live daemon PID/address"));
}

#[test]
fn parses_daemon_envelope_after_noisy_stdout_preamble() {
    let envelope = parse_envelope(
        "Setting up Swift test infrastructure...\nSwift unavailable; Swift extension installed but not ready...\n{\"success\":true,\"data\":{\"action\":\"start\",\"address\":\"127.0.0.1:49152\",\"pid\":123,\"lease_id\":\"lease-1\"}}\n",
    )
    .expect("parse envelope after preamble");

    assert!(envelope.success);
    assert_eq!(
        envelope
            .data
            .unwrap()
            .get("address")
            .and_then(Value::as_str),
        Some("127.0.0.1:49152")
    );
}

#[test]
fn compares_cli_and_daemon_version_shapes() {
    assert!(versions_match("homeboy 0.204.0", "0.204.0"));
    assert!(versions_match("0.204.0", "homeboy 0.204.0"));
    assert!(versions_match(
        "homeboy 0.204.0+19a41cd5102d",
        "0.204.0+19a41cd5102d"
    ));
    assert!(!versions_match("homeboy 0.201.3", "homeboy 0.204.0"));
}

#[test]
fn extracts_current_daemon_version_shape() {
    assert_eq!(
        daemon_version_from_body(&serde_json::json!({"version":"0.204.0"})),
        Some("0.204.0")
    );
    assert_eq!(
        daemon_version_from_body(&serde_json::json!({
            "success": true,
            "data": {"version": "0.281.2"}
        })),
        Some("0.281.2")
    );
    assert_eq!(
        daemon_identity_from_body(
            &serde_json::json!({"version":"0.228.13","build_identity":{"display":"homeboy 0.228.13+f7569a5e"}})
        ),
        Some("homeboy 0.228.13+f7569a5e")
    );
    assert_eq!(
        daemon_identity_from_body(&serde_json::json!({
            "success": true,
            "data": {
                "build_identity": {"display": "homeboy 0.281.2+b078972b3edd"}
            }
        })),
        Some("homeboy 0.281.2+b078972b3edd")
    );
    assert_eq!(
        daemon_identity_from_body(&serde_json::json!({"version":"0.228.13"})),
        None
    );
}

#[test]
fn parses_self_identity_json_envelope() {
    let identity = parse_self_identity_output(
        r#"{"success":true,"data":{"version":"0.228.13","display":"homeboy 0.228.13+19a41cd5102d"}}"#,
    )
    .expect("identity");

    assert_eq!(identity.version, "0.228.13");
    assert_eq!(identity.display, "homeboy 0.228.13+19a41cd5102d");
}

#[test]
fn stale_daemon_warning_includes_ordered_restart_recovery_commands() {
    let warning = RunnerStaleDaemonWarning::new(
        "homeboy-lab",
        "homeboy 0.201.3".to_string(),
        "homeboy 0.204.0".to_string(),
        Some("homeboy 0.201.3+old".to_string()),
        Some("homeboy 0.204.0+new".to_string()),
    );

    assert_eq!(warning.session_homeboy_version, "homeboy 0.201.3");
    assert_eq!(warning.current_homeboy_version, "homeboy 0.204.0");
    assert_eq!(warning.severity, "warning");
    assert_eq!(
        warning.active_daemon_control_plane_version,
        "homeboy 0.201.3"
    );
    assert_eq!(warning.job_command_binary_version, "homeboy 0.204.0");
    assert_eq!(
        warning.session_homeboy_build_identity.as_deref(),
        Some("homeboy 0.201.3+old")
    );
    assert_eq!(
        warning
            .active_daemon_control_plane_build_identity
            .as_deref(),
        Some("homeboy 0.201.3+old")
    );
    assert_eq!(
        warning.job_command_binary_build_identity.as_deref(),
        Some("homeboy 0.204.0+new")
    );
    assert_eq!(
        warning.refresh_command,
        format!(
            "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect && homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert!(warning.message.contains("daemon control plane"));
    assert!(warning.message.contains("job command binary"));
    assert!(warning.message.contains("active jobs are drained"));
    assert_eq!(
        warning.recovery_commands,
        vec![
            format!(
                "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect",
                env!("CARGO_PKG_VERSION")
            ),
            "homeboy runner disconnect homeboy-lab".to_string(),
            "homeboy runner connect homeboy-lab".to_string(),
        ]
    );
}

#[test]
fn parses_daemon_runtime_stale_paths_from_version_body() {
    let paths = daemon_runtime_stale_paths_from_body(&serde_json::json!({
        "version": "0.228.13",
        "runtime_paths": {
            "stale": [{
                "env": "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH",
                "path": "/home/chubes/Developer/sample-runtime",
                "loaded_fingerprint": "files=10",
                "current_fingerprint": "files=11"
            }]
        }
    }));

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].env, "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH");
    assert_eq!(paths[0].loaded_fingerprint, "files=10");
    assert_eq!(paths[0].current_fingerprint, "files=11");
}

#[test]
fn changed_runtime_paths_reports_runner_config_changes_since_daemon_start() {
    let mut runner_env = HashMap::new();
    runner_env.insert(
        "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
        "/home/chubes/Developer/sample-runtime@new".to_string(),
    );
    let loaded = daemon_runtime_loaded_paths_from_body(&serde_json::json!({
        "runtime_paths": {
            "loaded": [{
                "env": "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH",
                "path": "/home/chubes/Developer/sample-runtime@old",
                "fingerprint": "files=10"
            }]
        }
    }));

    let changed = changed_runtime_paths(&runner_env, &loaded);

    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].env, "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH");
    assert_eq!(
        changed[0].loaded_path.as_deref(),
        Some("/home/chubes/Developer/sample-runtime@old")
    );
    assert_eq!(
        changed[0].configured_path.as_deref(),
        Some("/home/chubes/Developer/sample-runtime@new")
    );
}

#[test]
fn runtime_path_warning_uses_rebuild_specific_message() {
    let warning = RunnerStaleDaemonWarning::new(
        "homeboy-lab",
        "0.228.13".to_string(),
        "0.228.13".to_string(),
        Some("homeboy 0.228.13+same".to_string()),
        Some("homeboy 0.228.13+same".to_string()),
    )
    .with_runtime_paths(
        "homeboy-lab",
        vec![RunnerStaleRuntimePath {
            env: "HOMEBOY_SAMPLE_RUNTIME_COMPONENT_PATH".to_string(),
            path: "/home/chubes/Developer/sample-runtime".to_string(),
            loaded_fingerprint: "files=10".to_string(),
            current_fingerprint: "files=11".to_string(),
        }],
        Vec::new(),
    );

    assert!(warning.message.contains("runner-side rebuilds"));
    assert_eq!(
        warning.recovery_commands,
        vec![
            "homeboy runner disconnect homeboy-lab".to_string(),
            "homeboy runner connect homeboy-lab".to_string(),
        ]
    );
}

#[test]
fn parses_remote_daemon_status_lease_as_single_source_of_truth() {
    let envelope = parse_envelope(
        r#"{"success":true,"data":{"action":"status","running":true,"fresh":true,"reachable":true,"state":{"lease_id":"lease-1","address":"127.0.0.1:49152","pid":123}}}"#,
    )
    .expect("parse envelope");
    let data = envelope.data.expect("status data");
    let state = data.get("state").expect("lease state");

    assert!(data.get("running").and_then(Value::as_bool).unwrap());
    assert_eq!(
        state.get("lease_id").and_then(Value::as_str),
        Some("lease-1")
    );
    assert_eq!(
        state.get("address").and_then(Value::as_str),
        Some("127.0.0.1:49152")
    );
}

#[test]
fn routine_disconnect_refuses_an_unbound_session_without_executing_a_configured_binary() {
    let mut session = direct_ssh_session("lease-live");
    session.local_url = None;

    let error = stop_transport_recovery::disconnect_remote_daemon(&session, false)
        .expect_err("routine disconnect must require the live daemon tunnel and exact lease");

    assert!(error.contains("no live daemon tunnel"));
}

#[test]
fn routine_disconnect_posts_the_exact_live_lease_to_the_daemon_tunnel() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request");
        let mut request = [0; 4096];
        let length = stream.read(&mut request).expect("read request");
        let request = String::from_utf8(request[..length].to_vec()).expect("request text");
        assert!(request.starts_with("POST /lifecycle/stop HTTP/1.1"));
        assert!(request.contains("\"lease_id\":\"lease-live\""));
        assert!(request.contains("\"force\":false"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .expect("response");
    });
    let mut session = direct_ssh_session("lease-live");
    session.local_url = Some(format!("http://{address}"));

    stop_transport_recovery::disconnect_remote_daemon(&session, false)
        .expect("guarded lifecycle stop accepted");
    server.join().expect("server");
}

#[test]
fn refresh_disconnect_refuses_remote_lease_drift_after_promotion_started() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
            .expect("create runner");
        let recorded = direct_ssh_session("lease-original");
        let mut replacement = direct_ssh_session("lease-replacement");
        replacement.local_url = Some("http://127.0.0.1:9".to_string());
        write_session(&replacement).expect("record replacement session");

        let error = disconnect_with_session("homeboy-lab", Some(&recorded), false)
            .expect_err("refresh must not stop through a different tunnel");

        assert!(error.message.contains("remote daemon ownership changed"));
        assert_eq!(
            read_session("homeboy-lab")
                .expect("read session")
                .expect("session")
                .remote_daemon_lease_id
                .as_deref(),
            Some("lease-replacement")
        );
    });
}

#[test]
fn refresh_disconnect_accepts_local_tunnel_rotation_and_uses_the_current_tunnel() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
            .expect("create runner");
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request through rotated tunnel");
            let mut request = [0; 4096];
            let length = stream.read(&mut request).expect("read request");
            let request = String::from_utf8(request[..length].to_vec()).expect("request text");
            assert!(request.starts_with("POST /lifecycle/stop HTTP/1.1"));
            assert!(request.contains("\"lease_id\":\"lease-stable\""));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .expect("response");
        });
        let recorded = direct_ssh_session("lease-stable");
        let mut rotated = recorded.clone();
        rotated.local_port = Some(address.port());
        rotated.local_url = Some(format!("http://{address}"));
        rotated.tunnel_pid = None;
        rotated.connected_at = "2026-07-15T23:00:00Z".to_string();
        rotated.homeboy_build_identity = Some("homeboy test+rotated".to_string());
        write_session(&rotated).expect("record rotated tunnel");

        disconnect_with_session("homeboy-lab", Some(&recorded), false)
            .expect("stable remote daemon can be stopped through the current tunnel");
        server.join().expect("server");
        assert!(read_session("homeboy-lab").expect("read session").is_none());
    });
}

#[test]
fn refresh_disconnect_refuses_remote_address_or_pid_drift() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
            .expect("create runner");
        let recorded = direct_ssh_session("lease-stable");

        for (field, mut replacement) in [("address", recorded.clone()), ("pid", recorded.clone())] {
            match field {
                "address" => {
                    replacement.remote_daemon_address = Some("127.0.0.1:49154".to_string())
                }
                "pid" => replacement.remote_daemon_pid = Some(4243),
                _ => unreachable!(),
            }
            write_session(&replacement).expect("record remote drift");

            let error = disconnect_with_session("homeboy-lab", Some(&recorded), false)
                .expect_err("remote ownership drift is refused");
            assert!(error.message.contains("remote daemon ownership changed"));
        }
    });
}

#[test]
fn test_open_loopback_tunnel_noops_for_local_runner() {
    let server = Server {
        id: "local".to_string(),
        aliases: Vec::new(),
        host: "127.0.0.1".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        kind: None,
        auth: None,
        env: HashMap::new(),
        runner: None,
    };

    let tunnel = open_loopback_tunnel(&server, 49100, "127.0.0.1", 49200);

    assert!(tunnel.success);
    assert_eq!(tunnel.pid, None);
    assert_eq!(tunnel.stderr, "");
}

#[test]
fn connect_reports_local_runner_as_unsupported() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create runner");

        let (report, exit_code) = connect("lab-local").expect("connect report");

        assert_eq!(exit_code, 20);
        assert!(!report.connected);
        assert_eq!(report.failure_kind, Some(RunnerFailureKind::SshFailure));
        assert!(report
            .failure_message
            .as_deref()
            .unwrap_or_default()
            .contains("only SSH runners"));
    });
}

#[test]
fn disconnect_removes_existing_session_file() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("create runner");
        // A reverse-tunnel session: disconnect removes local session state
        // without the direct-SSH remote-daemon-stop path, which (since
        // #8250) refuses to unbind a leaseless direct-SSH daemon. This test
        // asserts local session-file cleanup, not remote stop.
        let session = RunnerSession {
            runner_id: "lab-local".to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:49152".to_string()),
            local_port: Some(49153),
            local_url: Some("http://127.0.0.1:49153".to_string()),
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+abc123".to_string()),
            connected_at: Utc::now().to_rfc3339(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        };
        write_session(&session).expect("write session");
        let path = session_path("lab-local").expect("session path");
        assert!(path.exists());

        let report = disconnect("lab-local").expect("disconnect");

        assert!(report.disconnected);
        assert_eq!(report.session.expect("session").runner_id, "lab-local");
        assert!(!path.exists());
    });
}

#[test]
fn records_reverse_runner_session_without_marking_transport_live() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(
            r#"{"id":"homeboy-lab","kind":"local","workspace_root":"/home/user/Developer"}"#,
            false,
        )
        .expect("create runner");

        let (report, exit_code) = connect_reverse(ReverseRunnerConnectOptions {
            controller_id: "extra-chill".to_string(),
            runner_id: "homeboy-lab".to_string(),
            broker_url: None,
        })
        .expect("record reverse session");

        assert_eq!(exit_code, 0);
        assert!(!report.connected);
        assert_eq!(report.recorded, Some(true));
        assert_eq!(report.mode, Some(RunnerTunnelMode::Reverse));
        assert_eq!(report.role, Some(RunnerSessionRole::Runner));
        assert_eq!(report.controller_id.as_deref(), Some("extra-chill"));

        let status = status("homeboy-lab").expect("status");
        assert!(!status.connected);
        assert_eq!(status.state, RunnerSessionState::Recorded);
        let session = status.session.expect("session");
        assert_eq!(session.mode, RunnerTunnelMode::Reverse);
        assert_eq!(session.role, RunnerSessionRole::Runner);
        assert_eq!(session.controller_id.as_deref(), Some("extra-chill"));
        assert_eq!(session.broker_url, None);
        assert_eq!(session.local_url, None);
        assert_eq!(session.local_port, None);
    });
}

#[test]
fn status_lists_reverse_session_records() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
            .expect("create runner");
        connect_reverse(ReverseRunnerConnectOptions {
            controller_id: "extra-chill".to_string(),
            runner_id: "homeboy-lab".to_string(),
            broker_url: None,
        })
        .expect("record reverse session");

        let reports = statuses().expect("statuses");
        let report = reports
            .iter()
            .find(|report| report.runner_id == "homeboy-lab")
            .expect("homeboy-lab status");

        assert_eq!(report.state, RunnerSessionState::Recorded);
    });
}

#[test]
fn status_marks_connected_reverse_active_jobs_unavailable_without_broker_url() {
    test_support::with_isolated_home(|_| {
        crate::runner::create(r#"{"id":"homeboy-lab","kind":"local"}"#, false)
            .expect("create runner");
        let mut session = reverse_controller_session();
        session.broker_url = None;
        write_session(&session).expect("write session");

        let report = status("homeboy-lab").expect("status");

        assert!(report.connected);
        assert_eq!(report.active_job_count, 0);
        assert_eq!(report.active_jobs, Vec::new());
        assert_eq!(report.active_runner_jobs, Vec::new());
        assert_eq!(report.active_job_state, RunnerActiveJobState::Unavailable);
        assert_eq!(report.active_job_source, None);
        let error = report.active_job_error.expect("active job error");
        assert_eq!(error.code, "internal.unexpected");
        assert!(error.message.contains("no broker URL"));
    });
}

#[test]
fn child_run_matching_accepts_durable_run_id_or_command_reference() {
    let run = sample_run_summary("run-child-1");
    let by_durable_id = sample_active_job(Some("run-child-1"), "homeboy test wpcom");
    let by_command = sample_active_job(None, "homeboy test wpcom --run run-child-1");
    let unrelated = sample_active_job(Some("other-run"), "homeboy test wpcom");

    assert!(child_run_has_active_job(&run, &[by_durable_id]));
    assert!(child_run_has_active_job(&run, &[by_command]));
    assert!(!child_run_has_active_job(&run, &[unrelated]));
}

#[test]
fn orphaned_child_run_job_reports_stale_retryable_runner_state() {
    let job = orphaned_child_run_job("homeboy-lab", sample_run_summary("run-child-1"));

    assert_eq!(job.runner_id, "homeboy-lab");
    assert_eq!(job.job_id, "orphaned-child-run-run-child-1");
    assert_eq!(job.source, "runner-observation");
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(job.durable_run_id.as_deref(), Some("run-child-1"));
    assert_eq!(
        job.stale_reason.as_deref(),
        Some("child_run_running_without_active_runner_job")
    );
    assert_eq!(job.lifecycle_state.as_deref(), Some("recoverable_orphan"));
    assert_eq!(job.retryable, Some(true));
}

#[test]
fn direct_daemon_fresh_live_job_suppresses_false_orphan_inference() {
    assert!(should_infer_child_run_orphans(0, Some(0)));
    assert!(should_infer_child_run_orphans(1, Some(1)));
    assert!(should_infer_child_run_orphans(0, None));
    assert!(
        !should_infer_child_run_orphans(0, Some(1)),
        "a fresh daemon heartbeat for an untyped child is authoritative live evidence"
    );
}

#[test]
fn synthetic_active_job_run_summaries_are_not_child_runs() {
    let mut synthetic = sample_run_summary("runner-job-job-1");
    synthetic.status_note =
        Some("active runner job: source=direct-daemon kind=test runner=homeboy-lab".to_string());

    assert!(is_synthetic_active_job_run_summary(&synthetic));
    assert!(!is_synthetic_active_job_run_summary(&sample_run_summary(
        "run-child-1"
    )));
}

#[test]
fn active_runner_job_source_maps_direct_and_reverse_endpoints() {
    let mut direct = reverse_controller_session();
    direct.mode = RunnerTunnelMode::DirectSsh;
    direct.role = RunnerSessionRole::Controller;
    direct.local_url = Some("http://127.0.0.1:49153".to_string());
    direct.broker_url = None;
    assert_eq!(
        active_runner_job_source(&direct),
        Some(RunnerActiveJobSource::DirectDaemon)
    );

    let reverse = reverse_controller_session();
    assert_eq!(
        active_runner_job_source(&reverse),
        Some(RunnerActiveJobSource::ReverseBroker)
    );
}

#[test]
fn reverse_controller_session_requires_fresh_heartbeat() {
    let mut session = reverse_controller_session();

    assert_eq!(session_state(Some(&session)), RunnerSessionState::Connected);

    session.last_seen_at = Some((Utc::now() - chrono::Duration::seconds(120)).to_rfc3339());
    assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);

    session.last_seen_at = None;
    assert_eq!(session_state(Some(&session)), RunnerSessionState::Recorded);
}

#[test]
fn sessionless_active_daemon_reattaches_only_with_matching_endpoint_identity() {
    let mut status = remote_daemon_status_for_test(true, true, 2, "lease-live", 1183765);
    let daemon = status.daemon.as_mut().expect("daemon");
    daemon.version = Some("0.284.0".to_string());
    daemon.build_identity = Some("homeboy 0.284.0+live".to_string());

    assert_eq!(
        remote_daemon_connect_action_with_controller_identity(
            None,
            &status,
            "homeboy 0.284.0+live"
        )
        .expect("matching controller reattaches"),
        RemoteDaemonConnectAction::Reattach
    );

    let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);
    assert_eq!(recovery.daemon_version.as_deref(), Some("0.284.0"));
    assert_eq!(
        recovery.daemon_build_identity.as_deref(),
        Some("homeboy 0.284.0+live")
    );
}

#[test]
fn sessionless_active_daemon_prescribes_matching_pinned_controller_on_identity_mismatch() {
    let mut status = remote_daemon_status_for_test(true, true, 2, "lease-live", 1183765);
    let daemon = status.daemon.as_mut().expect("daemon");
    daemon.version = Some("0.284.0".to_string());
    daemon.build_identity = Some("homeboy 0.284.0+live".to_string());

    let error = remote_daemon_connect_action_with_controller_identity(
        None,
        &status,
        "homeboy 0.284.0+other",
    )
    .expect_err("mismatched controller must not replace an active daemon");

    assert!(error.contains("Run a controller pinned to `homeboy 0.284.0+live`"));
    assert!(error.contains("refusing replacement"));
}

#[test]
fn sessionless_active_daemon_fails_closed_when_endpoint_identity_is_ambiguous() {
    let status = remote_daemon_status_for_test(true, true, 2, "lease-live", 1183765);

    let error = remote_daemon_connect_action_with_controller_identity(
        None,
        &status,
        "homeboy 0.284.0+live",
    )
    .expect_err("missing endpoint identity must not authorize reattachment");

    assert!(error.contains("did not provide a build identity"));
}
