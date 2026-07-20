#![cfg(test)]

use clap::Parser;
use std::collections::HashMap;
use std::io::{Read, Write};

use super::*;

#[test]
fn rejects_non_loopback_remote_daemon_address() {
    assert!(parse_loopback_daemon_addr("0.0.0.0:1234").is_err());
    assert!(parse_loopback_daemon_addr("127.0.0.1:1234").is_ok());
}

#[test]
fn parses_daemon_status_envelope() {
    let envelope = parse_envelope(
        r#"{"success":true,"data":{"action":"status","running":true,"state":{"address":"127.0.0.1:49152","pid":123}}}"#,
    )
    .expect("parse envelope");

    assert!(envelope.success);
    assert_eq!(
        envelope
            .data
            .unwrap()
            .get("state")
            .unwrap()
            .get("address")
            .unwrap(),
        "127.0.0.1:49152"
    );
}

#[test]
fn reads_remote_active_job_count_from_daemon_freshness() {
    let status = serde_json::json!({
        "freshness": { "active_jobs": 2 }
    });

    assert_eq!(remote_daemon_active_jobs(&status), 2);
}

#[test]
fn reattaches_active_daemon_without_changing_lease_or_pid() {
    let session = direct_ssh_session("lease-active");
    let status = remote_daemon_status_for_test(true, true, 1, "lease-active", 4242);

    let action = remote_daemon_connect_action(Some(&session), &status).expect("reattach");

    assert_eq!(action, RemoteDaemonConnectAction::Reattach);
    let daemon = status.daemon.expect("daemon");
    assert_eq!(daemon.lease_id.as_deref(), Some("lease-active"));
    assert_eq!(daemon.pid, Some(4242));
    assert_eq!(
        status.active_jobs, 1,
        "active job must not trigger replacement"
    );
}

#[test]
fn tunnel_only_failure_reattaches_the_persisted_daemon() {
    let mut session = direct_ssh_session("lease-tunnel");
    session.tunnel_pid = Some(999_999);
    session.local_url = Some("http://127.0.0.1:1".to_string());
    let status = remote_daemon_status_for_test(true, true, 0, "lease-tunnel", 4343);

    assert_eq!(
        remote_daemon_connect_action(Some(&session), &status).expect("reattach"),
        RemoteDaemonConnectAction::Reattach
    );
}

#[test]
fn stale_daemon_without_a_matching_session_fails_closed_without_replacement() {
    let status = remote_daemon_status_for_test(false, true, 0, "lease-stale", 4444);

    assert!(remote_daemon_connect_action(None, &status)
        .expect_err("stale daemon ownership is unknown")
        .contains("--adopt-live-lease lease-stale --expected-live-pid 4444"));
}

fn idle_stale_status(work_evidence: RemoteDaemonWorkEvidence) -> RemoteDaemonStatus {
    let mut status = remote_daemon_status_for_test_with_reason(
        false,
        true,
        0,
        "lease-stale",
        4444,
        Some(DaemonStaleReasonCode::VersionMismatch),
    );
    let daemon = status.daemon.as_mut().expect("daemon");
    daemon.version = Some("0.288.13".to_string());
    daemon.build_identity = Some("homeboy 0.288.13+stale".to_string());
    status.work_evidence = work_evidence;
    status
}

#[test]
fn replaces_idle_stale_daemon_when_typed_jobs_are_zero_without_lease_recovery_evidence() {
    let status = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);

    assert_eq!(
        remote_daemon_connect_action_with_controller_identity(
            Some(&direct_ssh_session("lease-stale")),
            &status,
            "homeboy 0.289.0+configured",
        )
        .expect("bounded stale replacement"),
        RemoteDaemonConnectAction::ReplaceIdleStale,
    );
    let freshness = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);
    assert!(!freshness.restartable, "lease evidence remains unavailable");
    assert_eq!(freshness.active_jobs, 0);
}

#[test]
fn reconciles_multiple_stale_generation_leases_to_one_authoritative_idle_lease() {
    let status = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);

    assert_eq!(
        authoritative_idle_lease_for_stale_generations(
            &status,
            &[
                "lease-a".to_string(),
                "lease-b".to_string(),
                "lease-c".to_string()
            ],
        )
        .expect("authoritative idle lease is bounded"),
        Some("lease-stale".to_string())
    );
}

#[test]
fn stale_generation_reconciliation_requires_every_ledger_entry_to_be_direct_and_leased() {
    let first = direct_ssh_session("lease-a");
    let second = direct_ssh_session("lease-b");
    let mut mixed = direct_ssh_session("lease-b");
    mixed.mode = RunnerTunnelMode::Reverse;
    let mut missing = direct_ssh_session("lease-b");
    missing.remote_daemon_lease_id = None;
    let mut empty = direct_ssh_session("lease-b");
    empty.remote_daemon_lease_id = Some(String::new());

    assert_eq!(
        super::super::stop_transport_recovery::eligible_stale_generation_leases(&[first, second]),
        Some(vec!["lease-a".to_string(), "lease-b".to_string()])
    );
    assert_eq!(
        super::super::stop_transport_recovery::eligible_stale_generation_leases(&[
            direct_ssh_session("lease-a"),
            mixed,
        ]),
        None
    );
    assert_eq!(
        super::super::stop_transport_recovery::eligible_stale_generation_leases(&[
            direct_ssh_session("lease-a"),
            missing,
        ]),
        None
    );
    assert_eq!(
        super::super::stop_transport_recovery::eligible_stale_generation_leases(&[
            direct_ssh_session("lease-a"),
            empty,
        ]),
        None
    );
}

#[test]
fn stale_generation_reconciliation_refuses_active_jobs_changed_lease_or_unproven_owner() {
    let mut active = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);
    active.active_jobs = 1;
    let changed = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);
    let mut unproven = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);
    unproven.endpoint_probe_error = Some("identity unavailable".to_string());

    assert!(
        authoritative_idle_lease_for_stale_generations(&active, &["lease-a".to_string()]).is_err()
    );
    assert_eq!(
        authoritative_idle_lease_for_stale_generations(&changed, &["lease-stale".to_string()])
            .expect("matching lease remains under normal fencing"),
        None
    );
    assert!(
        authoritative_idle_lease_for_stale_generations(&unproven, &["lease-a".to_string()])
            .is_err()
    );
}

#[test]
fn stale_generation_reconciliation_refuses_a_lease_change_after_stop() {
    let stopped = remote_daemon_status_for_test_with_reason(
        false,
        false,
        0,
        "lease-stale",
        4444,
        Some(DaemonStaleReasonCode::PidDead),
    );
    let changed = remote_daemon_status_for_test(true, true, 0, "lease-raced", 5555);

    assert!(authoritative_lease_stop_confirmed(&stopped, "lease-stale").is_ok());
    assert!(authoritative_lease_stop_confirmed(&changed, "lease-stale")
        .expect_err("a new lease during recovery is not the stopped lease")
        .contains("ownership changed"));
}

#[test]
fn idle_stale_replacement_fails_closed_for_active_or_inconsistent_typed_jobs() {
    let configured = "homeboy 0.289.0+configured";
    let mut freshness_active = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);
    freshness_active.active_jobs = 1;
    let typed_active = idle_stale_status(RemoteDaemonWorkEvidence::ActiveOrUnresolved(1));
    let typed_unknown = idle_stale_status(RemoteDaemonWorkEvidence::Unknown);

    for status in [freshness_active, typed_active, typed_unknown] {
        assert!(
            remote_daemon_connect_action_with_controller_identity(None, &status, configured)
                .expect_err("unsafe evidence fails closed")
                .contains("runner ownership is not proven")
        );
    }
}

#[test]
fn idle_stale_replacement_refuses_unreachable_daemon_evidence() {
    let mut status = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);
    status.reachable = false;

    assert!(remote_daemon_connect_action_with_controller_identity(
        None,
        &status,
        "homeboy 0.289.0+configured",
    )
    .expect_err("unreachable evidence must fail closed")
    .contains("unreachable"));
}

#[test]
fn reconnect_converges_to_configured_identity_and_repeated_recovery_reattaches() {
    let mut status = idle_stale_status(RemoteDaemonWorkEvidence::AuthoritativelyIdle);
    let daemon = status.daemon.as_mut().expect("daemon");
    daemon.version = Some("0.289.0".to_string());
    daemon.build_identity = Some("homeboy 0.289.0+configured".to_string());
    status.fresh = true;
    status.stale_reason = None;
    status.stale_reason_code = None;

    assert_eq!(
        remote_daemon_connect_action_with_controller_identity(
            Some(&direct_ssh_session("lease-stale")),
            &status,
            "homeboy 0.289.0+configured",
        )
        .expect("converged daemon reattaches"),
        RemoteDaemonConnectAction::Reattach,
    );
}

#[test]
fn stale_active_daemon_without_a_matching_session_fails_closed_without_replacing_it() {
    let status = remote_daemon_status_for_test_with_reason(
        false,
        true,
        1,
        "lease-busy",
        4545,
        Some(DaemonStaleReasonCode::VersionMismatch),
    );

    assert!(remote_daemon_connect_action(None, &status)
        .expect_err("stale active daemon ownership is unknown")
        .contains("runner ownership is not proven"));
    let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);
    assert!(!recovery.fresh);
    assert_eq!(recovery.active_jobs, 1);
    assert_eq!(
        recovery.stale_reason_code,
        Some(DaemonStaleReasonCode::VersionMismatch)
    );
    let warning = stale_reattach_warning_for_report("homeboy-lab", &recovery)
        .expect("stale reattach warning");
    assert!(warning.contains("VersionMismatch"));
    assert!(warning.contains("active jobs were preserved"));
    assert!(warning.contains(
        "Continue with `homeboy runner refresh-homeboy homeboy-lab --reconnect` when its work is complete."
    ));
    assert_eq!(
        recovery.adoption_command.as_deref(),
        Some("homeboy runner connect homeboy-lab --reconcile-leaseless-orphans --confirm-no-daemon-owner")
    );
}

#[test]
fn recorded_dead_daemon_with_active_jobs_refuses_implicit_replacement() {
    let session = direct_ssh_session("lease-dead");
    let status = remote_daemon_status_for_test_with_reason(
        false,
        false,
        1,
        "lease-dead",
        4545,
        Some(DaemonStaleReasonCode::PidDead),
    );

    let err = remote_daemon_connect_action(Some(&session), &status)
        .expect_err("require explicit orphan adoption");

    assert!(err.contains("unreachable"));
    assert!(err.contains("1 active job(s) were not replaced"));
    assert!(err.contains("active-job recovery guidance"));
    assert!(err.contains("Inspect `homeboy daemon status`"));
}

#[test]
fn remote_dead_lease_recovery_exposes_exact_adoption_command() {
    let status = remote_daemon_status_for_test_with_reason(
        false,
        false,
        1,
        "lease-dead",
        4545,
        Some(DaemonStaleReasonCode::PidDead),
    );

    let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);

    assert_eq!(recovery.lease_id.as_deref(), Some("lease-dead"));
    assert_eq!(recovery.pid, Some(4545));
    assert_eq!(recovery.active_jobs, 1);
    assert_eq!(
        recovery.recovery_evidence,
        Some(homeboy_core::daemon::DaemonRecoveryEvidence::ProvenDead)
    );
    assert_eq!(
        recovery.adoption_command.as_deref(),
        Some(
            "homeboy runner connect homeboy-lab --adopt-orphan-lease lease-dead --confirm-pid-dead"
        )
    );
}

#[test]
fn remote_status_without_reason_is_evidence_unavailable_and_non_adoptable() {
    let status = remote_daemon_status_for_test(false, false, 1, "lease-unknown", 4545);

    let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);

    assert_eq!(recovery.lease_id.as_deref(), Some("lease-unknown"));
    assert_eq!(recovery.pid, Some(4545));
    assert_eq!(recovery.active_jobs, 1);
    assert_eq!(
        recovery.recovery_evidence,
        Some(homeboy_core::daemon::DaemonRecoveryEvidence::Unavailable)
    );
    assert!(recovery.adoption_command.is_none());
}

#[test]
fn fresh_idle_remote_daemon_is_recoverable_by_reconnect() {
    // #8694: a remote daemon that self-reports fresh with zero authoritatively
    // idle jobs, while the controller session is lost, must be recoverable via
    // a plain reconnect — not reported as a non-restartable dead end with the
    // nonsensical "active jobs are protected" message.
    let mut status = remote_daemon_status_for_test(true, true, 0, "lease-live", 4545);
    status.work_evidence = crate::connection::remote_daemon::RemoteDaemonWorkEvidence::idle();

    let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);

    assert_eq!(recovery.active_jobs, 0);
    assert!(recovery.fresh);
    assert_eq!(
        recovery.recovery_evidence,
        Some(homeboy_core::daemon::DaemonRecoveryEvidence::Recoverable)
    );
    assert_eq!(
        recovery.adoption_command.as_deref(),
        Some("homeboy runner connect homeboy-lab")
    );
    let ownership = recovery
        .ownership_evidence
        .as_deref()
        .expect("recovery guidance");
    assert!(
        ownership.contains("safely reconnected"),
        "ownership evidence must describe the reconnect recovery: {ownership}"
    );
    assert!(
        !ownership.contains("active jobs are protected"),
        "must not claim protected jobs when there are provably zero: {ownership}"
    );
}

#[test]
fn remote_missing_or_corrupt_lease_with_active_jobs_exposes_bounded_reconciliation() {
    for reason in [
        DaemonStaleReasonCode::LeaseMissing,
        DaemonStaleReasonCode::LeaseCorrupt,
    ] {
        let mut status = remote_daemon_status_for_test_with_reason(
            false,
            false,
            1,
            "legacy-lease",
            4545,
            Some(reason),
        );
        status.daemon = None;

        let recovery = remote_daemon_recovery_freshness_from_status("homeboy-lab", &status);

        assert_eq!(recovery.active_jobs, 1, "{reason:?}");
        assert_eq!(
            recovery.recovery_evidence,
            Some(homeboy_core::daemon::DaemonRecoveryEvidence::Unavailable),
            "recovery remains explicit until remote ownership probes pass ({reason:?})"
        );
        assert_eq!(
            recovery.adoption_command.as_deref(),
            Some("homeboy runner connect homeboy-lab --reconcile-leaseless-orphans --confirm-no-daemon-owner"),
            "{reason:?}"
        );
        assert!(recovery
            .ownership_evidence
            .as_deref()
            .expect("recovery guidance")
            .contains("explicit reconciliation"));
    }
}

#[test]
fn unavailable_remote_recovery_is_fail_closed() {
    let recovery = unavailable_recovery_freshness("remote command timed out");

    assert_eq!(
        recovery.stale_reason_code,
        Some(DaemonStaleReasonCode::TransportUnreachable)
    );
    assert_eq!(
        recovery.recovery_evidence,
        Some(homeboy_core::daemon::DaemonRecoveryEvidence::Unavailable)
    );
    assert!(recovery.lease_id.is_none());
    assert!(recovery.pid.is_none());
    assert!(recovery.adoption_command.is_none());
}

#[test]
fn remote_daemon_status_probe_has_a_bounded_deadline() {
    assert_eq!(
        remote_daemon::REMOTE_DAEMON_STATUS_TIMEOUT,
        Duration::from_secs(15)
    );
}

#[test]
fn remote_leaseless_recovery_decodes_and_propagates_report() {
    let envelope = parse_envelope(
        r#"{"success":true,"data":{
        "affected_job_ids": [],
        "affected_job_count": 0,
        "evidence_snapshot_path": "/evidence/jobs.snapshot",
        "ownership_proof": ["owner lock acquired"],
        "retry_guidance": "retry",
        "replacement": {
            "pid": 42,
            "address": "127.0.0.1:7421",
            "state_path": "/state.json",
            "lease_id": "lease-new"
        }
    }}"#,
    )
    .expect("parse daemon envelope");
    let recovery = decode_leaseless_recovery(envelope.data).expect("decode recovery report");
    assert_eq!(recovery.replacement.lease_id, "lease-new");
    assert_eq!(recovery.evidence_snapshot_path, "/evidence/jobs.snapshot");
    assert_eq!(recovery.ownership_proof, vec!["owner lock acquired"]);
}

#[test]
fn state_loss_recovery_delegation_decodes_and_serializes_auditable_evidence() {
    let command =
        remote_state_loss_recovery_command("/opt/homeboy", "lease-old", 4242, "127.0.0.1:7421");
    assert!(command.contains("--recorded-endpoint 127.0.0.1:7421"));
    let envelope = parse_envelope(
        r#"{"success":true,"data":{
        "recovered_lease_id":"lease-old",
        "recorded_dead_pid":4242,
        "recorded_endpoint":"127.0.0.1:7421",
        "affected_job_ids":["7ab96605-38b7-4a6a-bbb8-99db839fa6dc"],
        "affected_job_count":1,
        "evidence_snapshot_path":"/evidence/jobs.snapshot",
        "ownership_proof":["owner lock acquired","endpoint unreachable"],
        "retry_guidance":"retry",
        "replacement":{"pid":43,"address":"127.0.0.1:7422","state_path":"/state.json","lease_id":"lease-new"}
    }}"#,
    )
    .expect("parse daemon envelope");
    let recovery = decode_state_loss_recovery(envelope.data).expect("decode recovery report");
    let (mut report, _) = failed_connect(
        "runner",
        std::path::PathBuf::from("/session.json"),
        RunnerFailureKind::DaemonStartupFailure,
        "test".to_string(),
    );
    report.state_loss_recovery = Some(recovery);
    let data = serde_json::to_value(report).expect("serialize controller report");
    assert_eq!(
        data["state_loss_recovery"]["recovered_lease_id"],
        "lease-old"
    );
    assert_eq!(
        data["state_loss_recovery"]["affected_job_ids"][0],
        "7ab96605-38b7-4a6a-bbb8-99db839fa6dc"
    );
    assert_eq!(
        data["state_loss_recovery"]["replacement"]["lease_id"],
        "lease-new"
    );
}

#[test]
fn ensure_or_tunnel_failure_report_retains_completed_state_loss_recovery() {
    let envelope = parse_envelope(r#"{"success":true,"data":{"recovered_lease_id":"lease-old","recorded_dead_pid":42,"recorded_endpoint":"127.0.0.1:7421","affected_job_ids":[],"affected_job_count":0,"evidence_snapshot_path":"/snapshot","ownership_proof":[],"retry_guidance":"retry","replacement":{"pid":43,"address":"127.0.0.1:7422","state_path":"/state","lease_id":"lease-new"}}}"#).expect("envelope");
    let recovery = decode_state_loss_recovery(envelope.data).expect("recovery");
    let (mut report, _) = failed_connect(
        "runner",
        std::path::PathBuf::from("/session"),
        RunnerFailureKind::TunnelFailure,
        "tunnel failed".to_string(),
    );
    attach_state_loss_recovery(&mut report, Some(recovery));
    assert_eq!(
        report
            .state_loss_recovery
            .as_ref()
            .map(|value| value.replacement.lease_id.as_str()),
        Some("lease-new")
    );
}

#[test]
fn remote_leaseless_recovery_timeout_is_actionable() {
    let message = leaseless_recovery_failure_message(&homeboy_core::server::CommandOutput {
        stdout: String::new(),
        stderr: String::new(),
        success: false,
        exit_code: 124,
        timed_out: true,
        child_resource: None,
    });
    assert!(message.contains("daemon status"));
}

#[cfg(unix)]
#[test]
fn runner_connect_persists_recovery_evidence_after_daemon_failure() {
    test_support::with_isolated_home(|home| {
        let daemon = home.path().join("remote-homeboy");
        let argv_path = home.path().join("recovery-argv");
        std::fs::write(
            &daemon,
            r#"#!/bin/sh
case "$1 $2" in
  "self identity")
printf '%s\n' '{"success":true,"data":{"version":"0.284.0","display":"homeboy 0.284.0+test"}}'
;;
"daemon reconcile-leaseless-orphans")
if [ "$3" = "--help" ]; then
  printf '%s\n' 'OPTIONS:' '    --confirm-no-daemon-owner'
else
  printf '%s\n' "$@" > "$HOMEBOY_TEST_RECOVERY_ARGV"
  printf '%s\n' '{"success":true,"data":{"affected_job_ids":[],"affected_job_count":0,"affected_jobs":[],"historical_lease_ids":[],"evidence_snapshot_path":"/tmp/jobs.snapshot","ownership_proof":["owner lock acquired"],"retry_guidance":"retry","replacement":{"pid":42,"address":"127.0.0.1:7421","state_path":"/tmp/state.json","lease_id":"lease-new"}}}'
fi
;;
  "daemon status") exit 1 ;;
esac
"#,
        )
        .expect("write remote Homeboy shim");
        let mut permissions = std::fs::metadata(&daemon)
            .expect("read remote Homeboy shim metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&daemon, permissions)
            .expect("make remote Homeboy shim executable");

        server::create(
            &serde_json::json!({
                "id": "local-runner",
                "host": "localhost",
                "user": "test",
            })
            .to_string(),
            false,
        )
        .expect("create local server");
        crate::create(
            &serde_json::json!({
                "id": "local-runner",
                "kind": "ssh",
                "homeboy_path": daemon,
                "env": { "HOMEBOY_TEST_RECOVERY_ARGV": argv_path },
            })
            .to_string(),
            false,
        )
        .expect("enable local runner");

        let (report, exit_code) =
            connect_with_orphan_adoption("local-runner", None, &[], true, None, None, None)
                .expect("connect result");

        assert_eq!(
            exit_code, 20,
            "the shim intentionally rejects status after recovery"
        );
        assert!(!report.connected);
        assert_eq!(
            report.failure_kind,
            Some(RunnerFailureKind::DaemonStartupFailure)
        );
        assert_eq!(
            report
                .leaseless_recovery
                .as_ref()
                .expect("recovery report")
                .replacement
                .lease_id,
            "lease-new"
        );
        let evidence = report
            .leaseless_recovery_evidence
            .as_ref()
            .expect("recovery evidence");
        assert_eq!(
            evidence.contract,
            RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner
        );
        assert_eq!(evidence.remote_command_identity, "homeboy 0.284.0+test");
        assert_eq!(
            evidence
                .recovery
                .as_ref()
                .expect("recovery evidence result")
                .replacement
                .lease_id,
            "lease-new"
        );
        let session = read_session("local-runner")
            .expect("read recovery session")
            .expect("recovery session");
        assert!(session.local_url.is_none());
        assert!(session.local_port.is_none());
        let recovery_evidence: crate::RunnerLeaselessRecoveryEvidence = serde_json::from_value(
            session
                .leaseless_recovery_evidence
                .clone()
                .expect("persisted recovery evidence"),
        )
        .expect("typed recovery evidence");
        assert_eq!(
            recovery_evidence
                .recovery
                .as_ref()
                .expect("persisted recovery result")
                .replacement
                .lease_id,
            "lease-new"
        );
        assert_eq!(
            std::fs::read_to_string(argv_path).expect("read dispatched recovery argv"),
            "daemon\nreconcile-leaseless-orphans\n--confirm-no-daemon-owner\n--addr\n127.0.0.1:0\n"
        );
    });
}

#[test]
fn state_loss_recovery_delegation_uses_the_canonical_exact_contract() {
    let command =
        remote_state_loss_recovery_command("/opt/homeboy", "lease exact", 4242, "127.0.0.1:4242");
    assert_eq!(
        command,
        "/opt/homeboy daemon recover-missing-lease-state --lease-id 'lease exact' --recorded-pid 4242 --recorded-endpoint 127.0.0.1:4242 --confirm-pid-dead --confirm-control-plane-lost --addr 127.0.0.1:0"
    );
}

#[test]
fn leaseless_recovery_uses_confirm_no_daemon_owner_contract() {
    let contract = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "OPTIONS:\n    --confirm-no-daemon-owner\n",
        false,
    ))
    .expect("one-flag contract");

    assert_eq!(
        contract,
        RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner
    );
    let command = remote_leaseless_recovery_command("/opt/homeboy", "127.0.0.1:0", contract);
    assert!(command.contains("--confirm-no-daemon-owner"));
    assert!(!command.contains("--reconcile-leaseless-orphans"));
    assert!(!command.contains("--confirm-control-plane-lost"));
}

#[test]
fn leaseless_recovery_rejects_legacy_two_flag_contract() {
    let error = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n",
        false,
    ))
    .expect_err("legacy two-flag contract is unsupported");
    assert!(error.contains("canonical"));
}

#[test]
fn leaseless_recovery_rejects_control_plane_lost_contract() {
    let error = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "OPTIONS:\n    --confirm-control-plane-lost\n",
        false,
    ))
    .expect_err("legacy control-plane-lost contract is unsupported");
    assert!(error.contains("canonical"));
}

#[test]
fn leaseless_recovery_refuses_unsupported_or_ambiguous_help() {
    let unsupported = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "OPTIONS:\n    --addr <ADDR>\n",
        false,
    ))
    .expect_err("unsupported contract");
    assert!(unsupported.contains("did not advertise"));

    let ambiguous = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n    --confirm-control-plane-lost\n",
        false,
    ))
    .expect_err("mixed legacy flags are unsupported");
    assert!(ambiguous.contains("canonical"));
}

#[test]
fn leaseless_recovery_parses_only_exact_option_declarations() {
    let options = declared_long_options(
        "OPTIONS:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n    --addr <ADDR>\n",
    );
    assert!(options.contains("--reconcile-leaseless-orphans"));
    assert!(options.contains("--confirm-no-daemon-owner"));
    assert!(options.contains("--addr"));

    let prose = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "Examples:\n    --reconcile-leaseless-orphans\n    --confirm-no-daemon-owner\n",
        false,
    ))
    .expect_err("example lines must not advertise a contract");
    assert!(prose.contains("did not advertise"));

    let prose = negotiate_leaseless_recovery_contract(&command_output(
        true,
        "Options:\n    --reconcile-leaseless-orphans after inspection\n    --confirm-no-daemon-owner after inspection\n",
        false,
    ))
    .expect_err("prose in the options section must not advertise a contract");
    assert!(prose.contains("did not advertise"));
}

#[test]
fn leaseless_recovery_evidence_records_selected_contract_and_command_identity() {
    for (help, expected_contract) in [(
        "Options:\n    --confirm-no-daemon-owner\n",
        RunnerLeaselessRecoveryContract::ConfirmNoDaemonOwner,
    )] {
        let contract = negotiate_leaseless_recovery_contract(&command_output(true, help, false))
            .expect("advertised contract");
        let evidence = leaseless_recovery_evidence(
            contract,
            "homeboy 0.284.1+abc123",
            sample_leaseless_recovery(),
        );

        assert_eq!(evidence.contract, expected_contract);
        assert_eq!(evidence.remote_command_identity, "homeboy 0.284.1+abc123");
        assert_eq!(
            evidence
                .recovery
                .as_ref()
                .expect("recovery result")
                .replacement
                .lease_id,
            "lease-new"
        );
    }
}

// NOTE: the test asserting generated recovery commands parse against the real
// CLI (`cli_surface::Cli`) was removed as part of extracting homeboy-core:
// parser correctness is covered by the CLI layer's own tests, and keeping it
// here forced core to depend upward on the CLI parser. The command builders
// themselves remain covered by the surrounding recovery tests.

#[test]
fn persisted_session_without_leaseless_recovery_evidence_deserializes() {
    let session: RunnerSession = serde_json::from_value(serde_json::json!({
        "runner_id": "homeboy-lab",
        "server_id": null,
        "tunnel_pid": null,
        "remote_daemon_pid": null,
        "homeboy_version": "test",
        "connected_at": "2026-07-14T00:00:00Z"
    }))
    .expect("legacy session");

    assert!(session.leaseless_recovery_evidence.is_none());
}

#[test]
fn persisted_recovery_evidence_without_result_deserializes() {
    let evidence: RunnerLeaselessRecoveryEvidence = serde_json::from_value(serde_json::json!({
        "contract": "reconcile_leaseless_orphans_and_confirm_no_daemon_owner",
        "remote_command_identity": "homeboy 0.284.1+abc123"
    }))
    .expect("prior recovery evidence");

    assert!(evidence.recovery.is_none());
}

#[test]
fn failed_connect_without_recovery_omits_recovery_evidence() {
    let (report, _) = failed_connect(
        "runner",
        PathBuf::from("/session.json"),
        RunnerFailureKind::DaemonStartupFailure,
        "daemon failed".to_string(),
    );

    let serialized = serde_json::to_value(report).expect("serialize failed connect");
    assert!(serialized.get("leaseless_recovery_evidence").is_none());
}

#[test]
fn leaseless_recovery_refuses_failed_or_timed_out_probe() {
    let failed =
        negotiate_leaseless_recovery_contract(&command_output(false, String::new(), false))
            .expect_err("failed probe");
    assert!(failed.contains("capability probe failed"));

    let timed_out =
        negotiate_leaseless_recovery_contract(&command_output(false, String::new(), true))
            .expect_err("timed out probe");
    assert!(timed_out.contains("timed out"));
}

#[test]
fn leaseless_recovery_does_not_mutate_before_successful_negotiation() {
    let events = std::cell::RefCell::new(Vec::new());
    let result = execute_remote_leaseless_recovery(
        || {
            events.borrow_mut().push("probe");
            command_output(true, "OPTIONS:\n    --addr <ADDR>\n", false)
        },
        |_| {
            events.borrow_mut().push("recover");
            command_output(true, String::new(), false)
        },
    );

    assert!(result.is_err());
    assert_eq!(*events.borrow(), vec!["probe"]);
}

#[test]
fn lost_local_session_refuses_unreachable_daemon_with_active_jobs() {
    let status = remote_daemon_status_for_test_with_reason(
        false,
        false,
        1,
        "lease-dead",
        4545,
        Some(DaemonStaleReasonCode::PidDead),
    );

    let err = remote_daemon_connect_action(None, &status).expect_err("unreachable daemon");

    assert!(err.contains("unreachable"));
}

#[test]
fn orphan_adoption_command_carries_exact_lease_and_dead_pid_confirmation() {
    let job_id =
        uuid::Uuid::parse_str("fbac0390-dbb1-464b-8716-0894ccc05f2f").expect("valid job ID");
    let command = remote_daemon_adopt_orphan_command("/opt/homeboy", "lease dead", &[job_id]);

    assert!(command.contains("daemon adopt-orphan"));
    assert!(command.contains("--lease-id 'lease dead'"));
    assert!(command.contains("--confirm-pid-dead"));
    assert!(command.contains("--confirm-untracked-child-dead fbac0390-dbb1-464b-8716-0894ccc05f2f"));
}

#[test]
fn refuses_to_replace_proven_dead_daemon_with_active_jobs_when_lease_mismatches() {
    let session = direct_ssh_session("lease-recorded");
    let status = remote_daemon_status_for_test_with_reason(
        false,
        false,
        1,
        "lease-dead",
        4545,
        Some(DaemonStaleReasonCode::PidDead),
    );

    let err = remote_daemon_connect_action(Some(&session), &status)
        .expect_err("refuse mismatched dead lease");

    assert!(err.contains("1 active job(s)"));
    assert!(err.contains("unreachable"));
    assert!(err.contains("1 active job(s) were not replaced"));
    assert!(err.contains("active-job recovery guidance"));
}

#[test]
fn dead_recorded_daemon_without_active_jobs_routes_to_idempotent_ensure_start() {
    let status = remote_daemon_status_for_test_with_reason(
        false,
        false,
        0,
        "lease-dead",
        4545,
        Some(DaemonStaleReasonCode::PidDead),
    );

    assert_eq!(
        remote_daemon_connect_action(Some(&direct_ssh_session("lease-dead")), &status)
            .expect("ensure start"),
        RemoteDaemonConnectAction::Start
    );
}

#[cfg(unix)]
fn with_idle_stale_replacement_shim(
    post_lease_id: &str,
    post_identity: &str,
    run: impl FnOnce(&SshClient, &str, &str, PathBuf),
) {
    test_support::with_isolated_home(|home| {
        let daemon = home.path().join("remote-homeboy");
        let marker = home.path().join("replacement-complete");
        let argv_path = home.path().join("replacement-argv");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let marker_for_server = marker.clone();
        let post_lease_id = post_lease_id.to_string();
        let post_identity = post_identity.to_string();
        let post_lease_id_for_server = post_lease_id.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().expect("request");
                let mut request = [0; 4096];
                let length = stream.read(&mut request).expect("read request");
                let request = String::from_utf8(request[..length].to_vec()).expect("request text");
                let replacement_complete = marker_for_server.exists();
                let identity = if replacement_complete {
                    post_identity.as_str()
                } else {
                    "homeboy 0.288.13+stale"
                };
                let body = if request.starts_with("GET /version ") {
                    serde_json::json!({
                        "version": "0.289.0",
                        "build_identity": { "display": identity },
                    })
                } else {
                    assert!(request.starts_with("GET /jobs "), "{request}");
                    serde_json::json!({
                        "success": true,
                        "data": { "body": {
                            "active_runner_jobs": [],
                            "stale_runner_jobs": [],
                        }},
                    })
                }
                .to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .expect("response");
            }
        });
        std::fs::write(
            &daemon,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$@" >> "$HOMEBOY_TEST_REPLACEMENT_ARGV"
case "$1 $2" in
  "daemon status")
    if [ -f "{marker}" ]; then
      printf '%s\n' '{{"success":true,"data":{{"running":true,"fresh":true,"reachable":true,"freshness":{{"active_jobs":0}},"state":{{"address":"{address}","pid":222,"lease_id":"{post_lease_id}"}}}}}}'
    else
      printf '%s\n' '{{"success":true,"data":{{"running":true,"fresh":false,"reachable":true,"freshness":{{"active_jobs":0,"stale_reason_code":"version_mismatch"}},"state":{{"address":"{address}","pid":111,"lease_id":"lease-old"}}}}}}'
    fi
    ;;
  "daemon stop")
    touch "{marker}"
    printf '%s\n' '{{"success":true,"data":{{"action":"stop"}}}}'
    ;;
  "daemon ensure-running")
    printf '%s\n' '{{"success":true,"data":{{"address":"{address}","pid":222,"lease_id":"lease-new"}}}}'
    ;;
esac
"#,
                marker = marker.display(),
                address = address,
                post_lease_id = post_lease_id_for_server,
            ),
        )
        .expect("write remote Homeboy shim");
        let mut permissions = std::fs::metadata(&daemon)
            .expect("read remote Homeboy shim metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&daemon, permissions)
            .expect("make remote Homeboy shim executable");
        server::create(
            &serde_json::json!({ "id": "local-runner", "host": "localhost", "user": "test" })
                .to_string(),
            false,
        )
        .expect("create local server");
        let server_config = server::load("local-runner").expect("load local server");
        let mut client =
            SshClient::from_server(&server_config, "local-runner").expect("SSH client");
        client.env.insert(
            "HOMEBOY_TEST_REPLACEMENT_ARGV".to_string(),
            argv_path.display().to_string(),
        );
        run(
            &client,
            daemon.to_str().expect("daemon path"),
            "homeboy 0.289.0+configured",
            argv_path,
        );
        server.join().expect("server");
    });
}

#[cfg(unix)]
#[test]
fn idle_stale_replacement_uses_actual_endpoint_envelopes_and_reprobes_the_new_owner() {
    with_idle_stale_replacement_shim(
        "lease-new",
        "homeboy 0.289.0+configured",
        |client, homeboy, configured_identity, argv_path| {
            let daemon = ensure_remote_daemon(
                client,
                homeboy,
                "homeboy-lab",
                None,
                configured_identity,
                None,
                &[],
                None,
            )
            .expect("replacement succeeds");
            assert_eq!(daemon.lease_id.as_deref(), Some("lease-new"));
            assert_eq!(daemon.pid, Some(222));
            assert_eq!(daemon.build_identity.as_deref(), Some(configured_identity));
            assert_eq!(
                std::fs::read_to_string(argv_path).expect("read command argv"),
                "daemon\nstatus\ndaemon\nstop\n--force\n--lease-id\nlease-old\ndaemon\nensure-running\n--addr\n127.0.0.1:0\ndaemon\nstatus\n"
            );
        },
    );
}

#[cfg(unix)]
#[test]
fn idle_stale_replacement_refuses_a_post_stop_owner_or_identity_change() {
    with_idle_stale_replacement_shim(
        "lease-raced",
        "homeboy 0.288.13+stale",
        |client, homeboy, configured_identity, _| {
            let error = ensure_remote_daemon(
                client,
                homeboy,
                "homeboy-lab",
                None,
                configured_identity,
                None,
                &[],
                None,
            )
            .expect_err("concurrent stale daemon is refused");
            assert!(error.contains("ownership changed"));
        },
    );
}

#[cfg(unix)]
#[test]
fn idle_stale_replacement_refuses_a_post_stop_identity_change() {
    with_idle_stale_replacement_shim(
        "lease-new",
        "homeboy 0.288.13+stale",
        |client, homeboy, configured_identity, _| {
            let error = ensure_remote_daemon(
                client,
                homeboy,
                "homeboy-lab",
                None,
                configured_identity,
                None,
                &[],
                None,
            )
            .expect_err("stale replacement identity is refused");
            assert!(error.contains("does not match configured runner binary"));
        },
    );
}

#[cfg(unix)]
#[test]
fn stale_replacement_force_stop_rejects_a_success_envelope_without_stop_action() {
    test_support::with_isolated_home(|home| {
        let daemon = home.path().join("remote-homeboy");
        std::fs::write(
            &daemon,
            "#!/bin/sh\nprintf '%s\\n' '{\"success\":true,\"data\":{\"action\":\"status\"}}'\n",
        )
        .expect("write remote Homeboy shim");
        let mut permissions = std::fs::metadata(&daemon).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&daemon, permissions).expect("make shim executable");
        server::create(
            &serde_json::json!({ "id": "local-runner", "host": "localhost", "user": "test" })
                .to_string(),
            false,
        )
        .expect("create local server");
        let server_config = server::load("local-runner").expect("load local server");
        let client = SshClient::from_server(&server_config, "local-runner").expect("SSH client");
        let error = remote_daemon::remote_daemon_force_stop(
            &client,
            daemon.to_str().expect("daemon path"),
            "lease-old",
        )
        .expect_err("malformed success response");
        assert!(error.contains("unexpected response"));
    });
}
