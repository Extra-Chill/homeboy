use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use super::{
    artifact_content_url, can_recover_startup_attempt, ensure_running_with_operations,
    fetch_artifact_to_path, observe_startup_lease,
    terminate_token_owned_startup_process_with_operations,
};
use crate::api_jobs::{JobStatus, JobStore};
use crate::build_identity::BuildIdentity;
use crate::daemon::{
    DaemonFreshnessReport, DaemonRuntimeSnapshot, DaemonStaleReasonCode, DaemonState, DaemonStatus,
    DaemonTerminationClassification, DaemonTerminationEvidence,
};
use crate::process::{SIGNAL_KILL, SIGNAL_TERMINATE};
use crate::test_support::with_isolated_home;

#[cfg(unix)]
#[test]
fn detached_daemon_owner_does_not_inherit_the_launcher_session() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf '%s\\n' \"$$\"; sleep 30"]);
    super::detach_from_launcher_session(&mut command);
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("child");
    let mut stdout = String::new();
    BufReader::new(child.stdout.take().expect("child stdout"))
        .read_line(&mut stdout)
        .expect("read child pid");
    let pid = stdout.trim().parse::<libc::pid_t>().expect("child pid");

    // A session leader has a session ID equal to its PID, so the child can no
    // longer receive SSH-launcher session teardown signals.
    assert_eq!(unsafe { libc::getsid(pid) }, pid);
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGTERM) }, 0);
    child.wait().expect("reap child");
}

#[derive(Default)]
struct FakeEnsureState {
    daemon: Option<super::DaemonStartResult>,
    starts: usize,
}

#[test]
fn completed_leaseless_recovery_replays_the_exact_replacement_without_starting_another() {
    with_isolated_home(|_| {
        let replacement = fake_daemon(4343, "replacement-lease");
        let state = fake_daemon_state(replacement.clone());
        let status = DaemonStatus {
            running: true,
            fresh: true,
            reachable: true,
            freshness: DaemonFreshnessReport {
                fresh: true,
                stale_reason_code: None,
                restartable: false,
                lease_id: Some(replacement.lease_id.clone()),
                pid: Some(replacement.pid),
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
            },
            stale_reason: None,
            state: Some(state),
            state_path: "/fake/daemon-state.json".to_string(),
            state_identity: "fresh-replacement".to_string(),
            process_candidates: Vec::new(),
            active_job_recovery_evidence: Vec::new(),
            termination_evidence: None,
        };
        let receipt = super::LeaselessRecoveryReceipt {
            affected_job_ids: Vec::new(),
            affected_jobs: Vec::new(),
            historical_lease_ids: vec!["stale-lease".to_string()],
            evidence_snapshot_path: "/fake/recovery.snapshot".to_string(),
            ownership_proof: vec!["owner lock was acquired".to_string()],
            phase: super::StateLossRecoveryPhase::ReplacementStarted,
            replacement: Some(replacement.clone()),
            replacement_startup_token: Some("fake-startup-token".to_string()),
            replacement_operation_id: None,
        };
        let path = crate::paths::daemon_leaseless_recovery_receipt_file().expect("receipt path");
        super::write_leaseless_recovery_receipt(&path, &receipt).expect("write receipt");

        let replay = super::replay_leaseless_recovery(&status, "127.0.0.1:0", None)
            .expect("replay lookup")
            .expect("exact completed recovery replays");

        assert_eq!(replay.replacement, replacement);
        assert_eq!(replay.affected_job_count, 0);
        assert!(replay
            .retry_guidance
            .contains("no additional daemon was started"));
    });
}

#[test]
fn daemon_process_attribution_falls_back_to_conventional_home_store() {
    let home = tempfile::tempdir().expect("home");
    let jobs = home.path().join(".config/homeboy/daemon/jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4242 {} HOME={} {} daemon serve --addr 127.0.0.1:7421",
        executable.display(),
        home.path().display(),
        executable.display()
    );

    let candidate =
        super::parse_daemon_process_candidate(&line, &jobs, Some(&executable)).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Owning
    );
    assert_eq!(candidate.bind_endpoint.as_deref(), Some("127.0.0.1:7421"));
    assert_eq!(candidate.durable_store_path.as_deref(), jobs.to_str());
}

#[test]
fn daemon_process_attribution_matches_explicit_state_directory_store() {
    let home = tempfile::tempdir().expect("home");
    let state_dir = tempfile::tempdir().expect("state directory");
    let jobs = state_dir.path().join("jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4243 {} HOME={} HOMEBOY_DAEMON_STATE_DIR={} {} daemon serve --addr 127.0.0.1:7421",
        executable.display(),
        home.path().display(),
        state_dir.path().display(),
        executable.display()
    );

    let candidate =
        super::parse_daemon_process_candidate(&line, &jobs, Some(&executable)).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Owning
    );
    assert_eq!(candidate.durable_store_path.as_deref(), jobs.to_str());
}

#[test]
fn daemon_process_attribution_classifies_multiple_quoted_generation_state_directory_stores() {
    let home = tempfile::tempdir().expect("home");
    let generation_root = tempfile::tempdir().expect("generation root");
    let jobs = home.path().join(".config/homeboy/daemon/jobs.json");
    let executable = std::env::current_exe().expect("current executable");

    for (generation, quote) in [("generation-a", '\''), ("generation-b", '"')] {
        let state_dir = generation_root.path().join(generation);
        let line = format!(
            "4243 {} HOME={} HOMEBOY_DAEMON_STATE_DIR={quote}{}{quote} {} daemon serve --addr 127.0.0.1:7421",
            executable.display(),
            home.path().display(),
            state_dir.display(),
            executable.display(),
        );
        let candidate = super::parse_daemon_process_candidate(&line, &jobs, Some(&executable))
            .expect("candidate");
        assert_eq!(
            candidate.ownership,
            super::super::DaemonProcessOwnership::Unrelated,
            "{line}"
        );
        assert_eq!(
            candidate.durable_store_path.as_deref(),
            state_dir.join("jobs.json").to_str()
        );
    }
}

#[test]
fn daemon_process_attribution_keeps_quoted_same_store_ambiguous_without_binary_proof() {
    let state_dir = tempfile::tempdir().expect("state directory");
    let jobs = state_dir.path().join("jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4243 {} HOMEBOY_DAEMON_STATE_DIR=\"{}\" {} daemon serve --addr 127.0.0.1:7421",
        executable.display(),
        state_dir.path().display(),
        executable.display(),
    );

    let candidate = super::parse_daemon_process_candidate(&line, &jobs, None).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Ambiguous
    );
    assert_eq!(candidate.durable_store_path.as_deref(), jobs.to_str());
}

#[test]
fn daemon_process_attribution_keeps_whitespace_environment_assignments_ambiguous() {
    let root = tempfile::tempdir().expect("state root");
    let state_dir = root.path().join("state directory");
    let home = root.path().join("home directory");
    std::fs::create_dir(&state_dir).expect("state directory");
    std::fs::create_dir(&home).expect("home directory");
    let executable = std::env::current_exe().expect("current executable");

    for assignment in [
        format!("HOMEBOY_DAEMON_STATE_DIR={}", state_dir.display()),
        format!("HOME={}", home.display()),
    ] {
        let line = format!(
            "4243 {} {} {} daemon serve --addr 127.0.0.1:7421",
            executable.display(),
            assignment,
            executable.display(),
        );
        let candidate = super::parse_daemon_process_candidate(
            &line,
            &state_dir.join("jobs.json"),
            Some(&executable),
        )
        .expect("candidate");
        assert_eq!(
            candidate.ownership,
            super::super::DaemonProcessOwnership::Ambiguous,
            "{assignment}"
        );
        assert_eq!(candidate.durable_store_path, None, "{assignment}");
    }
}

#[test]
fn daemon_process_attribution_matches_state_directory_argument() {
    let state_dir = tempfile::tempdir().expect("state directory");
    let jobs = state_dir.path().join("jobs.json");
    let executable = std::env::current_exe().expect("current executable");

    for state_dir_argument in [
        format!("--state-dir {}", state_dir.path().display()),
        format!("--state-dir={}", state_dir.path().display()),
    ] {
        let line = format!(
            "4243 {} {} daemon serve --addr 127.0.0.1:7421 {}",
            executable.display(),
            executable.display(),
            state_dir_argument,
        );
        let candidate = super::parse_daemon_process_candidate(&line, &jobs, Some(&executable))
            .expect("candidate");
        assert_eq!(
            candidate.ownership,
            super::super::DaemonProcessOwnership::Owning,
            "{state_dir_argument}"
        );
        assert_eq!(candidate.durable_store_path.as_deref(), jobs.to_str());
    }
}

#[test]
fn daemon_process_attribution_keeps_whitespace_state_directory_arguments_ambiguous() {
    let root = tempfile::tempdir().expect("state root");
    let state_dir = root.path().join("state directory");
    std::fs::create_dir(&state_dir).expect("state directory");
    let jobs = state_dir.join("jobs.json");
    let executable = std::env::current_exe().expect("current executable");

    for state_dir_argument in [
        format!("--state-dir {}", state_dir.display()),
        format!("--state-dir={}", state_dir.display()),
        format!("--state-dir \"{}\"", state_dir.display()),
        format!("--state-dir=\"{}\"", state_dir.display()),
    ] {
        let line = format!(
            "4243 {} {} daemon serve {}",
            executable.display(),
            executable.display(),
            state_dir_argument,
        );
        let candidate = super::parse_daemon_process_candidate(&line, &jobs, Some(&executable))
            .expect("candidate");
        assert_eq!(
            candidate.ownership,
            super::super::DaemonProcessOwnership::Ambiguous,
            "{state_dir_argument}"
        );
        assert_eq!(candidate.durable_store_path, None, "{state_dir_argument}");
    }
}

#[test]
fn daemon_process_attribution_keeps_same_state_directory_argument_ambiguous_without_binary_proof() {
    let state_dir = tempfile::tempdir().expect("state directory");
    let jobs = state_dir.path().join("jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4243 {} {} daemon serve --state-dir {}",
        executable.display(),
        executable.display(),
        state_dir.path().display(),
    );

    let candidate = super::parse_daemon_process_candidate(&line, &jobs, None).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Ambiguous
    );
    assert_eq!(candidate.durable_store_path.as_deref(), jobs.to_str());
}

#[test]
fn daemon_process_attribution_state_directory_argument_overrides_environment_and_home() {
    let home = tempfile::tempdir().expect("home");
    let environment_state_dir = tempfile::tempdir().expect("environment state directory");
    let argument_state_dir = tempfile::tempdir().expect("argument state directory");
    let jobs = argument_state_dir.path().join("jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4243 {} HOME={} HOMEBOY_DAEMON_STATE_DIR={} {} daemon serve --state-dir {}",
        executable.display(),
        home.path().display(),
        environment_state_dir.path().display(),
        executable.display(),
        argument_state_dir.path().display(),
    );

    let candidate =
        super::parse_daemon_process_candidate(&line, &jobs, Some(&executable)).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Owning
    );
    assert_eq!(candidate.durable_store_path.as_deref(), jobs.to_str());
}

#[test]
fn daemon_process_attribution_proves_explicit_different_state_directory_unrelated() {
    let home = tempfile::tempdir().expect("home");
    let other_state_dir = tempfile::tempdir().expect("other state directory");
    let jobs = home.path().join(".config/homeboy/daemon/jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4244 {} HOME={} HOMEBOY_DAEMON_STATE_DIR={} {} daemon serve --addr 127.0.0.1:7421",
        executable.display(),
        home.path().display(),
        other_state_dir.path().display(),
        executable.display()
    );

    let candidate =
        super::parse_daemon_process_candidate(&line, &jobs, Some(&executable)).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Unrelated
    );
    assert_eq!(
        candidate.durable_store_path.as_deref(),
        other_state_dir.path().join("jobs.json").to_str()
    );
}

#[test]
fn daemon_process_attribution_proves_state_directory_argument_different_store_unrelated() {
    let home = tempfile::tempdir().expect("home");
    let other_state_dir = tempfile::tempdir().expect("other state directory");
    let jobs = home.path().join(".config/homeboy/daemon/jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4244 {} HOME={} {} daemon serve --state-dir={}",
        executable.display(),
        home.path().display(),
        executable.display(),
        other_state_dir.path().display(),
    );

    let candidate =
        super::parse_daemon_process_candidate(&line, &jobs, Some(&executable)).expect("candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Unrelated
    );
    assert_eq!(
        candidate.durable_store_path.as_deref(),
        other_state_dir.path().join("jobs.json").to_str()
    );
}

#[test]
fn daemon_process_attribution_proves_unrelated_and_keeps_unbound_daemon_ambiguous() {
    let home = tempfile::tempdir().expect("home");
    let other_home = tempfile::tempdir().expect("other home");
    let jobs = home.path().join(".config/homeboy/daemon/jobs.json");
    let executable = std::env::current_exe().expect("current executable");
    let unrelated = format!(
        "4243 {} HOME={} {} daemon serve --addr 127.0.0.1:7421",
        executable.display(),
        other_home.path().display(),
        executable.display()
    );
    let ambiguous = format!(
        "4244 {} {} daemon serve --addr 127.0.0.1:7421",
        executable.display(),
        executable.display()
    );

    assert_eq!(
        super::parse_daemon_process_candidate(&unrelated, &jobs, Some(&executable))
            .expect("unrelated")
            .ownership,
        super::super::DaemonProcessOwnership::Unrelated
    );
    assert_eq!(
        super::parse_daemon_process_candidate(&ambiguous, &jobs, Some(&executable))
            .expect("unbound daemon candidate")
            .ownership,
        super::super::DaemonProcessOwnership::Ambiguous
    );
}

#[cfg(target_os = "linux")]
#[test]
fn candidate_attribution_reads_command_env_store_identity_from_procfs() {
    let state_dir = tempfile::tempdir().expect("state directory");
    let mut process = Command::new("sh")
        .args(["-c", "sleep 30"])
        .env(crate::paths::DAEMON_STATE_DIR_ENV, state_dir.path())
        .spawn()
        .expect("process with daemon state environment");

    let deadline = Instant::now() + Duration::from_secs(1);
    let store = loop {
        if let Some(store) = super::process_durable_store_path(process.id(), true) {
            break store;
        }
        assert!(Instant::now() < deadline, "procfs store path");
        std::thread::sleep(Duration::from_millis(10));
    };
    process.kill().expect("stop process");
    process.wait().expect("reap process");

    assert_eq!(store, state_dir.path().join("jobs.json"));
}

#[test]
fn ambiguous_command_state_uses_explicit_process_store_without_home_fallback() {
    let selected_state = tempfile::tempdir().expect("selected state directory");
    let candidate_state = tempfile::tempdir().expect("candidate state directory");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4243 {} {} daemon serve --state-dir /flattened/ambiguous trailing-ps-text",
        executable.display(),
        executable.display()
    );
    let selected_jobs = selected_state.path().join("jobs.json");
    let mut candidate =
        super::parse_daemon_process_candidate(&line, &selected_jobs, None).expect("candidate");
    assert_eq!(candidate.durable_store_path, None);

    let mut allow_home_seen = None;
    super::recover_candidate_store_from_process_environment(
        &mut candidate,
        &selected_jobs,
        Some(false),
        |pid, allow_home| {
            assert_eq!(pid, 4243);
            allow_home_seen = Some(allow_home);
            Some(candidate_state.path().join("jobs.json"))
        },
    );

    assert_eq!(allow_home_seen, Some(false));
    assert_eq!(
        candidate.durable_store_path.as_deref(),
        candidate_state.path().join("jobs.json").to_str()
    );
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Unrelated
    );
}

#[test]
fn absent_command_state_allows_process_home_fallback_but_keeps_same_store_safe() {
    let state = tempfile::tempdir().expect("state directory");
    let executable = std::env::current_exe().expect("current executable");
    let line = format!(
        "4243 {} {} daemon serve --addr 127.0.0.1:0",
        executable.display(),
        executable.display()
    );
    let jobs = state.path().join("jobs.json");
    let mut candidate =
        super::parse_daemon_process_candidate(&line, &jobs, None).expect("candidate");

    super::recover_candidate_store_from_process_environment(
        &mut candidate,
        &jobs,
        Some(true),
        |_, allow_home| {
            assert!(allow_home);
            Some(jobs.clone())
        },
    );

    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Ambiguous,
        "store evidence does not replace executable identity proof"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn ambiguous_ps_state_dir_uses_only_explicit_procfs_state_identity() {
    let selected_state = tempfile::tempdir().expect("selected state directory");
    let candidate_state = tempfile::tempdir().expect("candidate state directory");
    let mut process = Command::new("sh")
        .args([
            "-c",
            "trap 'kill \"$child\" 2>/dev/null' TERM EXIT; sleep 30 & child=$!; wait \"$child\"",
            "homeboy",
            "daemon",
            "serve",
            "--addr",
            "127.0.0.1:0",
            "--startup-token",
            "candidate-token",
            "--state-dir",
            "/flattened/ambiguous",
            "trailing-ps-text",
        ])
        .env(crate::paths::DAEMON_STATE_DIR_ENV, candidate_state.path())
        .spawn()
        .expect("candidate process");

    let candidates = super::daemon_process_candidates(&selected_state.path().join("jobs.json"))
        .expect("candidate scan");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.pid == process.id())
        .expect("spawned candidate");
    assert_eq!(
        candidate.durable_store_path.as_deref(),
        candidate_state.path().join("jobs.json").to_str()
    );
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Unrelated
    );

    let candidates = super::daemon_process_candidates(&candidate_state.path().join("jobs.json"))
        .expect("same-store candidate scan");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.pid == process.id())
        .expect("same-store candidate");
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Ambiguous,
        "procfs store proof does not replace executable identity proof"
    );

    process.kill().expect("stop candidate process");
    process.wait().expect("reap candidate process");
}

#[cfg(target_os = "linux")]
#[test]
fn ambiguous_ps_state_dir_does_not_fall_back_to_procfs_home() {
    let home = tempfile::tempdir().expect("candidate home");
    let mut process = Command::new("sh")
        .args([
            "-c",
            "trap 'kill \"$child\" 2>/dev/null' TERM EXIT; sleep 30 & child=$!; wait \"$child\"",
            "homeboy",
            "daemon",
            "serve",
            "--state-dir",
            "/flattened/ambiguous",
            "trailing-ps-text",
        ])
        .env_remove(crate::paths::DAEMON_STATE_DIR_ENV)
        .env("HOME", home.path())
        .spawn()
        .expect("candidate process");

    let candidates =
        super::daemon_process_candidates(&home.path().join(".config/homeboy/daemon/jobs.json"))
            .expect("candidate scan");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.pid == process.id())
        .expect("spawned candidate");
    assert_eq!(candidate.durable_store_path, None);
    assert_eq!(
        candidate.ownership,
        super::super::DaemonProcessOwnership::Ambiguous
    );

    process.kill().expect("stop candidate process");
    process.wait().expect("reap candidate process");
}

#[test]
fn multiple_ambiguous_candidates_block_recovery_and_pid_reuse_blocks_adoption() {
    let ambiguous = super::super::DaemonProcessCandidate {
        pid: 71,
        process_start_identity: None,
        executable: "/tmp/homeboy".to_string(),
        cmdline: "homeboy daemon serve --addr 127.0.0.1:0".to_string(),
        bind_endpoint: Some("127.0.0.1:0".to_string()),
        durable_store_path: None,
        build_identity: None,
        startup_token: None,
        ownership: super::super::DaemonProcessOwnership::Ambiguous,
    };
    assert!(!super::candidates_prove_no_owner(&[
        ambiguous.clone(),
        ambiguous
    ]));
    assert!(super::pid_is_proven_dead(71, |_| false));
    assert!(
        !super::pid_is_proven_dead(71, |_| true),
        "a reused PID remains live under the lifecycle lock"
    );
}

#[test]
fn dead_persisted_lease_with_ambiguous_candidates_refuses_replacement_before_spawn() {
    let candidate = super::super::DaemonProcessCandidate {
        pid: 71,
        process_start_identity: None,
        executable: "/tmp/homeboy".to_string(),
        cmdline: "homeboy daemon serve --addr 127.0.0.1:0".to_string(),
        bind_endpoint: Some("127.0.0.1:0".to_string()),
        durable_store_path: None,
        build_identity: None,
        startup_token: None,
        ownership: super::super::DaemonProcessOwnership::Ambiguous,
    };
    let state = tempfile::tempdir().expect("state directory");
    let state_path = state.path().join("state.json");
    std::fs::write(&state_path, "dead persisted lease").expect("persist dead lease");
    let started = std::time::Instant::now();

    let error = super::refuse_unleased_process_conflict_with_candidates(
        &state_path,
        vec![candidate.clone(), candidate],
    )
    .expect_err("ambiguous foreground processes block a dead-lease replacement");

    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(
        error.details["classification"],
        "daemon_unleased_process_conflict"
    );
    assert_eq!(error.details["candidate_count"], 2);
    assert_eq!(
        error.details["candidates"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn artifact_content_url_builds_encoded_daemon_byte_alias() {
    let url = artifact_content_url(
        "http://127.0.0.1:7421/base?ignored=true",
        "run 1",
        "report/summary.json",
    )
    .expect("url");

    assert_eq!(
        url,
        "http://127.0.0.1:7421/runs/run%201/artifacts/report%2Fsummary.json/content"
    );
}

#[test]
fn leaseless_snapshot_uses_the_exact_bytes_opened_for_reconciliation() {
    with_isolated_home(|home| {
        let path = home.path().join("jobs.json");
        let raw = br#"{"jobs":[]}"#;
        std::fs::write(&path, raw).expect("write store");
        let snapshot = super::snapshot_job_store(&path, raw).expect("snapshot");
        assert_eq!(std::fs::read(snapshot).expect("read snapshot"), raw);
        assert!(super::super::JobStore::open_without_reconciliation_from_bytes(&path, raw).is_ok());
    });
}

#[test]
fn exact_lease_adoption_refuses_owner_lock_and_preserves_store() {
    with_isolated_home(|_| {
        let state_path = crate::paths::daemon_state_file().expect("state path");
        let mut state = fake_daemon_state(fake_daemon(999_999, "lease-dead"));
        state.state_path = state_path.display().to_string();
        std::fs::create_dir_all(state_path.parent().expect("state parent")).expect("state parent");
        std::fs::write(&state_path, serde_json::to_vec(&state).expect("state json"))
            .expect("state");

        let jobs_path = crate::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&jobs_path)
            .expect("store")
            .with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("start job");
        let before = std::fs::read(&jobs_path).expect("store bytes");
        let owner = super::super::try_acquire_daemon_owner_lock()
            .expect("owner lock")
            .expect("owner acquired");

        let error = super::adopt_orphaned_lease("lease-dead", &[], "127.0.0.1:0")
            .expect_err("owner lock blocks adoption");
        assert!(error.message.contains("owner lock is held"));
        assert_eq!(std::fs::read(&jobs_path).expect("store bytes"), before);
        assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
        drop(owner);
    });
}

#[test]
fn legacy_child_recovery_refuses_operation_and_owner_locks_without_mutation() {
    with_isolated_home(|_| {
        let (store, job, endpoint) = legacy_recovery_fixture("lease-dead");
        let before = std::fs::read(crate::paths::daemon_jobs_file().expect("jobs path"))
            .expect("store bytes");

        let operation = super::super::acquire_daemon_operation_lock().expect("operation lock");
        let operation_error = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("operation lock blocks recovery");
        assert!(operation_error
            .message
            .contains("operation already in progress"));
        assert_eq!(
            std::fs::read(crate::paths::daemon_jobs_file().expect("jobs path"))
                .expect("store bytes"),
            before
        );
        drop(operation);

        let owner = super::super::try_acquire_daemon_owner_lock()
            .expect("owner lock")
            .expect("owner acquired");
        let owner_error = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("owner lock blocks recovery");
        assert!(owner_error.message.contains("owner lock is held"));
        assert_eq!(store.get(job.id).expect("job").status, JobStatus::Running);
        drop(owner);
    });
}

#[test]
fn legacy_child_recovery_refuses_live_daemon_endpoint_and_lease_mismatch() {
    with_isolated_home(|_| {
        let (_store, job, endpoint) = legacy_recovery_fixture("lease-dead");
        write_legacy_recovery_state("lease-dead", std::process::id(), &endpoint);
        let live_pid = super::recover_missing_child_identity(
            "lease-dead",
            std::process::id(),
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("live recorded daemon PID blocks recovery");
        assert!(live_pid.message.contains("recorded daemon PID is live"));

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let reachable = listener.local_addr().expect("endpoint").to_string();
        write_legacy_recovery_state("lease-dead", u32::MAX, &reachable);
        let live_endpoint = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &reachable,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("reachable endpoint blocks recovery");
        assert!(live_endpoint.message.contains("is reachable"));
        drop(listener);

        write_legacy_recovery_state("other-lease", u32::MAX, &endpoint);
        let mismatch = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("mismatched lease blocks recovery");
        assert!(mismatch
            .message
            .contains("does not match current daemon state"));
    });
}

#[test]
fn legacy_child_recovery_requires_matching_persisted_lease_address() {
    with_isolated_home(|_| {
        let (_store, job, endpoint) = legacy_recovery_fixture("lease-dead");
        let jobs_path = crate::paths::daemon_jobs_file().expect("jobs path");
        let before = std::fs::read(&jobs_path).expect("store bytes");
        std::fs::remove_file(crate::paths::daemon_state_file().expect("state path"))
            .expect("remove state");
        let missing = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("missing persisted lease blocks recovery");
        assert!(missing
            .message
            .contains("requires the persisted daemon lease record"));
        assert_eq!(std::fs::read(&jobs_path).expect("store bytes"), before);

        write_legacy_recovery_state("lease-dead", u32::MAX, "127.0.0.1:4242");
        let mismatch = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("persisted address must match supplied endpoint");
        assert!(mismatch.message.contains("endpoint does not match"));
        assert_eq!(std::fs::read(&jobs_path).expect("store bytes"), before);
    });
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_child_recovery_exact_evidence_is_idempotent_and_conflicts_fail_closed() {
    with_isolated_home(|_| {
        let (_store, job, endpoint) = legacy_recovery_fixture("lease-dead");
        let recovered = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect("absent child identity recovers exactly one job");
        let events = JobStore::open_without_reconciliation(
            crate::paths::daemon_jobs_file().expect("jobs path"),
        )
        .expect("reopen recovered store")
        .events(job.id)
        .expect("events");
        assert_eq!(recovered.status, JobStatus::Failed);
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.data.as_ref().is_some_and(|data| {
                        data["reason"] == "operator_legacy_child_identity_recovery"
                    })
                })
                .count(),
            1
        );

        let replay = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect("identical evidence is idempotent");
        assert_eq!(replay.id, recovered.id);
        assert_eq!(replay.status, recovered.status);
        assert_eq!(
            JobStore::open_without_reconciliation(
                crate::paths::daemon_jobs_file().expect("jobs path"),
            )
            .expect("reopen replayed store")
            .events(job.id)
            .expect("events")
            .len(),
            events.len()
        );

        let conflict = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            2,
        )
        .expect_err("conflicting replay evidence fails closed");
        assert!(conflict.message.contains("conflicts"));
    });
}

#[cfg(not(target_os = "linux"))]
#[test]
fn legacy_child_recovery_exact_evidence_is_idempotent_and_conflicts_fail_closed() {
    with_isolated_home(|_| {
        let (_store, job, endpoint) = legacy_recovery_fixture("lease-dead");
        let error = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect_err("Linux-only identity recovery fails closed elsewhere");
        assert!(error.message.contains("cannot verify Linux child identity"));
    });
}

#[test]
fn exact_no_pid_recovery_requires_unexpected_termination_and_refuses_process_contradiction() {
    let daemon = fake_daemon(4242, "lease-dead");
    let mismatch = super::reconcile_dead_lease_orphans_with_operations(
        "other-lease",
        super::DeadLeaseOrphanRecoveryOperations {
            status: || Ok(dead_status_with_unexpected_termination(daemon.clone())),
            pid_is_running: |_| false,
            acquire_owner: || Ok(Some(())),
            prove_no_owner: || unreachable!("lease mismatch blocks owner probe"),
            reconcile: |_| unreachable!("lease mismatch blocks mutation"),
            start: || unreachable!("lease mismatch blocks replacement"),
        },
    )
    .expect_err("mismatched lease is refused");
    assert!(mismatch.message.contains("does not match"));

    let missing = super::reconcile_dead_lease_orphans_with_operations(
        "lease-dead",
        super::DeadLeaseOrphanRecoveryOperations {
            status: || Ok(fake_dead_status(daemon.clone())),
            pid_is_running: |_| false,
            acquire_owner: || Ok(Some(())),
            prove_no_owner: || Ok(vec!["no owner".to_string()]),
            reconcile: |_| unreachable!("missing evidence blocks mutation"),
            start: || unreachable!("missing evidence blocks replacement"),
        },
    )
    .expect_err("missing unexpected-exit evidence is refused");
    assert!(missing.message.contains("unexpected-termination evidence"));

    let status = dead_status_with_unexpected_termination(daemon.clone());
    let contradiction = super::reconcile_dead_lease_orphans_with_operations(
        "lease-dead",
        super::DeadLeaseOrphanRecoveryOperations {
            status: || Ok(status),
            pid_is_running: |_| false,
            acquire_owner: || Ok(Some(())),
            prove_no_owner: || {
                Err(crate::error::Error::validation_invalid_argument(
                    "owner_probe",
                    "live workload process evidence contradicts operator confirmation",
                    None,
                    None,
                ))
            },
            reconcile: |_| unreachable!("contradictory process evidence blocks mutation"),
            start: || unreachable!("contradictory process evidence blocks replacement"),
        },
    )
    .expect_err("contradictory process evidence is refused");
    assert!(contradiction.message.contains("contradicts"));
}

#[test]
fn exact_no_pid_recovery_starts_only_after_reconciliation() {
    let daemon = fake_daemon(4242, "lease-dead");
    let job = uuid::Uuid::new_v4();
    let result = super::reconcile_dead_lease_orphans_with_operations(
        "lease-dead",
        super::DeadLeaseOrphanRecoveryOperations {
            status: || Ok(dead_status_with_unexpected_termination(daemon)),
            pid_is_running: |_| false,
            acquire_owner: || Ok(Some(())),
            prove_no_owner: || Ok(vec!["owner lock acquired".to_string()]),
            reconcile: |_| {
                Ok(crate::api_jobs::DaemonLeaseJobDiagnostics {
                    expected_lease_id: "lease-dead".to_string(),
                    matching_job_ids: vec![job],
                    ..Default::default()
                })
            },
            start: || Ok(fake_daemon(4343, "replacement")),
        },
    )
    .expect("exact reconciliation succeeds before replacement");
    assert_eq!(result.reconciled_job_ids, vec![job]);
    assert_eq!(
        result.termination_evidence.classification,
        DaemonTerminationClassification::UnexpectedExit
    );
    assert_eq!(result.replacement.lease_id, "replacement");
}

#[test]
fn fetch_artifact_to_path_downloads_daemon_byte_alias() {
    with_isolated_home(|home| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0; 1024];
            let bytes = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request
                .starts_with("GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                )
                .expect("response");
        });
        let output = home.path().join("summary.json");

        let outcome = fetch_artifact_to_path(
            "run-1",
            "report/summary.json",
            Some(format!("http://{addr}")),
            Some(output.clone()),
        )
        .expect("artifact get");

        server.join().expect("server");
        assert_eq!(outcome.content_type.as_deref(), Some("application/json"));
        assert_eq!(outcome.size_bytes, 11);
        assert_eq!(outcome.sha256.as_deref(), Some("abc123"));
        assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
    });
}

#[test]
fn ensure_running_times_out_when_lifecycle_lock_remains_held() {
    with_isolated_home(|_| {
        let _lock = super::super::acquire_daemon_operation_lock().expect("hold lifecycle lock");

        let err = ensure_running_with_operations(
            Duration::from_millis(10),
            super::super::acquire_daemon_operation_lock_for_ensure,
            || unreachable!("lock acquisition should time out first"),
            |_| unreachable!("lock acquisition should time out first"),
            || unreachable!("lock acquisition should time out first"),
        )
        .expect_err("ensure should time out behind held lock");

        assert!(err.message.contains("timed out"));
        assert!(err.message.contains("ensure-running lifecycle lock"));
    });
}

#[test]
fn ensure_running_returns_stale_live_daemon_without_starting_duplicate() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-stale");
        let state = Arc::new(Mutex::new(FakeEnsureState {
            daemon: Some(daemon.clone()),
            starts: 0,
        }));
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);

        let attached = ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    false,
                ))
            },
            |pid| pid == daemon.pid,
            move || {
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                Ok(fake_daemon(4343, "unexpected-replacement"))
            },
        )
        .expect("attach stale live daemon");

        assert_eq!(attached, daemon);
        assert_eq!(state.lock().expect("state").starts, 0);
    });
}

#[test]
fn ensure_running_concurrent_callers_converge_on_same_daemon() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-shared");
        let state = Arc::new(Mutex::new(FakeEnsureState::default()));
        let barrier = Arc::new(Barrier::new(3));
        let first =
            ensure_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon.clone());
        let second = ensure_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon);
        barrier.wait();

        let first = first.join().expect("first thread").expect("first ensure");
        let second = second
            .join()
            .expect("second thread")
            .expect("second ensure");
        assert_eq!(first.pid, second.pid);
        assert_eq!(first.lease_id, second.lease_id);
        assert_eq!(first.address, second.address);
        assert_eq!(state.lock().expect("state").starts, 1);
    });
}

#[test]
fn startup_observation_requires_a_live_attempt_token() {
    let expected = "attempt-token";
    let wrong = fake_status(Some(fake_daemon(4242, "other-lease")), true);
    let result = observe_startup_lease(4343, expected, 0, || Ok(wrong.clone()), || {})
        .expect("observe startup");
    let observed = result.expect_err("wrong daemon must not be adopted");
    assert_eq!(observed.observed_pid, Some(4242));
    assert_eq!(observed.observed_lease_id.as_deref(), Some("other-lease"));

    let mut stale = fake_status(Some(fake_daemon(4242, "stale-lease")), true);
    stale.state.as_mut().expect("state").startup_token = expected.to_string();
    let result = observe_startup_lease(4343, expected, 0, || Ok(stale.clone()), || {})
        .expect("observe startup");
    assert!(
        result.is_ok(),
        "the matching token identifies the serve child of this launcher"
    );

    let mut dead = fake_status(Some(fake_daemon(4242, "dead-lease")), false);
    dead.state.as_mut().expect("state").startup_token = expected.to_string();
    let result = observe_startup_lease(4343, expected, 0, || Ok(dead.clone()), || {})
        .expect("observe startup");
    assert!(result.is_err(), "a stale matching token is not ready");
}

#[test]
fn startup_observation_waits_for_delayed_token_without_provider_work() {
    let expected = "attempt-token";
    let missing = fake_status(None, false);
    let mut ready = fake_status(Some(fake_daemon(4343, "lease-new")), true);
    ready.state.as_mut().expect("state").startup_token = expected.to_string();
    let states = Arc::new(Mutex::new(vec![missing, ready]));
    let read_states = Arc::clone(&states);
    let result = observe_startup_lease(
        4343,
        expected,
        1,
        move || Ok(read_states.lock().expect("states").remove(0)),
        || {},
    )
    .expect("observe delayed startup")
    .expect("matching delayed token is ready");
    assert_eq!(result.lease_id, "lease-new");
}

#[test]
fn startup_cleanup_bounds_signals_and_revalidates_ownership_before_sigkill() {
    let pid = 4343;
    let token = "attempt-token";

    let mut cooperative_signals = Vec::new();
    let signal = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(true),
        |_, signal| {
            cooperative_signals.push(signal);
            Ok(())
        },
        |_, _| true,
    )
    .expect("cooperative cleanup");
    assert_eq!(signal, "SIGTERM");
    assert_eq!(cooperative_signals, vec![SIGNAL_TERMINATE]);

    let mut escalation_ownership = VecDeque::from([true, true]);
    let mut escalation_waits = VecDeque::from([false, true]);
    let mut escalation_signals = Vec::new();
    let signal = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(escalation_ownership.pop_front().expect("ownership probe")),
        |_, signal| {
            escalation_signals.push(signal);
            Ok(())
        },
        |_, _| escalation_waits.pop_front().expect("exit probe"),
    )
    .expect("escalated cleanup");
    assert_eq!(signal, "SIGKILL");
    assert_eq!(escalation_signals, vec![SIGNAL_TERMINATE, SIGNAL_KILL]);

    let mut lost_ownership = VecDeque::from([true, false]);
    let mut lost_signals = Vec::new();
    let error = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(lost_ownership.pop_front().expect("ownership probe")),
        |_, signal| {
            lost_signals.push(signal);
            Ok(())
        },
        |_, _| false,
    )
    .expect_err("ownership loss must refuse SIGKILL");
    assert!(error
        .to_string()
        .contains("lost daemon startup-token ownership"));
    assert_eq!(lost_signals, vec![SIGNAL_TERMINATE]);

    let mut survival_waits = VecDeque::from([false, false]);
    let error = terminate_token_owned_startup_process_with_operations(
        pid,
        token,
        Duration::from_millis(1),
        |_, _| Ok(true),
        |_, _| Ok(()),
        |_, _| survival_waits.pop_front().expect("exit probe"),
    )
    .expect_err("SIGKILL survival must fail");
    assert!(error
        .to_string()
        .contains("survived bounded SIGTERM-to-SIGKILL escalation"));
}

#[test]
fn startup_recovery_reuses_only_the_authoritative_attempt_identity() {
    let expected = "attempt-token";
    let absent = super::StartupLeaseObservation {
        observed_pid: None,
        observed_lease_id: None,
        observed_token: None,
    };
    assert!(can_recover_startup_attempt(true, expected, &absent, &[]));

    let stale = super::StartupLeaseObservation {
        observed_pid: Some(4242),
        observed_lease_id: Some("stale-lease".to_string()),
        observed_token: Some(expected.to_string()),
    };
    assert!(can_recover_startup_attempt(true, expected, &stale, &[]));

    let competing = super::StartupLeaseObservation {
        observed_pid: Some(4343),
        observed_lease_id: Some("other-lease".to_string()),
        observed_token: Some("other-token".to_string()),
    };
    assert!(!can_recover_startup_attempt(
        true,
        expected,
        &competing,
        &[]
    ));
    assert!(!can_recover_startup_attempt(
        true,
        expected,
        &stale,
        &[
            "retained lease for pid 4242 because its token ownership could not be proven"
                .to_string()
        ],
    ));
}

#[test]
fn locked_start_operations_publish_once_and_converge_concurrent_callers() {
    with_isolated_home(|_| {
        let daemon = fake_daemon(4242, "lease-published");
        let state = Arc::new(Mutex::new(FakeEnsureState::default()));
        let barrier = Arc::new(Barrier::new(3));
        let first = locked_start_with_fake_operations(
            Arc::clone(&barrier),
            Arc::clone(&state),
            daemon.clone(),
        );
        let second =
            locked_start_with_fake_operations(Arc::clone(&barrier), Arc::clone(&state), daemon);
        barrier.wait();
        let first = first.join().expect("first start").expect("first result");
        let second = second.join().expect("second start").expect("second result");
        assert_eq!(first.pid, second.pid);
        assert_eq!(first.lease_id, second.lease_id);
        assert_eq!(state.lock().expect("state").starts, 1);
    });
}

#[test]
fn state_loss_recovery_requires_a_concrete_unreachable_recorded_endpoint() {
    for endpoint in ["127.0.0.1:0", "0.0.0.0:7421", "not-an-endpoint"] {
        assert!(super::parse_recorded_daemon_endpoint(endpoint).is_err());
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let endpoint = listener.local_addr().expect("endpoint");
    let error = super::probe_recorded_daemon_endpoint(endpoint)
        .expect_err("reachable recorded endpoint must block recovery");
    assert!(error.message.contains("is reachable"));
}

fn ensure_with_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::error::Result<super::DaemonStartResult>> {
    std::thread::spawn(move || {
        barrier.wait();
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);
        let daemon_pid = daemon.pid;
        ensure_running_with_operations(
            Duration::from_secs(1),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    true,
                ))
            },
            move |pid| pid == daemon_pid,
            move || {
                let mut state = start_state.lock().expect("state");
                state.starts += 1;
                state.daemon = Some(daemon.clone());
                Ok(daemon)
            },
        )
    })
}

fn locked_start_with_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::error::Result<super::DaemonStartResult>> {
    std::thread::spawn(move || {
        barrier.wait();
        let read_state = Arc::clone(&state);
        let start_state = Arc::clone(&state);
        let daemon_pid = daemon.pid;
        super::ensure_running_with_operations(
            Duration::from_secs(1),
            super::super::acquire_daemon_operation_lock_for_ensure,
            move || {
                Ok(fake_status(
                    read_state.lock().expect("state").daemon.clone(),
                    false,
                ))
            },
            move |pid| pid == daemon_pid,
            move || {
                let publish_state = Arc::clone(&start_state);
                super::start_or_return_live_with_operations(
                    || {
                        Ok(fake_status(
                            start_state.lock().expect("state").daemon.clone(),
                            false,
                        ))
                    },
                    || Ok(Some(())),
                    || Ok(()),
                    move || {
                        let mut state = publish_state.lock().expect("state");
                        state.starts += 1;
                        state.daemon = Some(daemon.clone());
                        Ok(daemon)
                    },
                )
            },
        )
    })
}

fn fake_daemon(pid: u32, lease_id: &str) -> super::DaemonStartResult {
    super::DaemonStartResult {
        pid,
        address: "127.0.0.1:49152".to_string(),
        state_path: "/fake/daemon-state.json".to_string(),
        lease_id: lease_id.to_string(),
    }
}

fn legacy_recovery_fixture(lease_id: &str) -> (JobStore, crate::api_jobs::Job, String) {
    let path = crate::paths::daemon_jobs_file().expect("jobs path");
    let store = JobStore::open_without_reconciliation(&path)
        .expect("store")
        .with_daemon_lease(lease_id.to_string());
    let job = store.create("runner.exec");
    store.start(job.id).expect("legacy job starts");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve endpoint");
    let endpoint = listener.local_addr().expect("endpoint").to_string();
    drop(listener);
    write_legacy_recovery_state(lease_id, u32::MAX, &endpoint);
    (store, job, endpoint)
}

fn write_legacy_recovery_state(lease_id: &str, pid: u32, endpoint: &str) {
    let state_path = crate::paths::daemon_state_file().expect("state path");
    let mut state = fake_daemon_state(super::DaemonStartResult {
        pid,
        address: endpoint.to_string(),
        state_path: state_path.display().to_string(),
        lease_id: lease_id.to_string(),
    });
    state.state_path = state_path.display().to_string();
    std::fs::create_dir_all(state_path.parent().expect("state parent")).expect("state parent");
    std::fs::write(state_path, serde_json::to_vec(&state).expect("state json")).expect("state");
}

fn fake_status(daemon: Option<super::DaemonStartResult>, fresh: bool) -> DaemonStatus {
    let stale_reason_code = (!fresh).then_some(DaemonStaleReasonCode::VersionMismatch);
    DaemonStatus {
        running: fresh,
        fresh,
        reachable: true,
        freshness: DaemonFreshnessReport {
            fresh,
            stale_reason_code,
            restartable: !fresh,
            lease_id: daemon.as_ref().map(|daemon| daemon.lease_id.clone()),
            pid: daemon.as_ref().map(|daemon| daemon.pid),
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
        },
        stale_reason: (!fresh).then(|| "simulated stale daemon".to_string()),
        state: daemon.map(fake_daemon_state),
        state_path: "/fake/daemon-state.json".to_string(),
        state_identity: "sha256:fake".to_string(),
        process_candidates: Vec::new(),
        active_job_recovery_evidence: Vec::new(),
        termination_evidence: None,
    }
}

fn fake_dead_status(daemon: super::DaemonStartResult) -> DaemonStatus {
    DaemonStatus {
        running: false,
        fresh: false,
        reachable: false,
        freshness: DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(DaemonStaleReasonCode::PidDead),
            restartable: false,
            lease_id: Some(daemon.lease_id.clone()),
            pid: Some(daemon.pid),
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 1,
            termination_evidence: None,
            repair_plan: Vec::new(),
        },
        stale_reason: Some("daemon lease pid is not running".to_string()),
        state: Some(fake_daemon_state(daemon)),
        state_path: "/fake/daemon-state.json".to_string(),
        state_identity: "sha256:fake".to_string(),
        process_candidates: Vec::new(),
        active_job_recovery_evidence: Vec::new(),
        termination_evidence: None,
    }
}

fn dead_status_with_unexpected_termination(daemon: super::DaemonStartResult) -> DaemonStatus {
    let mut status = fake_dead_status(daemon.clone());
    status.termination_evidence = Some(DaemonTerminationEvidence {
        classification: DaemonTerminationClassification::UnexpectedExit,
        observed_at: "2026-01-01T00:00:00Z".to_string(),
        lease_id: Some(daemon.lease_id),
        pid: Some(daemon.pid),
        binary_identity: None,
        active_jobs: 1,
        resource_evidence: "test".to_string(),
        os_evidence: "test".to_string(),
        exit_code: None,
        signal: Some(libc::SIGTERM),
        supervisor_signal: None,
        stdout: None,
        stderr: None,
        stop_requested: false,
    });
    status
}

fn fake_daemon_state(daemon: super::DaemonStartResult) -> DaemonState {
    DaemonState {
        schema: "homeboy.daemon.session_lease.v1".to_string(),
        lease_id: daemon.lease_id,
        startup_token: "fake-startup-token".to_string(),
        address: daemon.address,
        pid: daemon.pid,
        state_path: daemon.state_path,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        last_seen_at: "2026-01-01T00:00:00Z".to_string(),
        build_identity: BuildIdentity {
            version: "test".to_string(),
            git_commit: None,
            git_dirty: None,
            display: "homeboy test".to_string(),
        },
        binary_sha256: None,
        runtime_paths: DaemonRuntimeSnapshot {
            loaded_at: "2026-01-01T00:00:00Z".to_string(),
            paths: Vec::new(),
        },
    }
}
