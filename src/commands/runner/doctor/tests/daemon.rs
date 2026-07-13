use super::super::*;
use types::RunnerDoctorStatus;

#[test]
fn daemon_exec_probe_reports_structured_failure() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = [0; 4096];
        let _ = std::io::Read::read(&mut stream, &mut buffer).expect("read request");
        let body = serde_json::json!({
            "success": false,
            "data": {
                "error": "validation.invalid_argument",
                "message": "Invalid argument 'runner': stale daemon session"
            },
            "error": {
                "error": "validation.invalid_argument",
                "message": "Invalid argument 'runner': stale daemon session"
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
    });

    let check = probes::daemon_exec_check(
        "homeboy-lab",
        "/home/user/Developer",
        &format!("http://{addr}"),
    );

    assert_eq!(check.id, "daemon.exec");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check.message.contains("failed the lightweight exec probe"));
    assert!(check
        .details
        .get("response")
        .expect("response detail")
        .contains("validation.invalid_argument"));
    assert!(check
        .remediation
        .expect("remediation")
        .contains("homeboy runner connect homeboy-lab"));
}

#[test]
fn ssh_target_uses_runner_env_for_remote_probes() {
    crate::test_support::with_isolated_home(|_| {
        server::create(
            r#"{
                "id":"lab",
                "host":"localhost",
                "user":"tester",
                "env":{"PATH":"/server/bin:$PATH"}
            }"#,
            false,
        )
        .expect("create server");
        runner::create(
            r#"{
                "id":"lab",
                "kind":"ssh",
                "server_id":"lab",
                "workspace_root":"/tmp",
                "env":{"PATH":"/runner/bin:$PATH"}
            }"#,
            false,
        )
        .expect("create runner");

        let target = target::resolve("lab").expect("resolve runner target");
        let target::RunnerTarget::Ssh { client, .. } = target else {
            panic!("expected ssh target");
        };

        assert_eq!(
            client.env.get("PATH").map(String::as_str),
            Some("/runner/bin:$PATH")
        );
    });
}

#[test]
fn remote_default_artifact_root_expands_under_home() {
    assert_eq!(
        remote::default_artifact_root_for_home("/home/runner"),
        Some("/home/runner/.local/share/homeboy/artifacts".to_string())
    );
}

#[test]
fn remote_default_artifact_root_normalizes_trailing_home_slash() {
    assert_eq!(
        remote::default_artifact_root_for_home("/Users/user/"),
        Some("/Users/user/.local/share/homeboy/artifacts".to_string())
    );
}

#[test]
fn remote_default_artifact_root_rejects_empty_home() {
    assert_eq!(remote::default_artifact_root_for_home("  "), None);
}

#[test]
fn disconnected_lab_doctor_reuses_daemon_recovery_envelope() {
    let recovery = homeboy::core::daemon::DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(homeboy::core::daemon::DaemonStaleReasonCode::PidDead),
        restartable: false,
        lease_id: Some("lease-dead".to_string()),
        pid: Some(4545),
        recovery_evidence: Some(homeboy::core::daemon::DaemonRecoveryEvidence::ProvenDead),
        ownership_evidence: Some(
            "remote daemon status over SSH proved PID 4545 is dead".to_string(),
        ),
        adoption_command: Some(
            "homeboy runner connect lab --adopt-orphan-lease lease-dead --confirm-pid-dead"
                .to_string(),
        ),
        binary_hash: None,
        runtime_paths: None,
        active_jobs: 1,
        repair_plan: Vec::new(),
    };
    let runner = Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some("lab".to_string()),
        workspace_root: None,
        settings: server::RunnerSettings::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: server::RunnerPolicy::default(),
    };
    let server = server::Server {
        id: "lab".to_string(),
        aliases: Vec::new(),
        host: "example.test".to_string(),
        user: "runner".to_string(),
        port: 22,
        identity_file: None,
        kind: None,
        auth: None,
        env: Default::default(),
        runner: None,
    };

    let report = remote::disconnected_report("lab", &runner, &server, Some(recovery));

    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].id, "daemon.recovery");
    assert_eq!(report.daemon_recovery.expect("recovery").active_jobs, 1);
}
