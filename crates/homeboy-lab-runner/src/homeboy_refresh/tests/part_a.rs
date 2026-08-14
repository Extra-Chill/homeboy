#![cfg(test)]

use super::*;
use crate::{
    RunnerActiveJobState, RunnerAdmissionSnapshot, RunnerExecMode, RunnerSessionState,
    RunnerStaleDaemonWarning, RunnerStatusReport,
};
use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use homeboy_core::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};
use homeboy_core::test_support;

/// Environment variables that redirect Cargo's output directory. A nested Cargo
/// fixture that asserts a fixture-local `target/` path must not inherit these
/// from the controller, `homeboy review`, or CI — otherwise the nested build
/// succeeds while writing somewhere else entirely, and the assertion fails on a
/// path that was never going to exist (#9846).
const CARGO_TARGET_DIR_OVERRIDES: [&str; 3] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "HOMEBOY_CARGO_TARGET_DIR",
];

/// A shell for nested Cargo fixtures, hermetic with respect to shared
/// target-directory configuration.
fn fixture_build_shell(script: &str) -> Command {
    let mut command = Command::new("bash");
    command.args(["-c", script]);
    for key in CARGO_TARGET_DIR_OVERRIDES {
        command.env_remove(key);
    }
    command
}

/// Deterministic coverage for the hermeticity contract itself: the fixture shell
/// must clear every shared target-directory override regardless of what the
/// surrounding review/CI environment set. This asserts the command's configured
/// environment rather than mutating process-global state (#9846).
#[test]
fn fixture_build_shell_clears_shared_cargo_target_overrides() {
    let command = fixture_build_shell("true");
    let removed: Vec<&str> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .filter_map(|(key, _)| key.to_str())
        .collect();

    for key in CARGO_TARGET_DIR_OVERRIDES {
        assert!(
            removed.contains(&key),
            "fixture shell must clear {key}; cleared: {removed:?}"
        );
    }
}

fn fixture_commits_are_ancestral(repository: &Path, older: &str, newer: &str) -> Result<bool> {
    let status = Command::new("git")
        .args(["merge-base", "--is-ancestor", older, newer])
        .current_dir(repository)
        .status()
        .expect("compare fixture commits");
    Ok(status.success())
}

fn ancestry_exec_output(exit_code: i32) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: "lab".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: vec!["git".to_string()],
        remote_cwd: "/runner/homeboy".to_string(),
        exit_code,
        stdout: String::new(),
        stderr: String::new(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: Some("ancestry-job".to_string()),
        job_events: None,
        mirror_run_id: Some("ancestry-run".to_string()),
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}

fn linear_commit_fixture() -> (tempfile::TempDir, String, String) {
    let fixture = tempfile::tempdir().expect("git fixture");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "homeboy@example.test"],
        vec!["config", "user.name", "Homeboy Test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("git")
            .success());
    }
    let revision = || {
        String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(fixture.path())
                .output()
                .expect("revision")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_string()
    };
    std::fs::write(fixture.path().join("release"), "old\n").expect("old release");
    for args in [vec!["add", "."], vec!["commit", "-m", "old"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("commit old")
            .success());
    }
    let old = revision();
    std::fs::write(fixture.path().join("release"), "new\n").expect("new release");
    assert!(Command::new("git")
        .args(["commit", "-am", "new"])
        .current_dir(fixture.path())
        .status()
        .expect("commit new")
        .success());
    let new = revision();
    (fixture, old, new)
}

#[test]
fn routine_reconnect_refuses_to_interrupt_an_admitted_lab_offload() {
    let admission = active_admission("0b77251a-b6a7-42a6-91a3-e49ff5f57c16");
    let error = protect_active_jobs_before_reconnect("homeboy-lab", &[admission], false)
        .expect_err("routine reconnect must preserve the active cook");

    assert_eq!(
        error.details["active_job_ids"],
        serde_json::json!(["0b77251a-b6a7-42a6-91a3-e49ff5f57c16"])
    );
    assert!(error.message.contains("homeboy-lab"));
    assert!(error.message.contains("--force"));
    assert!(error.details["tried"][0]
        .as_str()
        .is_some_and(|command| command.contains("runner job logs homeboy-lab")));
}

#[test]
fn forced_reconnect_reports_the_jobs_it_will_interrupt() {
    let admission = active_admission("0b77251a-b6a7-42a6-91a3-e49ff5f57c16");
    let interrupted = protect_active_jobs_before_reconnect("homeboy-lab", &[admission], true)
        .expect("explicit force permits interruption");

    assert_eq!(
        interrupted,
        vec!["0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string()]
    );
}

#[test]
fn deferred_reconnect_reports_selected_binary_and_exact_active_job_followups() {
    let job_id = "0b77251a-b6a7-42a6-91a3-e49ff5f57c16".to_string();
    let followups = active_job_followups("homeboy-lab", std::slice::from_ref(&job_id));

    let deferred = HomeboyReconnectDeferred {
        reason: "active_daemon_jobs",
        active_job_ids: vec![job_id.clone()],
        selected_binary_path: "/runner/homeboy".to_string(),
        followup_commands: followups.clone(),
        ownership_contention: None,
    };

    assert_eq!(deferred.active_job_ids, vec![job_id]);
    assert_eq!(deferred.selected_binary_path, "/runner/homeboy");
    assert_eq!(
        followups,
        vec![
            "homeboy runner job logs homeboy-lab 0b77251a-b6a7-42a6-91a3-e49ff5f57c16 --follow",
            "homeboy runner refresh-homeboy homeboy-lab --reconnect",
        ]
    );
}

fn active_admission(job_id: &str) -> homeboy_core::api_jobs::ActiveRunnerJobSummary {
    homeboy_core::api_jobs::ActiveRunnerJobSummary {
        runner_id: "homeboy-lab".to_string(),
        job_id: job_id.to_string(),
        operation: "runner.admission".to_string(),
        source: "runner-daemon".to_string(),
        kind: "runner.admission".to_string(),
        status: homeboy_core::api_jobs::JobStatus::Queued,
        command: "homeboy generic-workload".to_string(),
        cwd: None,
        started_at_ms: 1,
        updated_at_ms: 1,
        elapsed_ms: 0,
        heartbeat_age_ms: 0,
        claim: Default::default(),
        claim_expires_in_ms: None,
        lifecycle: None,
        durable_run_id: None,
        stale_reason: None,
        lifecycle_state: Some("active".to_string()),
        retryable: Some(false),
        active_child_count: None,
        active_cell_count: None,
    }
}

#[test]
fn refresh_preserves_only_its_direct_controller_lease_for_orphan_recovery() {
    let session = RunnerSession {
        runner_id: "lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:7421".to_string()),
        local_port: Some(7421),
        local_url: Some("http://127.0.0.1:7421".to_string()),
        tunnel_pid: Some(1),
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(2),
        remote_daemon_lease_id: Some("lease-refresh".to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-01-01T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };

    assert_eq!(
        refresh_owned_lease(session),
        Some("lease-refresh".to_string())
    );
}

#[test]
fn disconnected_refresh_connects_without_rotating_a_missing_daemon() {
    let operations = std::cell::RefCell::new(Vec::new());

    disconnect_before_reconnect(None, |_| {
        panic!("a disconnected runner has no daemon to rotate")
    })
    .expect("disconnected refresh does not attempt daemon rotation");
    operations.borrow_mut().push("connect promoted binary");

    assert_eq!(operations.into_inner(), ["connect promoted binary"]);
}

/// #12418: reconnect behavior is selected from the authoritative post-lease
/// transport state. A stale but connected daemon is still rotated; a retained
/// record for a disconnected runner must not block connecting the promotion.
#[test]
fn reconnect_transport_state_matrix_preserves_disconnected_and_rotation_paths() {
    let disconnected = refreshed_daemon_status(false, None);
    let mut stale_connected = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    stale_connected.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+newcommit".to_string()),
    ));
    let healthy_connected = refreshed_daemon_status(true, Some("homeboy 0.294.0+newcommit"));

    assert!(
        !reconnect_rotates_existing_transport(&disconnected),
        "a disconnected runner starts the promoted binary directly"
    );
    assert_eq!(
        reconnect_transport_exit_code(disconnected.is_connected()),
        1,
        "a disconnected runner cannot report reconnect transport success"
    );
    assert!(
        reconnect_rotates_existing_transport(&stale_connected),
        "a stale connected daemon is rotated before reconnect"
    );
    assert_eq!(
        reconnect_transport_exit_code(stale_connected.is_connected()),
        0,
        "a stale but connected transport remains a completed reconnect phase"
    );
    assert!(
        reconnect_rotates_existing_transport(&healthy_connected),
        "a healthy connected daemon retains the established rotation behavior"
    );
    assert_eq!(
        reconnect_transport_exit_code(healthy_connected.is_connected()),
        0,
        "a healthy connected transport remains a completed reconnect phase"
    );
}

#[test]
fn connected_refresh_rotates_before_connecting_the_promoted_binary() {
    let session = RunnerSession {
        runner_id: "lab".to_string(),
        mode: RunnerTunnelMode::DirectSsh,
        role: RunnerSessionRole::Controller,
        server_id: Some("lab".to_string()),
        controller_id: None,
        broker_url: None,
        remote_daemon_address: Some("127.0.0.1:7421".to_string()),
        local_port: Some(7421),
        local_url: Some("http://127.0.0.1:7421".to_string()),
        tunnel_pid: Some(1),
        tunnel_process_start_identity: None,
        proxy_forward: None,
        remote_daemon_pid: Some(2),
        remote_daemon_lease_id: Some("lease-refresh".to_string()),
        homeboy_version: "test".to_string(),
        homeboy_build_identity: None,
        connected_at: "2026-01-01T00:00:00Z".to_string(),
        worker_identity: None,
        worker_pid: None,
        last_seen_at: None,
        leaseless_recovery_evidence: None,
    };
    let operations = std::cell::RefCell::new(Vec::new());

    disconnect_before_reconnect(Some(&session), |_| {
        operations.borrow_mut().push("disconnect existing daemon");
        Ok(())
    })
    .expect("connected refresh rotates the existing daemon");
    operations.borrow_mut().push("connect promoted binary");

    assert_eq!(
        operations.into_inner(),
        ["disconnect existing daemon", "connect promoted binary"]
    );
}

/// #12418: materialization begins from a disconnected observation, but another
/// controller can connect the old daemon before this refresh obtains promotion
/// ownership. The post-lease session must drive retirement, and active work
/// remains owned by the draining generation rather than being interrupted.
#[test]
fn materialization_race_rotates_the_newly_connected_daemon_and_preserves_active_work() {
    let materialization_snapshot = refreshed_daemon_status(false, None);
    let mut post_lease_status = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    let old_session = post_lease_status
        .session
        .as_mut()
        .expect("post-materialization connection persists a session");
    old_session.remote_daemon_lease_id = Some("lease-old".to_string());
    old_session.remote_daemon_pid = Some(42);

    assert!(
        !reconnect_rotates_existing_transport(&materialization_snapshot),
        "materialization began while the runner was disconnected"
    );
    let post_lease_session = reconnect_session_after_promotion(true, &post_lease_status)
        .expect("the post-lease connected daemon is the one to retire");
    assert_eq!(
        post_lease_session.remote_daemon_lease_id.as_deref(),
        Some("lease-old")
    );

    let operations = RefCell::new(Vec::new());
    disconnect_before_reconnect(Some(&post_lease_session), |_| {
        operations.borrow_mut().push("retire lease-old");
        Ok(())
    })
    .expect("the daemon observed after materialization is retired before connect");
    operations.borrow_mut().push("connect promoted binary");
    assert_eq!(
        operations.into_inner(),
        ["retire lease-old", "connect promoted binary"]
    );

    let active_job = active_admission("active-job");
    assert!(
        should_rotate_daemon_generation(true, false, false),
        "active work selects generation rotation instead of daemon replacement"
    );
    let mut generations = crate::RollingGenerations::new("lease-old", "old daemon");
    generations.admit_job(&active_job.job_id);
    assert_eq!(
        generations.begin("lease-new", "promoted daemon"),
        crate::RollingStart::Start
    );
    assert!(generations.activate("lease-new"));
    assert_eq!(
        generations.job_owner(&active_job.job_id),
        Some("lease-old"),
        "the old generation retains active work while the promoted generation admits new work"
    );
}

fn refreshed_daemon_status(connected: bool, identity: Option<&str>) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: "lab".to_string(),
        connected,
        state: if connected {
            RunnerSessionState::Connected
        } else {
            RunnerSessionState::Disconnected
        },
        session: identity.map(|homeboy_build_identity| RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some(homeboy_build_identity.to_string()),
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }),
        stale_daemon: None,
        configured_job_binary_build_identity: identity.map(str::to_string),
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        stale_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: String::new(),
    }
}

#[test]
fn refreshed_daemon_verification_accepts_the_post_start_health_window() {
    let not_ready = refreshed_daemon_status(false, Some("homeboy 0.1.0+06bbf46013cf"));
    let ready = refreshed_daemon_status(true, Some("homeboy 0.1.0+06bbf46013cf"));
    let mut statuses = [not_ready, ready].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_topology_with(
        "lab",
        "06bbf46013cf",
        || Ok(statuses.next().expect("post-start status probe")),
        || retries += 1,
    )
    .expect("the persisted connected session identifies the requested daemon commit");
    assert_eq!(retries, 1, "the initial tunnel health race is retried once");
}

#[test]
fn refreshed_daemon_verification_converges_after_exact_identity_refresh() {
    let mut stale = refreshed_daemon_status(true, Some("homeboy 0.294.0+oldcommit"));
    stale.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "0.294.0".to_string(),
        "0.294.0".to_string(),
        Some("homeboy 0.294.0+oldcommit".to_string()),
        Some("homeboy 0.294.0+19a41cd5102d".to_string()),
    ));
    let converged = refreshed_daemon_status(true, Some("homeboy 0.294.0+19a41cd5102d"));
    let mut statuses = [stale, converged].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_topology_with(
        "lab",
        "19a41cd5102d",
        || Ok(statuses.next().expect("refresh convergence status")),
        || retries += 1,
    )
    .expect("refresh converges once daemon and configured executable identities match");
    assert_eq!(retries, 1);
}

#[test]
fn refreshed_daemon_verification_retries_until_the_configured_job_binary_converges() {
    let daemon_identity = "homeboy 0.294.0+19a41cd5102d";
    let mut stale_configured = refreshed_daemon_status(true, Some(daemon_identity));
    stale_configured.configured_job_binary_build_identity =
        Some("homeboy 0.294.0+oldcommit".to_string());
    let converged = refreshed_daemon_status(true, Some(daemon_identity));
    let mut statuses = [stale_configured, converged].into_iter();
    let mut retries = 0;

    verify_refreshed_daemon_topology_with(
        "lab",
        "19a41cd5102d",
        || Ok(statuses.next().expect("topology status probe")),
        || retries += 1,
    )
    .expect("refresh succeeds only after the configured binary and daemon converge");

    assert_eq!(
        retries, 1,
        "the stale configured binary is reread before success"
    );
}

#[test]
fn refreshed_daemon_verification_rejects_a_persisted_configured_binary_mismatch() {
    let mut status = refreshed_daemon_status(true, Some("homeboy 0.294.0+19a41cd5102d"));
    status.configured_job_binary_build_identity = Some("homeboy 0.294.0+oldcommit".to_string());

    let error = verify_refreshed_daemon_topology_status("lab", "19a41cd5102d", &status)
        .expect_err("matching daemon alone must not certify refresh success");

    assert!(error.message.contains("retained configured job binary"));
}

#[test]
fn refreshed_daemon_verification_rejects_commit_substring_mismatch() {
    let status = refreshed_daemon_status(true, Some("homeboy 0.1.0+x06bbf46013cf"));

    let error = verify_refreshed_daemon_topology_status("lab", "06bbf46013cf", &status)
        .expect_err("the daemon commit component must match exactly");
    assert!(error.message.contains("expected commit `06bbf46013cf`"));
}

#[test]
fn refreshed_daemon_rollback_stops_restores_and_reconnects_the_previous_binary() {
    let operations = std::cell::RefCell::new(Vec::new());

    rollback_refreshed_daemon_with(
        Some("/stable/homeboy"),
        || {
            operations.borrow_mut().push("stop new daemon".to_string());
            Ok(())
        },
        |path| {
            operations
                .borrow_mut()
                .push(format!("restore {}", path.expect("previous binary")));
            Ok(())
        },
        |path| {
            operations
                .borrow_mut()
                .push(format!("reconnect {}", path.expect("previous binary")));
            Ok(())
        },
    )
    .expect("rollback converges on the previous binary");

    assert_eq!(
        operations.into_inner(),
        [
            "stop new daemon",
            "restore /stable/homeboy",
            "reconnect /stable/homeboy",
        ]
    );
}

#[test]
fn materialize_plan_uses_clean_runner_cache() {
    let options = HomeboyBinaryRefreshOptions {
        runner_id: "lab".to_string(),
        mode: HomeboyBinaryRefreshMode::Materialize,
        source: Some("https://example.test/homeboy.git".to_string()),
        git_ref: Some("fix/foo".to_string()),
        target_dir: Some("/runner/ws/homeboy-clean".to_string()),
        reconnect: false,
        force: false,
        allow_downgrade: false,
        dry_run: true,
    };
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: options.source.clone(),
        git_ref: options.git_ref.clone(),
        target_dir: options.target_dir.clone(),
        binary_path: "/runner/ws/homeboy-clean/target/release/homeboy".to_string(),
        script: materialize_script(
            "https://example.test/homeboy.git",
            "fix/foo",
            "/runner/ws/homeboy-clean",
            "/runner/ws/homeboy-clean/target/release/homeboy",
            false,
        ),
        reconnect: false,
        followup_commands: refresh_followups("lab", false),
    };

    assert!(plan.script.contains("git clone \"$source\" \"$dir\""));
    assert!(plan
        .script
        .contains("rev-parse --verify --quiet \"${requested}^{commit}\""));
    assert!(plan.script.contains("checkout --quiet --force --detach"));
    assert!(plan.script.contains("cargo build --release --bin homeboy"));
    assert_eq!(
        plan.binary_path,
        "/runner/ws/homeboy-clean/target/release/homeboy"
    );
}

#[test]
fn materialize_plan_rejects_implicit_git_ancestry_downgrades() {
    let script = materialize_script(
        "https://example.test/homeboy.git",
        "v0.295.0",
        "/runner/ws/homeboy-clean",
        "/runner/ws/homeboy-clean/target/release/homeboy",
        false,
    );
    assert_eq!(
        script.matches("self identity").count(),
        1,
        "materialization probes identity once"
    );
    for legacy_sidecar in ["source_url", "checkout", "ref", "commit", "binary_sha256"] {
        assert!(
            !script.contains(&format!("$slot_dir/{legacy_sidecar}")),
            "materialization does not write legacy {legacy_sidecar} sidecars"
        );
    }

    assert!(script.contains("merge-base --is-ancestor \"$target\" \"$current\""));
    assert!(script.contains("HOMEBOY_REFRESH_DOWNGRADE_PREVIOUS=$current"));
    assert!(script.contains("--allow-downgrade only for an intentional rollback"));
    assert!(script.contains("checkout --quiet --force --detach \"$target\""));
}

#[test]
fn materialize_plan_allows_an_explicit_git_ancestry_downgrade() {
    let script = materialize_script(
        "https://example.test/homeboy.git",
        "v0.295.0",
        "/runner/ws/homeboy-clean",
        "/runner/ws/homeboy-clean/target/release/homeboy",
        true,
    );

    assert!(script.contains("allow_downgrade=true"));
}

#[test]
fn promotion_policy_blocks_tag_downgrade_without_mutating_selection_and_records_explicit_rollback()
{
    let fixture = tempfile::tempdir().expect("git fixture");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "homeboy@example.test"],
        vec!["config", "user.name", "Homeboy Test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("git")
            .success());
    }
    std::fs::write(fixture.path().join("release"), "same semver release\n").expect("release");
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(fixture.path())
        .status()
        .expect("add")
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "v1.0.0"])
        .current_dir(fixture.path())
        .status()
        .expect("commit")
        .success());
    assert!(Command::new("git")
        .args(["tag", "v1.0.0"])
        .current_dir(fixture.path())
        .status()
        .expect("tag")
        .success());
    let release = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.path())
            .output()
            .expect("release sha")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    std::fs::write(fixture.path().join("release"), "post tag, same semver\n").expect("post tag");
    assert!(Command::new("git")
        .args(["commit", "-am", "post tag"])
        .current_dir(fixture.path())
        .status()
        .expect("commit")
        .success());
    let newer = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.path())
            .output()
            .expect("new sha")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    let mut status = refreshed_daemon_status(true, Some(&format!("homeboy 1.0.0+{newer}")));
    status
        .session
        .as_mut()
        .expect("session")
        .homeboy_build_identity = Some(format!("homeboy 1.0.0+{newer}"));
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: Some("v1.0.0".to_string()),
        target_dir: Some(fixture.path().display().to_string()),
        binary_path: "/selected/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let candidate = serde_json::json!({"data":{"git_commit":release}});
    let authorities = RefreshPromotionAuthorities {
        controller: None,
        active_daemon: status
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.as_deref())
            .and_then(build_identity_commit)
            .map(str::to_string),
        configured_selected: None,
    };
    let denied =
        validate_refresh_promotion(&plan, &candidate, false, &authorities, |older, newer| {
            fixture_commits_are_ancestral(fixture.path(), older, newer)
        })
        .expect_err("post-tag active daemon must block implicit rollback");
    assert_eq!(denied.details["field"], "allow_downgrade");
    let rollback =
        validate_refresh_promotion(&plan, &candidate, true, &authorities, |older, newer| {
            fixture_commits_are_ancestral(fixture.path(), older, newer)
        })
        .expect("explicit rollback is allowed")
        .expect("rollback evidence");
    assert_eq!(rollback.requested.as_deref(), Some("v1.0.0"));
    assert_eq!(rollback.resolved, release);
    assert_eq!(rollback.selected, release);
    assert!(rollback
        .previous
        .iter()
        .any(|identity| identity.contains(&newer)));
}

#[test]
fn old_materializes_first_new_selects_first_uses_fresh_promotion_authorities() {
    let fixture = tempfile::tempdir().expect("git fixture");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "homeboy@example.test"],
        vec!["config", "user.name", "Homeboy Test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("git")
            .success());
    }
    std::fs::write(fixture.path().join("release"), "old\n").expect("old release");
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(fixture.path())
        .status()
        .expect("add")
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "old"])
        .current_dir(fixture.path())
        .status()
        .expect("commit")
        .success());
    let old = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.path())
            .output()
            .expect("old sha")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    std::fs::write(fixture.path().join("release"), "new\n").expect("new release");
    assert!(Command::new("git")
        .args(["commit", "-am", "new"])
        .current_dir(fixture.path())
        .status()
        .expect("commit")
        .success());
    let new = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.path())
            .output()
            .expect("new sha")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: None,
        git_ref: Some("old".to_string()),
        target_dir: Some(fixture.path().display().to_string()),
        binary_path: "/old/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let candidate = serde_json::json!({"data":{"git_commit":old}});

    // The old request finished materializing before the newer request selected.
    let stale_authorities = RefreshPromotionAuthorities {
        controller: None,
        active_daemon: None,
        configured_selected: None,
    };
    assert!(validate_refresh_promotion(
        &plan,
        &candidate,
        false,
        &stale_authorities,
        |older, newer| fixture_commits_are_ancestral(fixture.path(), older, newer),
    )
    .expect("stale snapshot would allow selection")
    .is_none());

    // Selection must use the authority reread after obtaining its lease.
    let fresh_authorities = RefreshPromotionAuthorities {
        controller: None,
        active_daemon: None,
        configured_selected: Some(new),
    };
    assert!(validate_refresh_promotion(
        &plan,
        &candidate,
        false,
        &fresh_authorities,
        |older, newer| fixture_commits_are_ancestral(fixture.path(), older, newer),
    )
    .is_err());
}

#[test]
fn rollback_evidence_excludes_unrelated_authorities() {
    let fixture = tempfile::tempdir().expect("git fixture");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "homeboy@example.test"],
        vec!["config", "user.name", "Homeboy Test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("git")
            .success());
    }
    std::fs::write(fixture.path().join("candidate"), "candidate\n").expect("candidate");
    for args in [vec!["add", "."], vec!["commit", "-m", "candidate"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("commit candidate")
            .success());
    }
    let revision = |name| {
        String::from_utf8(
            Command::new("git")
                .args(["rev-parse", name])
                .current_dir(fixture.path())
                .output()
                .expect("revision")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_string()
    };
    let candidate = revision("HEAD");
    assert!(Command::new("git")
        .args(["checkout", "--orphan", "unrelated"])
        .current_dir(fixture.path())
        .status()
        .expect("orphan")
        .success());
    assert!(Command::new("git")
        .args(["rm", "-rf", "."])
        .current_dir(fixture.path())
        .status()
        .expect("clear")
        .success());
    std::fs::write(fixture.path().join("authority"), "unrelated\n").expect("authority");
    for args in [vec!["add", "."], vec!["commit", "-m", "unrelated"]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(fixture.path())
            .status()
            .expect("commit unrelated")
            .success());
    }
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: Some("candidate".to_string()),
        target_dir: Some(fixture.path().display().to_string()),
        binary_path: "/selected/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let evidence = validate_refresh_promotion(
        &plan,
        &serde_json::json!({"data":{"git_commit":candidate}}),
        true,
        &RefreshPromotionAuthorities {
            controller: Some(revision("HEAD")),
            active_daemon: None,
            configured_selected: None,
        },
        |older, newer| fixture_commits_are_ancestral(fixture.path(), older, newer),
    )
    .expect("unrelated authority is not a rollback");
    assert!(evidence.is_none());
}

#[test]
fn materialize_script_records_the_peeled_commit_for_tags_and_direct_commits() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let source = fixture.path().join("source");
    let core = source.join("crates/homeboy-core");
    std::fs::create_dir_all(core.join("src")).expect("source directory");

    std::fs::write(
        source.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/homeboy-core\"]\n\n[package]\nname = \"homeboy\"\nversion = \"9.8.7\"\nedition = \"2021\"\n\n[[bin]]\nname = \"homeboy\"\npath = \"src/main.rs\"\n\n[dependencies]\nhomeboy-core = { path = \"crates/homeboy-core\" }\n",
    )
    .expect("write root manifest");
    std::fs::create_dir_all(source.join("src")).expect("root source directory");
    std::fs::write(
        source.join("src/main.rs"),
        "fn main() { println!(\"{{\\\"data\\\":{{\\\"git_commit\\\":\\\"{}\\\",\\\"git_dirty\\\":{}}}}}\", homeboy_core::git_commit(), homeboy_core::git_dirty()); }\n",
    )
    .expect("write root binary");
    std::fs::write(
        core.join("Cargo.toml"),
        "[package]\nname = \"homeboy-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .expect("write core manifest");
    // The build-identity script lives in the product-identity crate at the
    // workspace root. After lab-runner was extracted into `crates/homeboy-lab-runner`,
    // CARGO_MANIFEST_DIR points at this crate (which has no build.rs), so resolve
    // the real generator from the workspace root instead.
    let build_identity_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates/homeboy-product-identity");
    let build_identity_script = build_identity_dir.join("build.rs");
    std::fs::copy(&build_identity_script, core.join("build.rs"))
        .expect("copy core build identity script");
    std::fs::copy(
        build_identity_dir.join("src/git_watch_paths.rs"),
        core.join("src/git_watch_paths.rs"),
    )
    .expect("copy core build identity script dependency");
    // The generator emits HOMEBOY_PRODUCT_GIT_* (renamed from HOMEBOY_BUILD_GIT_*),
    // so the fixture's core crate consumes the current variable names.
    std::fs::write(
        core.join("src/lib.rs"),
        "pub fn git_commit() -> &'static str { env!(\"HOMEBOY_PRODUCT_GIT_COMMIT\") }\npub fn git_dirty() -> bool { env!(\"HOMEBOY_PRODUCT_GIT_DIRTY\") == \"true\" }\n",
    )
    .expect("write core build identity consumer");

    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Homeboy Test"],
        vec!["config", "user.email", "homeboy@example.test"],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("set up source fixture");
        assert!(status.success(), "source fixture setup succeeds");
    }
    std::fs::write(source.join("README.md"), "fixture\n").expect("write fixture");
    // Keep the materialized build tree clean: `cargo build` writes `target/` and
    // `Cargo.lock` into the clone, and the build-identity generator marks the
    // build dirty if `git status --porcelain` reports anything. Ignore those
    // build artifacts so the fixture's source-built binary reports a clean build.
    std::fs::write(source.join(".gitignore"), "/target\n/Cargo.lock\n").expect("write gitignore");
    for args in [vec!["add", "."], vec!["commit", "-m", "fixture"]] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("commit source fixture");
        assert!(status.success(), "source fixture commit succeeds");
    }
    let commit = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .expect("read fixture commit")
            .stdout,
    )
    .expect("fixture commit is UTF-8")
    .trim()
    .to_string();
    for args in [
        vec!["tag", "-a", "annotated", "-m", "annotated fixture"],
        vec!["tag", "lightweight"],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("tag source fixture");
        assert!(status.success(), "source fixture tag succeeds");
    }
    let annotated_object = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "annotated"])
            .current_dir(&source)
            .output()
            .expect("read annotated tag object")
            .stdout,
    )
    .expect("annotated tag object is UTF-8")
    .trim()
    .to_string();
    assert_ne!(annotated_object, commit, "fixture tag is annotated");
    std::fs::write(source.join("README.md"), "newer remote head\n").expect("update fixture");
    for args in [vec!["add", "."], vec!["commit", "-m", "newer head"]] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("advance source fixture");
        assert!(
            status.success(),
            "source fixture advances past requested refs"
        );
    }

    for (index, git_ref) in ["annotated", "lightweight", commit.as_str()]
        .iter()
        .enumerate()
    {
        let target_dir = fixture.path().join(format!("case-{index}/build"));
        let binary_path = target_dir.join("target/release/homeboy");
        let script = materialize_script(
            source.to_str().expect("source path"),
            git_ref,
            target_dir.to_str().expect("target path"),
            binary_path.to_str().expect("binary path"),
            false,
        );
        let output = fixture_build_shell(&script)
            .output()
            .expect("run materialize script");
        assert!(
            output.status.success(),
            "materialize {git_ref} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("script output is UTF-8");
        assert_eq!(
            source_sha_from_output(&stdout).as_deref(),
            Some(commit.as_str())
        );
        verify_materialized_identity(
            &ssh_bootstrap_plan(),
            &stdout,
            &parse_identity(&stdout).expect("source-built binary identity"),
        )
        .expect("peeled source identity matches the nested-workspace build commit");

        let immutable_binary = refreshed_binary_path(&ssh_bootstrap_plan(), &stdout)
            .expect("materialization reports immutable binary path");
        let repeated = fixture_build_shell(&script)
            .output()
            .expect("repeat materialize script");
        assert!(
            repeated.status.success(),
            "matching immutable slot is reusable: {}",
            String::from_utf8_lossy(&repeated.stderr)
        );
        assert_eq!(
            refreshed_binary_path(
                &ssh_bootstrap_plan(),
                &String::from_utf8(repeated.stdout).expect("repeat output is UTF-8"),
            )
            .expect("repeat reports immutable path"),
            immutable_binary
        );

        if index == 2 {
            std::fs::write(&immutable_binary, "corrupt immutable slot")
                .expect("corrupt immutable slot for validation coverage");
            let corrupt = fixture_build_shell(&script)
                .output()
                .expect("run materialize script against corrupt slot");
            assert!(
                !corrupt.status.success(),
                "corrupt slot is never overwritten"
            );
            assert!(String::from_utf8_lossy(&corrupt.stderr).contains("slot hash mismatch"));
        }
    }
}

#[test]
fn managed_slot_materialization_publishes_verified_select_authority() {
    let fixture = tempfile::tempdir().expect("fixture directory");
    let source = fixture.path().join("source");
    let build = fixture.path().join("build");
    let tools = fixture.path().join("tools");
    std::fs::create_dir_all(&source).expect("source directory");
    std::fs::create_dir_all(&tools).expect("tool directory");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "Homeboy Test"],
        vec!["config", "user.email", "homeboy@example.test"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&source)
            .status()
            .expect("initialize source")
            .success());
    }
    std::fs::write(source.join("README.md"), "fixture\n").expect("fixture source");
    assert!(Command::new("git")
        .args(["add", "."])
        .current_dir(&source)
        .status()
        .expect("stage source")
        .success());
    assert!(Command::new("git")
        .args(["commit", "-m", "fixture"])
        .current_dir(&source)
        .status()
        .expect("commit source")
        .success());
    let commit = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&source)
            .output()
            .expect("read commit")
            .stdout,
    )
    .expect("commit is UTF-8")
    .trim()
    .to_string();
    let cargo = tools.join("cargo");
    std::fs::write(
        &cargo,
        "#!/bin/sh\nset -e\nwhile [ \"$1\" != \"--manifest-path\" ]; do shift; done\ndir=$(dirname \"$2\")\nmkdir -p \"$dir/target/release\"\nprintf '%s\\n' '#!/bin/sh' 'if [ \"$1 $2\" = \"self identity\" ]; then printf \"%s\\n\" \"{\\\"data\\\":{\\\"git_commit\\\":\\\"'\"$(git -C \"$dir\" rev-parse HEAD)\"'\\\",\\\"git_dirty\\\":false}}\"; fi' > \"$dir/target/release/homeboy\"\nchmod 0755 \"$dir/target/release/homeboy\"\n",
    )
    .expect("fake cargo");
    assert!(Command::new("chmod")
        .args(["0755", cargo.to_str().expect("cargo path")])
        .status()
        .expect("make cargo executable")
        .success());
    let binary = build.join("target/release/homeboy");
    let script = materialize_script(
        source.to_str().expect("source path"),
        "HEAD",
        build.to_str().expect("build path"),
        binary.to_str().expect("binary path"),
        false,
    );
    let mut materialize = fixture_build_shell(&script);
    materialize.env(
        "PATH",
        format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
    );
    let output = materialize.output().expect("materialize managed slot");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("materialize output");
    let immutable = refreshed_binary_path(&ssh_bootstrap_plan(), &stdout).expect("immutable path");
    let slot = Path::new(&immutable).parent().expect("slot");
    assert!(slot.join("provenance").is_file());
    for legacy_sidecar in ["source_url", "checkout", "ref", "commit", "binary_sha256"] {
        assert!(
            !slot.join(legacy_sidecar).exists(),
            "materialization publishes no legacy {legacy_sidecar} sidecar"
        );
    }
    let select_output = fixture_build_shell(&identity_probe_script(&immutable))
        .output()
        .expect("probe managed selection");
    assert!(select_output.status.success());
    let select_stdout = String::from_utf8(select_output.stdout).expect("select output");
    let select_plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: immutable.clone(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let identity = parse_identity(&select_stdout).expect("selected identity");
    assert_eq!(identity_commit(&identity).as_deref(), Some(commit.as_str()));
    let checkout = managed_slot_checkout(&select_plan, &select_stdout, &identity)
        .expect("verified sidecar")
        .expect("managed authority");
    assert_eq!(checkout, build.display().to_string());

    assert!(Command::new("git")
        .args([
            "remote",
            "set-url",
            "origin",
            "https://attacker.example/homeboy.git"
        ])
        .current_dir(&build)
        .status()
        .expect("tamper checkout origin")
        .success());
    let tampered_checkout = fixture_build_shell(&identity_probe_script(&immutable))
        .output()
        .expect("probe tampered checkout");
    let tampered_checkout_stdout =
        String::from_utf8(tampered_checkout.stdout).expect("tampered checkout output");
    let tampered_checkout_identity =
        parse_identity(&tampered_checkout_stdout).expect("identity still reports");
    assert_eq!(
        managed_slot_checkout(
            &select_plan,
            &tampered_checkout_stdout,
            &tampered_checkout_identity
        )
        .expect("fail closed"),
        None
    );
    assert!(Command::new("git")
        .args([
            "remote",
            "set-url",
            "origin",
            source.to_str().expect("source path"),
        ])
        .current_dir(&build)
        .status()
        .expect("restore checkout origin")
        .success());
    std::fs::write(
        Path::new(&immutable)
            .parent()
            .expect("slot")
            .join("provenance"),
        "tampered\n",
    )
    .expect("tamper sidecar");
    let tampered = fixture_build_shell(&identity_probe_script(&immutable))
        .output()
        .expect("probe tampered slot");
    let tampered_stdout = String::from_utf8(tampered.stdout).expect("tampered output");
    let tampered_identity = parse_identity(&tampered_stdout).expect("identity still reports");
    assert_eq!(
        managed_slot_checkout(&select_plan, &tampered_stdout, &tampered_identity)
            .expect("fail closed"),
        None
    );
}

#[test]
fn materialize_failure_preserves_compiler_diagnostics_and_active_binary() {
    test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let source = fixture.path().join("source");
        let workspace = fixture.path().join("workspace");
        let bin = fixture.path().join("bin");
        std::fs::create_dir_all(source.join("src")).expect("source directory");
        std::fs::create_dir_all(&workspace).expect("workspace directory");
        std::fs::create_dir_all(&bin).expect("tool directory");
        let cargo = bin.join("cargo");
        std::fs::write(
            &cargo,
            "#!/bin/sh\necho compiler_diagnostic_marker >&2\nexit 101\n",
        )
        .expect("fake cargo");
        let status = Command::new("chmod")
            .args(["0755", cargo.to_str().expect("cargo path")])
            .status()
            .expect("make fake cargo executable");
        assert!(status.success(), "fake cargo is executable");
        std::fs::write(
            source.join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        std::fs::write(
            source.join("src/main.rs"),
            "fn main() { compiler_diagnostic_marker }\n",
        )
        .expect("invalid source");
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.email=homeboy@example.test",
                "-c",
                "user.name=Homeboy Test",
                "commit",
                "-m",
                "fixture",
            ],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .expect("run git");
            assert!(status.success(), "git fixture setup succeeds");
        }
        let source_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&source)
                .output()
                .expect("read source SHA")
                .stdout,
        )
        .expect("source SHA is UTF-8")
        .trim()
        .to_string();
        crate::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}","homeboy_path":"/active/homeboy","env":{{"PATH":"{}:{}"}}}}"#,
                workspace.display(),
                bin.display(),
                std::env::var("PATH").expect("PATH")
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            runner_id: "lab-local".to_string(),
            mode: HomeboyBinaryRefreshMode::Materialize,
            source: Some(source.display().to_string()),
            git_ref: Some("main".to_string()),
            target_dir: Some(workspace.join("build").display().to_string()),
            reconnect: false,
            force: false,
            allow_downgrade: false,
            dry_run: false,
        })
        .expect("refresh returns diagnostics for compiler failure");

        assert_eq!(
            exit_code,
            101,
            "stdout: {}\nstderr: {}",
            output
                .failure
                .as_ref()
                .map(|failure| failure.stdout.as_str())
                .unwrap_or_default(),
            output
                .failure
                .as_ref()
                .map(|failure| failure.stderr.as_str())
                .unwrap_or_default()
        );
        let failure = output.failure.expect("failure evidence is preserved");
        assert_eq!(
            output.phase_summary,
            vec![HomeboyRefreshPhase {
                name: "materialize",
                required: true,
                status: "failed",
                exit_code: 101,
                job_id: None,
                mirror_run_id: None,
            }]
        );
        assert_eq!(failure.exit_code, 101);
        assert_eq!(failure.source_sha.as_deref(), Some(source_sha.as_str()));
        assert!(failure
            .failed_command
            .starts_with(&["bash".to_string(), "-lc".to_string()]));
        assert!(failure
            .build_path
            .ends_with("/build/target/release/homeboy"));
        assert!(failure.stderr.contains("compiler_diagnostic_marker"));
        assert!(failure.capture.is_some());
        assert!(failure.execution_record.is_some());
        assert_eq!(failure.recovery_actions.len(), 1);
        assert_eq!(failure.recovery_actions[0].label, "refresh_retry");
        assert_eq!(
            crate::load("lab-local")
                .expect("reload runner")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/active/homeboy")
        );
    });
}

#[test]
fn select_plan_only_probes_requested_binary() {
    let script = identity_probe_script("/opt/homeboy/bin/homeboy");

    assert!(script.contains("binary='/opt/homeboy/bin/homeboy'"));
    assert!(script.contains("\"$binary\" self identity"));
    assert!(!script.contains("cargo build"));
}

#[test]
fn select_without_materialization_sha_promotes_the_verified_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/old"}"#,
            false,
        )
        .expect("runner");
        let mut plan = ssh_bootstrap_plan();
        plan.mode = "select".to_string();
        plan.binary_path = "/selected/homeboy".to_string();

        let promoted = ssh_bootstrap_promote_with(
            &plan,
            || Ok(r#"{"data":{"git_commit":"abc123","git_dirty":false}}"#.to_string()),
            |path, _| {
                let patch = refreshed_runner_patch("lab-local", path)?;
                match merge(Some("lab-local"), &patch.to_string(), &[])? {
                    MergeOutput::Single(result) => Ok((result.updated_fields, None)),
                    MergeOutput::Bulk(_) => Ok((Vec::new(), None)),
                }
            },
        )
        .expect("select has no materialization SHA requirement");

        assert_eq!(promoted.source_sha, None);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/selected/homeboy")
        );
    });
}

#[test]
fn reconnect_rollback_restores_only_its_own_selected_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/newer/homeboy"}"#,
            false,
        )
        .expect("runner");

        let restored = restore_runner_homeboy_path_if_selected(
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
        )
        .expect("compare and restore");

        assert!(!restored);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/newer/homeboy")
        );
    });
}

#[test]
fn reconnect_rollback_restores_its_own_selected_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/selected/homeboy"}"#,
            false,
        )
        .expect("runner");

        let restored = restore_runner_homeboy_path_if_selected(
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
        )
        .expect("compare and restore");

        assert!(restored);
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn post_promotion_active_job_race_restores_prior_selection_without_stopping_daemon() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/selected/homeboy"}"#,
            false,
        )
        .expect("runner");

        let deferred = defer_reconnect_after_promotion_race(
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
            &[active_admission("job-raced-after-precheck")],
        )
        .expect("defer raced reconnect");

        assert_eq!(deferred.reason, "active_daemon_jobs");
        assert_eq!(deferred.active_job_ids, ["job-raced-after-precheck"]);
        assert!(deferred.ownership_contention.is_none());
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn post_promotion_active_job_race_preserves_newer_selector_as_contention() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/newer/homeboy"}"#,
            false,
        )
        .expect("runner");

        let deferred = defer_reconnect_after_promotion_race(
            "lab-local",
            "/selected/homeboy",
            Some("/stable/homeboy"),
            &[active_admission("job-raced-after-precheck")],
        )
        .expect("defer raced reconnect");

        assert!(deferred
            .ownership_contention
            .as_deref()
            .is_some_and(|message| message.contains("preserving the newer owner")));
        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/newer/homeboy")
        );
    });
}

#[test]
fn reconnect_failure_after_stop_restores_and_reconnects_before_returning() {
    let operations = std::cell::RefCell::new(Vec::new());

    let error = rollback_refresh_connect_error_with::<(), _, _>(
        Error::internal_io("selected daemon reconnect failed".to_string(), None),
        || {
            operations
                .borrow_mut()
                .push("restore old binary".to_string());
            Ok(())
        },
        || {
            operations
                .borrow_mut()
                .push("persist old-or-new authoritative lease".to_string());
            Ok(())
        },
    )
    .expect_err("the original reconnect failure remains visible after convergence");

    assert_eq!(error.details["error"], "selected daemon reconnect failed");
    assert_eq!(
        operations.into_inner(),
        [
            "restore old binary",
            "persist old-or-new authoritative lease"
        ],
        "a failed reconnect after the old session is removed must compensate before returning"
    );
}

#[test]
fn nonzero_reconnect_report_rollback_restores_the_pre_refresh_binary() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab-local","kind":"local","homeboy_path":"/stable/homeboy"}"#,
            false,
        )
        .expect("runner");
        let patch =
            refreshed_runner_patch("lab-local", "/selected/homeboy").expect("selection patch");
        merge(Some("lab-local"), &patch.to_string(), &[]).expect("select binary");

        restore_runner_homeboy_path("lab-local", Some("/stable/homeboy"))
            .expect("rollback after nonzero reconnect report");

        assert_eq!(
            crate::load("lab-local")
                .expect("reload")
                .settings
                .homeboy_path
                .as_deref(),
            Some("/stable/homeboy")
        );
    });
}

#[test]
fn rollback_failure_keeps_the_primary_refresh_error() {
    let error = rollback_refresh_error_with::<(), _>(
        Error::validation_invalid_argument("disconnect", "primary stop failure", None, None),
        || {
            Err(Error::internal_io(
                "rollback write failure".to_string(),
                None,
            ))
        },
    )
    .expect_err("rollback failure is surfaced with the primary failure");

    assert!(error.message.contains("primary stop failure"));
    assert!(error.message.contains("rollback write failure"));
    assert_eq!(
        error.details["rollback_error"]["details"]["error"],
        "rollback write failure"
    );
}

#[test]
fn rollback_failure_persists_one_exact_previous_binary_continuation() {
    test_support::with_isolated_home(|_| {
        let error = durable_refresh_partial_error(
            Error::internal_unexpected("rollback reconnect failed"),
            "lab runner",
            Some("/stable builds/homeboy"),
            true,
        );
        let path = error.details["partial_state_path"]
            .as_str()
            .expect("partial state path");
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("durable partial state"))
                .expect("partial state JSON");

        assert_eq!(state["status"], "rollback_incomplete");
        assert_eq!(state["previous_binary_path"], "/stable builds/homeboy");
        assert_eq!(
            state["continuation_command"],
            "homeboy runner refresh-homeboy 'lab runner' --select '/stable builds/homeboy' --reconnect --force"
        );
        assert_eq!(
            error.details["continuation_command"],
            state["continuation_command"]
        );
    });
}

#[test]
fn successful_rollback_does_not_publish_partial_state() {
    test_support::with_isolated_home(|_| {
        let primary = rollback_refresh_error_with::<(), _>(
            Error::internal_unexpected("candidate connect failed"),
            || Ok(()),
        )
        .expect_err("primary failure remains visible");
        let error =
            durable_refresh_partial_error_if_needed(primary, "lab", Some("/stable/homeboy"), false);

        assert!(error.details.get("partial_state").is_none());
        assert!(!refresh_partial_state_path("lab")
            .expect("partial path")
            .exists());
    });
}

#[test]
fn default_target_dir_is_ref_scoped() {
    assert_eq!(
        default_target_dir("/runner/ws/", "origin/main"),
        "/runner/ws/_homeboy_binaries/homeboy-origin-main"
    );
    let target_dir = default_target_dir("/home/chubes/Developer", "main");
    let script = materialize_script(
        "https://example.test/homeboy.git",
        "main",
        &target_dir,
        &format!("{target_dir}/target/release/homeboy"),
        false,
    );
    assert!(script.contains("slot_dir=\"$(dirname \"$dir\")/homeboy-$binary_sha\""));
    assert!(!script.contains("_homeboy_binaries/$binary_sha"));
}

#[test]
fn parse_identity_reads_final_pretty_json_after_command_output() {
    let identity = parse_identity(
        "HEAD is now at abc123 fix runner\n{\n  \"success\": true,\n  \"data\": {\n    \"version\": \"0.263.0\"\n  }\n}\n",
    )
    .expect("identity parses");

    assert_eq!(identity["data"]["version"], "0.263.0");
}

#[test]
fn disconnected_ssh_refresh_dispatches_the_existing_script_with_bounded_transport() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: Some("https://example.test/homeboy.git".to_string()),
        git_ref: Some("accepted-sha".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/runner/homeboy/target/release/homeboy".to_string(),
        script: "managed clone fetch build select".to_string(),
        reconnect: true,
        followup_commands: Vec::new(),
    };

    let options = refresh_execution_options(
        &plan,
        vec!["bash".to_string(), "git".to_string(), "cargo".to_string()],
        true,
    );

    assert!(options.allow_diagnostic_ssh);
    assert_eq!(
        options.diagnostic_ssh_timeout,
        Some(DISCONNECTED_SSH_REFRESH_TIMEOUT)
    );
    assert_eq!(
        options.command,
        vec!["bash", "-lc", "managed clone fetch build select"]
    );
    assert_eq!(
        options
            .capability_preflight
            .expect("preflight")
            .required_commands,
        vec!["bash", "git", "cargo"]
    );
}

fn stale_daemon_admission_snapshot(
    active_job_count: usize,
    lease_id: Option<&str>,
    pid: Option<u32>,
) -> RunnerAdmissionSnapshot {
    let mut status = refreshed_daemon_status(true, Some("homeboy 0.1.0+stale"));
    status.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        "lab",
        "old".to_string(),
        "current".to_string(),
        None,
        Some("current".to_string()),
    ));
    status.daemon_freshness = Some(DaemonFreshnessReport {
        fresh: false,
        stale_reason_code: Some(DaemonStaleReasonCode::VersionMismatch),
        restartable: true,
        lease_id: lease_id.map(str::to_string),
        pid,
        recovery_evidence: None,
        ownership_evidence: None,
        adoption_command: None,
        binary_hash: None,
        daemon_version: None,
        daemon_build_identity: None,
        runtime_paths: None,
        active_jobs: active_job_count,
        termination_evidence: None,
        repair_plan: Vec::new(),
    });
    status.active_job_count = active_job_count;
    status.active_job_state = RunnerActiveJobState::Available;
    RunnerAdmissionSnapshot::from_status_and_generations(status, Vec::new(), Vec::new())
}

#[test]
fn refresh_routing_preserves_disconnected_ssh_execution_and_fences_unsafe_stale_daemons() {
    let runner = crate::Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: None,
        workspace_root: None,
        settings: Default::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: Default::default(),
    };
    let disconnected = RunnerAdmissionSnapshot::from_status_and_generations(
        refreshed_daemon_status(false, None),
        Vec::new(),
        Vec::new(),
    );

    let route = refresh_execution_route(&runner, &disconnected)
        .expect("disconnected SSH refresh retains diagnostic execution");
    let options = refresh_execution_options(
        &ssh_bootstrap_plan(),
        vec!["bash".to_string()],
        route.uses_diagnostic_ssh(),
    );
    assert!(options.allow_diagnostic_ssh);

    let unsafe_stale = stale_daemon_admission_snapshot(1, Some("lease-current"), Some(1));
    let error = refresh_execution_route(&runner, &unsafe_stale)
        .expect_err("connected stale daemon with active work remains fenced");
    assert_eq!(error.code.as_str(), "validation.invalid_argument");
    assert!(error.message.contains("permits daemon rotation"));

    let ambiguous_stale = stale_daemon_admission_snapshot(0, None, None);
    assert!(refresh_execution_route(&runner, &ambiguous_stale).is_err());
}

#[test]
fn diagnostic_ssh_bootstrap_preflight_requires_remote_materialization_tools() {
    let options = refresh_execution_options(
        &ssh_bootstrap_plan(),
        vec!["bash".to_string(), "git".to_string(), "cargo".to_string()],
        true,
    );

    assert_eq!(
        options
            .capability_preflight
            .expect("diagnostic SSH capability preflight")
            .required_commands,
        ["bash", "git", "cargo"],
        "diagnostic SSH must probe the remote shell before materialization"
    );
}

#[test]
fn connected_refresh_keeps_daemon_execution_options() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: "/runner/homeboy".to_string(),
        script: "probe".to_string(),
        reconnect: false,
        followup_commands: Vec::new(),
    };

    let options = refresh_execution_options(&plan, vec!["bash".to_string()], false);

    assert!(!options.allow_diagnostic_ssh);
    assert_eq!(options.diagnostic_ssh_timeout, None);
    assert!(options.mirror_evidence);
    assert!(options.print_handoff);
}

#[test]
fn refresh_ancestry_probe_executes_in_the_runner_repository() {
    let connected = refresh_ancestry_execution_options(
        "/runner/homeboy",
        "candidate-sha",
        "authority-sha",
        false,
    );
    assert_eq!(
        connected.command,
        vec![
            "git",
            "-C",
            "/runner/homeboy",
            "merge-base",
            "--is-ancestor",
            "candidate-sha",
            "authority-sha",
        ]
    );
    assert!(!connected.allow_diagnostic_ssh);
    assert_eq!(
        connected
            .capability_preflight
            .expect("connected preflight")
            .required_commands,
        vec!["git"]
    );
    assert!(connected.mirror_evidence);
    assert!(!connected.print_handoff);

    let disconnected = refresh_ancestry_execution_options(
        "/runner/homeboy",
        "candidate-sha",
        "authority-sha",
        true,
    );
    assert!(disconnected.allow_diagnostic_ssh);
    assert_eq!(
        disconnected.diagnostic_ssh_timeout,
        Some(DISCONNECTED_SSH_REFRESH_TIMEOUT)
    );
}

#[test]
fn refresh_phase_summary_distinguishes_tolerated_probe_from_required_failure() {
    assert_eq!(
        refresh_phase("downgrade_safety_probe", false, 1),
        HomeboyRefreshPhase {
            name: "downgrade_safety_probe",
            required: false,
            status: "tolerated_failure",
            exit_code: 1,
            job_id: None,
            mirror_run_id: None,
        }
    );
    assert_eq!(
        refresh_phase("materialize", true, 101),
        HomeboyRefreshPhase {
            name: "materialize",
            required: true,
            status: "failed",
            exit_code: 101,
            job_id: None,
            mirror_run_id: None,
        }
    );
    let select = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: "/runner/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    assert_eq!(refresh_execution_phase_name(&select), "select");
}

#[test]
fn fatal_ancestry_executor_output_retains_parent_actionable_refs() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: None,
        git_ref: Some("candidate".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/runner/homeboy/target/release/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };

    let probe =
        runner_commits_are_ancestral_with(&plan, None, "candidate", "authority", false, |_, _| {
            Ok((ancestry_exec_output(128), 128))
        })
        .expect("executor output is retained for classification");
    assert_eq!(probe.exit_code, 128);
    assert!(!probe.is_ancestor);
    let phase = refresh_ancestry_phase(&probe.execution);
    assert_eq!(
        vec![phase],
        vec![HomeboyRefreshPhase {
            name: "downgrade_safety_probe",
            required: true,
            status: "failed",
            exit_code: 128,
            job_id: Some("ancestry-job".to_string()),
            mirror_run_id: Some("ancestry-run".to_string()),
        }]
    );
    let failure = refresh_failure(&plan, probe.execution, probe.exit_code);
    assert_eq!(failure.job_id.as_deref(), Some("ancestry-job"));
    assert_eq!(failure.mirror_run_id.as_deref(), Some("ancestry-run"));
}

#[test]
fn terminal_refresh_errors_include_the_completed_and_failed_phases() {
    for phase in ["generation_rotation", "disconnect", "reconnect_transport"] {
        let error = refresh_error_with_phase_summary(
            Error::internal_unexpected("terminal refresh failure"),
            &[
                refresh_phase("materialize", true, 0),
                refresh_phase(phase, true, 1),
            ],
        );
        assert_eq!(error.details["phase_summary"][1]["name"], phase);
        assert_eq!(error.details["phase_summary"][1]["status"], "failed");
    }
}

#[test]
fn remote_forward_upgrade_uses_runner_owned_ancestry_evidence() {
    let (fixture, old, new) = linear_commit_fixture();
    let runner_repository = "/runner-only/homeboy";
    assert!(!Path::new(runner_repository).exists());
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: None,
        git_ref: Some("main".to_string()),
        target_dir: Some(runner_repository.to_string()),
        binary_path: format!("{runner_repository}/target/release/homeboy"),
        script: String::new(),
        reconnect: true,
        followup_commands: Vec::new(),
    };
    let mut probes = 0;

    let accepted = validate_refresh_promotion(
        &plan,
        &serde_json::json!({"data":{"git_commit":new}}),
        false,
        &RefreshPromotionAuthorities {
            controller: None,
            active_daemon: Some(old),
            configured_selected: None,
        },
        |older, newer| {
            runner_commits_are_ancestral_with(
                &plan,
                None,
                older,
                newer,
                false,
                |runner_id, mut options| {
                    probes += 1;
                    assert_eq!(runner_id, "lab");
                    assert_eq!(options.command[2], runner_repository);
                    options.command[2] = fixture.path().display().to_string();
                    let exit_code = Command::new(&options.command[0])
                        .args(&options.command[1..])
                        .status()
                        .expect("runner ancestry probe")
                        .code()
                        .expect("git exit code");
                    Ok((ancestry_exec_output(exit_code), exit_code))
                },
            )
            .map(|result| result.is_ancestor)
        },
    )
    .expect("forward upgrade is accepted");

    assert!(accepted.is_none());
    assert_eq!(probes, 1);
}

#[test]
fn remote_true_downgrade_is_still_refused_from_runner_owned_evidence() {
    let (fixture, old, new) = linear_commit_fixture();
    let runner_repository = "/runner-only/homeboy";
    assert!(!Path::new(runner_repository).exists());
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: None,
        git_ref: Some("old".to_string()),
        target_dir: Some(runner_repository.to_string()),
        binary_path: format!("{runner_repository}/target/release/homeboy"),
        script: String::new(),
        reconnect: true,
        followup_commands: Vec::new(),
    };

    let denied = validate_refresh_promotion(
        &plan,
        &serde_json::json!({"data":{"git_commit":old}}),
        false,
        &RefreshPromotionAuthorities {
            controller: None,
            active_daemon: Some(new),
            configured_selected: None,
        },
        |older, newer| {
            runner_commits_are_ancestral_with(
                &plan,
                None,
                older,
                newer,
                false,
                |runner_id, mut options| {
                    assert_eq!(runner_id, "lab");
                    assert_eq!(options.command[2], runner_repository);
                    options.command[2] = fixture.path().display().to_string();
                    let exit_code = Command::new(&options.command[0])
                        .args(&options.command[1..])
                        .status()
                        .expect("runner ancestry probe")
                        .code()
                        .expect("git exit code");
                    Ok((ancestry_exec_output(exit_code), exit_code))
                },
            )
            .map(|result| result.is_ancestor)
        },
    )
    .expect_err("true downgrade remains refused");

    assert_eq!(denied.details["field"], "allow_downgrade");
    assert!(denied.message.contains("refusing Homeboy runner downgrade"));
}

#[test]
fn managed_select_authority_accepts_forward_and_equal_commits() {
    let (fixture, old, new) = linear_commit_fixture();
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: "/runner/_homeboy_binaries/homeboy-hash/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let stdout = format!(
        "HOMEBOY_REFRESH_MANAGED_SOURCE=https://example.test/homeboy.git\nHOMEBOY_REFRESH_MANAGED_CHECKOUT={}\nHOMEBOY_REFRESH_MANAGED_REF=main\nHOMEBOY_REFRESH_MANAGED_COMMIT={new}\n{{\"data\":{{\"git_commit\":\"{new}\"}}}}",
        fixture.path().display(),
    );
    let candidate = parse_identity(&stdout).expect("managed binary identity");
    let checkout = managed_slot_checkout(&plan, &stdout, &candidate)
        .expect("managed metadata is valid")
        .expect("managed metadata supplies checkout");

    for authority in [&old, &new] {
        assert!(validate_refresh_promotion(
            &plan,
            &candidate,
            false,
            &RefreshPromotionAuthorities {
                controller: None,
                active_daemon: Some(authority.to_string()),
                configured_selected: None,
            },
            |older, newer| fixture_commits_are_ancestral(Path::new(&checkout), older, newer),
        )
        .expect("forward and equal managed selections are accepted")
        .is_none());
    }
}

#[test]
fn managed_select_authority_rejects_true_downgrade_and_unknown_slots() {
    let (fixture, old, new) = linear_commit_fixture();
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "select".to_string(),
        source: None,
        git_ref: None,
        target_dir: None,
        binary_path: "/arbitrary/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let unknown = r#"{"data":{"git_commit":"unknown"}}"#;
    assert_eq!(
        managed_slot_checkout(&plan, unknown, &parse_identity(unknown).expect("identity"))
            .expect("unknown slots remain untrusted"),
        None
    );

    let stdout = format!(
        "HOMEBOY_REFRESH_MANAGED_CHECKOUT={}\nHOMEBOY_REFRESH_MANAGED_COMMIT={old}\n{{\"data\":{{\"git_commit\":\"{old}\"}}}}",
        fixture.path().display(),
    );
    let candidate = parse_identity(&stdout).expect("managed binary identity");
    let checkout = managed_slot_checkout(&plan, &stdout, &candidate)
        .expect("managed metadata is valid")
        .expect("managed metadata supplies checkout");
    let error = validate_refresh_promotion(
        &plan,
        &candidate,
        false,
        &RefreshPromotionAuthorities {
            controller: None,
            active_daemon: Some(new),
            configured_selected: None,
        },
        |older, newer| fixture_commits_are_ancestral(Path::new(&checkout), older, newer),
    )
    .expect_err("true managed downgrade stays rejected");
    assert_eq!(error.details["field"], "allow_downgrade");
}

#[test]
fn materialized_identity_must_match_the_resolved_ref_and_be_clean() {
    let plan = HomeboyBinaryRefreshPlan {
        runner_id: "lab".to_string(),
        mode: "materialize".to_string(),
        source: Some("source".to_string()),
        git_ref: Some("accepted-sha".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/runner/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    };
    let wrong_identity = serde_json::json!({
        "data": { "git_commit": "badc0ffee", "git_dirty": false }
    });

    let error = verify_materialized_identity(
        &plan,
        "HOMEBOY_REFRESH_SOURCE_SHA=accepted-sha-123456\n",
        &wrong_identity,
    )
    .expect_err("a different built commit must not be selected");

    assert!(error.contains("does not match resolved ref"));
}

#[test]
fn materialized_identity_accepts_production_clean_envelope_without_dirty_metadata() {
    let plan = ssh_bootstrap_plan();
    let source_sha = "18915b824fdf";
    let identity = serde_json::json!({
        "success": true,
        "data": {
            "version": "0.284.1",
            "git_commit": source_sha,
            "display": "homeboy 0.284.1+18915b824fdf"
        }
    });

    verify_materialized_identity(
        &plan,
        &format!("HOMEBOY_REFRESH_SOURCE_SHA={source_sha}\n"),
        &identity,
    )
    .expect("production clean identity is accepted");
}

#[test]
fn materialized_identity_accepts_explicit_clean_state() {
    let plan = ssh_bootstrap_plan();
    let identity = serde_json::json!({
        "data": { "git_commit": "abc123", "git_dirty": false }
    });

    verify_materialized_identity(&plan, "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n", &identity)
        .expect("explicitly clean identity is accepted");
}

#[test]
fn materialized_identity_rejects_explicit_dirty_state() {
    let plan = ssh_bootstrap_plan();
    let identity = serde_json::json!({
        "data": { "git_commit": "abc123", "git_dirty": true }
    });

    let error =
        verify_materialized_identity(&plan, "HOMEBOY_REFRESH_SOURCE_SHA=abc123\n", &identity)
            .expect_err("explicitly dirty identity is rejected");

    assert!(error.contains("not a clean build"));
}
