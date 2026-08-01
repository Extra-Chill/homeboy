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
fn stalled_daemon_probe_returns_timeout_reason_code() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("accept");
        std::thread::sleep(std::time::Duration::from_secs(1));
    });

    let check = probes::daemon_exec_check_with_timeout(
        "homeboy-lab",
        "/home/user/Developer",
        &format!("http://{addr}"),
        std::time::Duration::from_millis(50),
    );

    assert_eq!(check.id, "daemon.exec");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert_eq!(
        check.details.get("reason_code").map(String::as_str),
        Some("runner_doctor.daemon_timeout")
    );
    assert_eq!(
        check.details.get("timeout_ms").map(String::as_str),
        Some("50")
    );
}

#[test]
fn exhausted_overall_budget_skips_daemon_probe_with_stable_reason_code() {
    let check = probes::daemon_exec_check_with_timeout(
        "homeboy-lab",
        "/home/user/Developer",
        "http://127.0.0.1:9",
        std::time::Duration::ZERO,
    );

    assert_eq!(check.id, "daemon.exec");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert_eq!(
        check.details.get("reason_code").map(String::as_str),
        Some("runner_doctor.overall_timeout")
    );
    assert_eq!(
        check.details.get("timeout_ms").map(String::as_str),
        Some("0")
    );
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
        daemon_version: Some("0.284.0".to_string()),
        daemon_build_identity: Some("homeboy 0.284.0+live".to_string()),
        runtime_paths: None,
        active_jobs: 1,
        termination_evidence: None,
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
    assert_eq!(report.checks[0].details["daemon_version"], "0.284.0");
    assert_eq!(
        report.checks[0].details["daemon_build_identity"],
        "homeboy 0.284.0+live"
    );
    assert_eq!(report.daemon_recovery.expect("recovery").active_jobs, 1);
}

/// #11103: the repair plan was computed everywhere and executed nowhere.
/// `--repair` refused most scopes outright and, for the one scope it handled,
/// emitted a fixed disconnect/connect pair regardless of what the report said.
/// These pin the dispatch itself: code in, typed action out, no shell parsing.
mod repair_dispatch {
    use super::super::super::repair::{dispatch_for, DaemonRepairDispatch};
    use homeboy::core::daemon::DaemonRepairStep;
    use homeboy::core::error::{ActionSafety, ExecutableAction};
    use homeboy::runner::runners::daemon_repair_codes;

    fn action(args: &[&str]) -> ExecutableAction {
        ExecutableAction::new(
            "runner.refresh_homeboy",
            "refresh",
            "homeboy",
            args.iter().map(|arg| arg.to_string()),
            ActionSafety::Mutating,
        )
    }

    /// The live reproduction on the machine that motivated this fix: a
    /// reachable daemon whose build identity no longer matches the binary. It
    /// must reach the binary refresh, not a session reconnect that would
    /// reproduce the same mismatch.
    #[test]
    fn a_version_mismatch_step_dispatches_to_a_binary_refresh() {
        let step = DaemonRepairStep::executable(
            daemon_repair_codes::RUNNER_REFRESH_HOMEBOY,
            action(&["runner", "refresh-homeboy", "lab", "--reconnect"]),
        );

        assert_eq!(
            dispatch_for(&step, Some("lease-live")),
            DaemonRepairDispatch::RefreshHomeboy {
                git_ref: None,
                allow_downgrade: false,
            }
        );
    }

    /// The recovery ref and the downgrade allowance come from the step's argv,
    /// which is the authoritative form. Nothing re-parses the rendered command.
    #[test]
    fn refresh_arguments_are_read_from_argv_not_from_the_rendered_command() {
        let step = DaemonRepairStep::executable(
            daemon_repair_codes::RUNNER_REFRESH_HOMEBOY,
            action(&[
                "runner",
                "refresh-homeboy",
                "lab",
                "--ref",
                "abc123",
                "--reconnect",
                "--allow-downgrade",
            ]),
        );

        assert_eq!(
            dispatch_for(&step, None),
            DaemonRepairDispatch::RefreshHomeboy {
                git_ref: Some("abc123".to_string()),
                allow_downgrade: true,
            }
        );
    }

    /// The lease comes from the report's typed evidence, never from the text.
    #[test]
    fn an_adoption_step_takes_its_lease_from_the_report() {
        let step = DaemonRepairStep::text(
            daemon_repair_codes::RUNNER_ADOPT_ORPHAN_LEASE,
            "homeboy runner connect lab --adopt-orphan-lease lease-in-text --confirm-pid-dead",
        );

        assert_eq!(
            dispatch_for(&step, Some("lease-from-report")),
            DaemonRepairDispatch::AdoptOrphanLease {
                lease_id: "lease-from-report".to_string(),
            },
            "the lease must come from typed evidence, not from the command string"
        );
    }

    #[test]
    fn an_adoption_step_without_a_lease_refuses_rather_than_guessing() {
        let step = DaemonRepairStep::text(
            daemon_repair_codes::RUNNER_ADOPT_ORPHAN_LEASE,
            "homeboy runner connect lab --adopt-orphan-lease lease-in-text --confirm-pid-dead",
        );

        let DaemonRepairDispatch::NotAutomatable { reason } = dispatch_for(&step, None) else {
            panic!("an adoption with no lease must not be executed");
        };
        assert!(reason.contains("no lease id"), "{reason}");
    }

    /// The empty-plan case. A report matching no repair branch now yields a
    /// read-only diagnosis, and the executor must surface it with an explicit
    /// reason instead of either running it as a repair or saying nothing.
    #[test]
    fn an_unmatched_report_yields_an_explicit_refusal_not_silence() {
        let step = DaemonRepairStep::text(
            daemon_repair_codes::RUNNER_DIAGNOSE,
            "homeboy runner doctor lab --scope lab-offload",
        );

        let DaemonRepairDispatch::NotAutomatable { reason } =
            dispatch_for(&step, Some("lease-live"))
        else {
            panic!("a read-only diagnosis is not a repair to apply");
        };
        assert!(reason.contains("authorizes no mutation"), "{reason}");
        assert!(!reason.is_empty());
    }

    /// A recovery command a runner advertised as text has no argv behind it, so
    /// it is reported rather than executed. It must not fall through silently.
    #[test]
    fn an_untyped_recovery_command_is_reported_rather_than_executed() {
        let step = DaemonRepairStep::text(
            daemon_repair_codes::STALE_DAEMON_RECOVERY,
            "homeboy runner refresh-homeboy lab --ref abc123 --reconnect",
        );

        let DaemonRepairDispatch::NotAutomatable { reason } = dispatch_for(&step, None) else {
            panic!("an untyped step must never be shell-parsed into an execution");
        };
        assert!(
            reason.contains(daemon_repair_codes::STALE_DAEMON_RECOVERY),
            "{reason}"
        );
    }

    #[test]
    fn the_generic_reconnect_plan_still_dispatches_step_by_step() {
        assert_eq!(
            dispatch_for(
                &DaemonRepairStep::text(
                    daemon_repair_codes::RUNNER_DISCONNECT,
                    "homeboy runner disconnect lab"
                ),
                None
            ),
            DaemonRepairDispatch::Disconnect
        );
        assert_eq!(
            dispatch_for(
                &DaemonRepairStep::text(
                    daemon_repair_codes::RUNNER_CONNECT,
                    "homeboy runner connect lab"
                ),
                None
            ),
            DaemonRepairDispatch::Connect
        );
        assert_eq!(
            dispatch_for(
                &DaemonRepairStep::text(
                    daemon_repair_codes::RUNNER_RECONCILE_LEASELESS_ORPHANS,
                    "homeboy runner connect lab --reconcile-leaseless-orphans --confirm-no-daemon-owner"
                ),
                None
            ),
            DaemonRepairDispatch::ReconcileLeaselessOrphans
        );
    }
}
