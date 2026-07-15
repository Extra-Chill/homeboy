use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::time::Duration;
#[cfg(unix)]
use std::{os::unix::process::CommandExt, process::Command};

use super::{
    artifact_content_url, ensure_running_with_operations, fetch_artifact_to_path,
    reconcile_dead_lease_and_ensure_running_with_operations,
    reconcile_leaseless_orphan_store_with_operations,
};
use crate::core::api_jobs::{JobEventKind, JobStatus, JobStore};
use crate::core::build_identity::BuildIdentity;
use crate::core::daemon::{
    DaemonFreshnessReport, DaemonRuntimeSnapshot, DaemonStaleReasonCode, DaemonState, DaemonStatus,
};
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

fn record_test_local_child(store: &JobStore, job_id: uuid::Uuid, pid: u32) {
    store
        .reserve_local_child(job_id)
        .expect("reserve local child");
    store
        .start_with_reserved_child_identity(
            job_id,
            pid,
            None,
            crate::core::api_jobs::LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks {
                ticks: 1,
            },
        )
        .expect("record local child identity");
}

fn record_test_absent_local_child(store: &JobStore, job_id: uuid::Uuid) {
    store
        .reserve_local_child(job_id)
        .expect("reserve local child");
    store
        .start_with_reserved_child_identity(
            job_id,
            u32::MAX,
            None,
            crate::core::api_jobs::LocalChildStartDiscriminator::Unsupported {
                evidence: "test process identity unavailable".to_string(),
            },
        )
        .expect("record absent local child identity");
}

#[test]
fn leaseless_store_requires_missing_lease_and_no_owner_then_preserves_evidence() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path).expect("store");
        let job = store.create("runner.exec");
        record_test_absent_local_child(&store, job.id);
        store
            .append_event(
                job.id,
                JobEventKind::Stdout,
                Some("retained output".to_string()),
                None,
            )
            .expect("output");
        let replacement = fake_daemon(4343, "fresh-lease");
        let result = reconcile_leaseless_orphan_store_with_operations(
            || Ok(leaseless_status(1)),
            || Ok(vec!["no process".to_string(), "no listener".to_string()]),
            || {
                let snapshot = path.with_extension("snapshot.json");
                std::fs::copy(&path, &snapshot).expect("snapshot");
                let store = JobStore::open_without_reconciliation(&path).expect("recovery store");
                Ok((snapshot, store.reconcile_leaseless_orphan_jobs()?))
            },
            || Ok(replacement.clone()),
        )
        .expect("reconcile");
        assert_eq!(result.affected_job_ids, vec![job.id.to_string()]);
        assert_eq!(result.no_owner_proof.len(), 2);
        assert!(std::path::Path::new(&result.snapshot_path).exists());
        let recovered = JobStore::open_without_reconciliation(&path).expect("recovered");
        assert_eq!(
            recovered.get(job.id).expect("job").status,
            JobStatus::Failed
        );
        assert!(recovered
            .events(job.id)
            .expect("events")
            .iter()
            .any(|event| event.message.as_deref() == Some("retained output")));
    });
}

#[test]
fn corrupt_lease_reconciliation_terminalizes_queued_durable_agent_task_after_proof() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path).expect("store");
        let job = store.create("agent-task.cook");

        let result = reconcile_leaseless_orphan_store_with_operations(
            || Ok(corrupt_leaseless_status(1)),
            || {
                Ok(vec![
                    "owner lock acquired".to_string(),
                    "no process".to_string(),
                ])
            },
            || {
                let snapshot = path.with_extension("corrupt-lease.snapshot.json");
                std::fs::copy(&path, &snapshot).expect("snapshot");
                let store = JobStore::open_without_reconciliation(&path).expect("recovery store");
                Ok((snapshot, store.reconcile_leaseless_orphan_jobs()?))
            },
            || Ok(fake_daemon(4343, "fresh-lease")),
        )
        .expect("reconcile corrupt lease");

        assert_eq!(result.affected_job_ids, vec![job.id.to_string()]);
        let recovered = JobStore::open_without_reconciliation(&path).expect("recovered");
        assert_eq!(
            recovered.get(job.id).expect("job").status,
            JobStatus::Failed
        );
        assert!(recovered
            .events(job.id)
            .expect("events")
            .iter()
            .any(|event| event
                .data
                .as_ref()
                .is_some_and(|data| { data["reason"] == "dead_daemon_lease" })));
    });
}

#[test]
fn version_mismatched_split_view_reconciles_only_after_no_owner_proof() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path).expect("store");
        let job = store.create("runner.exec");
        record_test_absent_local_child(&store, job.id);
        let mut status = leaseless_status(1);
        status.freshness.stale_reason_code = Some(DaemonStaleReasonCode::VersionMismatch);

        let result = reconcile_leaseless_orphan_store_with_operations(
            || Ok(status),
            || {
                Ok(vec![
                    "owner lock acquired".to_string(),
                    "no daemon process".to_string(),
                ])
            },
            || {
                let snapshot = path.with_extension("split-view.snapshot.json");
                std::fs::copy(&path, &snapshot).expect("snapshot");
                let store = JobStore::open_without_reconciliation(&path).expect("recovery store");
                Ok((snapshot, store.reconcile_leaseless_orphan_jobs()?))
            },
            || Ok(fake_daemon(4343, "replacement-lease")),
        )
        .expect("version-mismatched split view is recoverable after proof");

        assert_eq!(result.affected_job_ids, vec![job.id.to_string()]);
        assert_eq!(
            JobStore::open_without_reconciliation(&path)
                .expect("reopen store")
                .get(job.id)
                .expect("job")
                .status,
            JobStatus::Failed
        );
        let replay = JobStore::open_without_reconciliation(&path)
            .expect("reopen terminal store")
            .reconcile_leaseless_orphan_jobs()
            .expect("identical version-mismatch reconciliation is idempotent");
        assert_eq!(replay.reconciled_count(), 0);
    });
}

#[test]
fn version_mismatched_split_view_refuses_preserved_remote_claim_before_replacement() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("store")
            .with_daemon_lease("stale-lease".to_string());
        let remote_job = store
            .submit_remote_runner_job(
                serde_json::from_value(serde_json::json!({
                    "runner_id": "remote-runner",
                    "command": ["true"],
                }))
                .expect("remote request"),
            )
            .expect("queue remote job");
        let mut status = leaseless_status(1);
        status.freshness.stale_reason_code = Some(DaemonStaleReasonCode::VersionMismatch);

        let error = reconcile_leaseless_orphan_store_with_operations(
            || Ok(status),
            || {
                Ok(vec![
                    "owner lock acquired".to_string(),
                    "no daemon process".to_string(),
                ])
            },
            || {
                let snapshot = path.with_extension("remote-claim.snapshot.json");
                std::fs::copy(&path, &snapshot).expect("snapshot");
                let store = JobStore::open_without_reconciliation(&path).expect("recovery store");
                Ok((snapshot, store.reconcile_leaseless_orphan_jobs()?))
            },
            || unreachable!("preserved remote claim must block replacement"),
        )
        .expect_err("active broker-owned claim blocks version-mismatch recovery");

        assert!(error.message.contains("broker-owned remote job"));
        assert!(error.message.contains(&remote_job.id.to_string()));
        assert_eq!(
            store.get(remote_job.id).expect("remote job").status,
            JobStatus::Queued
        );
    });
}

#[test]
fn leaseless_store_aborts_on_ambiguous_or_live_owner_probe() {
    for probe in [
        || {
            Err(crate::core::Error::internal_unexpected(
                "daemon listener probe was ambiguous",
            ))
        },
        || {
            Err(crate::core::Error::validation_invalid_argument(
                "owner_probe",
                "a Homeboy daemon listener is live",
                None,
                None,
            ))
        },
    ] {
        let error = reconcile_leaseless_orphan_store_with_operations(
            || Ok(leaseless_status(1)),
            probe,
            || unreachable!("must not reconcile"),
            || unreachable!("must not start"),
        )
        .expect_err("probe must fail closed");
        assert!(error.message.contains("probe") || error.message.contains("listener"));
    }
}

#[test]
fn daemon_process_attribution_matches_only_explicit_store_and_binary_identity() {
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
fn daemon_process_attribution_proves_unrelated_and_fails_closed_when_ambiguous() {
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
            .expect("ambiguous")
            .ownership,
        super::super::DaemonProcessOwnership::Ambiguous
    );
}

#[test]
fn multiple_ambiguous_candidates_block_recovery_and_pid_reuse_blocks_adoption() {
    let ambiguous = super::super::DaemonProcessCandidate {
        pid: 71,
        executable: "/tmp/homeboy".to_string(),
        cmdline: "homeboy daemon serve --addr 127.0.0.1:0".to_string(),
        bind_endpoint: Some("127.0.0.1:0".to_string()),
        durable_store_path: None,
        build_identity: None,
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
fn concurrent_leaseless_recovery_callers_commit_once_and_preserve_job_evidence() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path).expect("store");
        let job = store.create("runner.exec");
        record_test_absent_local_child(&store, job.id);
        store
            .append_event(
                job.id,
                JobEventKind::Stdout,
                Some("output before control-plane loss".to_string()),
                None,
            )
            .expect("output");

        let lifecycle = Arc::new(Mutex::new(false));
        let starts = Arc::new(Mutex::new(0_usize));
        let barrier = Arc::new(Barrier::new(3));
        let first = concurrent_leaseless_recovery(
            Arc::clone(&barrier),
            Arc::clone(&lifecycle),
            Arc::clone(&starts),
            path.clone(),
        );
        let second = concurrent_leaseless_recovery(
            Arc::clone(&barrier),
            Arc::clone(&lifecycle),
            Arc::clone(&starts),
            path.clone(),
        );
        barrier.wait();

        let first = first.join().expect("first caller");
        let second = second.join().expect("second caller");
        assert_eq!(
            [first, second].into_iter().filter(Result::is_ok).count(),
            1,
            "only one caller may commit the recovery transaction"
        );
        assert_eq!(*starts.lock().expect("starts"), 1);

        let recovered = JobStore::open_without_reconciliation(&path).expect("recovered store");
        let events = recovered.events(job.id).expect("events");
        assert_eq!(
            recovered.get(job.id).expect("job").status,
            JobStatus::Failed
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event
                        .data
                        .as_ref()
                        .is_some_and(|data| data["reason"] == "dead_daemon_lease")
                })
                .count(),
            2,
            "one error and one status event are appended exactly once"
        );
        assert!(events
            .iter()
            .any(|event| { event.message.as_deref() == Some("output before control-plane loss") }));
    });
}

fn concurrent_leaseless_recovery(
    barrier: Arc<Barrier>,
    lifecycle: Arc<Mutex<bool>>,
    starts: Arc<Mutex<usize>>,
    path: std::path::PathBuf,
) -> std::thread::JoinHandle<
    crate::core::error::Result<super::DaemonLeaselessOrphanReconciliationResult>,
> {
    std::thread::spawn(move || {
        barrier.wait();
        let mut committed = lifecycle.lock().expect("lifecycle lock");
        if *committed {
            return Err(crate::core::Error::validation_invalid_argument(
                "lifecycle",
                "replacement daemon already committed by concurrent recovery",
                None,
                None,
            ));
        }
        let result = reconcile_leaseless_orphan_store_with_operations(
            || Ok(leaseless_status(1)),
            || Ok(vec!["no process".to_string(), "no listener".to_string()]),
            || {
                let snapshot = path.with_extension(format!("{}.snapshot", uuid::Uuid::new_v4()));
                std::fs::copy(&path, &snapshot).expect("snapshot");
                let store = JobStore::open_without_reconciliation(&path).expect("recovery store");
                Ok((snapshot, store.reconcile_leaseless_orphan_jobs()?))
            },
            || {
                let mut starts = starts.lock().expect("starts");
                *starts += 1;
                Ok(fake_daemon(5000 + *starts as u32, "replacement-lease"))
            },
        );
        if result.is_ok() {
            *committed = true;
        }
        result
    })
}

fn leaseless_status(active_jobs: usize) -> DaemonStatus {
    DaemonStatus {
        running: false,
        fresh: false,
        reachable: false,
        freshness: DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(DaemonStaleReasonCode::LeaseMissing),
            restartable: false,
            lease_id: None,
            pid: None,
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs,
            termination_evidence: None,
            repair_plan: Vec::new(),
        },
        stale_reason: None,
        state: None,
        state_path: "/fake/daemon-state.json".to_string(),
        state_identity: "lease-missing-test-state".to_string(),
        process_candidates: Vec::new(),
        active_job_recovery_evidence: Vec::new(),
        termination_evidence: None,
    }
}

fn corrupt_leaseless_status(active_jobs: usize) -> DaemonStatus {
    let mut status = leaseless_status(active_jobs);
    status.freshness.stale_reason_code = Some(DaemonStaleReasonCode::LeaseCorrupt);
    status.stale_reason = Some("invalid daemon lease".to_string());
    status
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
        let state_path = crate::core::paths::daemon_state_file().expect("state path");
        let mut state = fake_daemon_state(fake_daemon(999_999, "lease-dead"));
        state.state_path = state_path.display().to_string();
        std::fs::create_dir_all(state_path.parent().expect("state parent")).expect("state parent");
        std::fs::write(&state_path, serde_json::to_vec(&state).expect("state json"))
            .expect("state");

        let jobs_path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&jobs_path)
            .expect("store")
            .with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("start job");
        let before = std::fs::read(&jobs_path).expect("store bytes");
        let owner = super::super::try_acquire_daemon_owner_lock()
            .expect("owner lock")
            .expect("owner acquired");

        let error = super::adopt_orphaned_lease("lease-dead", true, "127.0.0.1:0")
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
        let before = std::fs::read(crate::core::paths::daemon_jobs_file().expect("jobs path"))
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
            std::fs::read(crate::core::paths::daemon_jobs_file().expect("jobs path"))
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
        let jobs_path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let before = std::fs::read(&jobs_path).expect("store bytes");
        std::fs::remove_file(crate::core::paths::daemon_state_file().expect("state path"))
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

#[cfg(unix)]
#[test]
fn leaseless_recovery_blocks_a_surviving_recorded_process_group() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let marker_dir = tempfile::tempdir().expect("marker dir");
        let marker = marker_dir.path().join("descendant.pid");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("durable store")
            .with_daemon_lease("lease-dead".to_string());
        let (root_reaped_tx, root_reaped_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let marker_for_child = marker.clone();
        let runner = store.run_local_child_background_with_source_snapshot_metadata_and_path_materialization_plan(
            "runner.exec".to_string(),
            None,
            None,
            None,
            move |job| {
                let mut command = Command::new("sh");
                command.args([
                    "-c",
                    &format!(
                        "sleep 30 & echo $! > {}",
                        crate::core::engine::shell::quote_arg(marker_for_child.to_str().expect("marker"))
                    ),
                ]);
                unsafe {
                    command.pre_exec(|| {
                        if libc::setpgid(0, 0) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let mut child = command.spawn().expect("spawn root");
                let pid = child.id();
                let pgid = crate::core::process::isolated_process_group_id(pid)
                    .expect("read process group")
                    .expect("Unix group identity");
                let discriminator = match crate::core::process::linux_process_starttime_ticks(pid) {
                    Ok(Some(ticks)) => crate::core::api_jobs::LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks { ticks },
                    Ok(None) => panic!("root exited before identity persisted"),
                    Err(evidence) => crate::core::api_jobs::LocalChildStartDiscriminator::Unsupported { evidence },
                };
                job.start_with_reserved_child_identity(pid, Some(pgid), discriminator)
                    .expect("persist root and group identity");
                child.wait().expect("reap root");
                root_reaped_tx.send(pgid).expect("report root exit");
                release_rx.recv().expect("release worker");
                Ok(serde_json::json!({ "exit_code": 0 }))
            },
        );
        let pgid = root_reaped_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("root exits while descendant remains");
        let descendant: u32 = std::fs::read_to_string(&marker)
            .expect("descendant marker")
            .trim()
            .parse()
            .expect("descendant pid");
        assert!(crate::core::process::pid_is_running(descendant));

        let blocked = JobStore::open_without_reconciliation(&path)
            .expect("reopen store")
            .reconcile_leaseless_orphan_jobs()
            .expect("group liveness is a protected disposition");
        assert_eq!(blocked.protected_job_ids, vec![runner.job_id]);
        assert_eq!(
            store.get(runner.job_id).expect("job").status,
            JobStatus::Running
        );

        crate::core::process::terminate_isolated_process_group(pgid)
            .expect("terminate recorded group");
        std::thread::sleep(Duration::from_millis(150));
        let recovered = JobStore::open_without_reconciliation(&path)
            .expect("reopen store")
            .reconcile_leaseless_orphan_jobs()
            .expect("absent root and group terminalize once");
        assert_eq!(recovered.reconciled_count(), 1);
        let replay = JobStore::open_without_reconciliation(&path)
            .expect("reopen store")
            .reconcile_leaseless_orphan_jobs()
            .expect("replay remains idempotent");
        assert_eq!(replay.reconciled_count(), 0);
        release_tx.send(()).expect("release worker");
        runner.handle.join().expect("worker");
    });
}

#[cfg(unix)]
#[test]
fn local_child_reservation_recovery_requires_death_before_one_replacement() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("durable store")
            .with_daemon_lease("lease-dead".to_string());
        let spawn_gate = Arc::new(Barrier::new(2));
        let release_spawn = Arc::clone(&spawn_gate);
        let (started_tx, started_rx) = mpsc::channel();
        let (reaped_tx, reaped_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let runner = store
            .run_local_child_background_with_source_snapshot_metadata_and_path_materialization_plan(
                "runner.exec".to_string(),
                None,
                None,
                None,
                move |job| {
                    release_spawn.wait();
                    let mut child_command = Command::new("sh");
                    child_command.args(["-c", "trap '' TERM; sleep 30"]);
                    unsafe {
                        child_command.pre_exec(|| {
                            if libc::setpgid(0, 0) != 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                    let mut child = child_command.spawn().expect("spawn child");
                    let pid = child.id();
                    let discriminator = match crate::core::process::linux_process_starttime_ticks(pid) {
                        Ok(Some(ticks)) => crate::core::api_jobs::LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks { ticks },
                        Ok(None) => panic!("child exited before identity persisted"),
                        Err(evidence) => crate::core::api_jobs::LocalChildStartDiscriminator::Unsupported { evidence },
                    };
                    job.start_with_reserved_child_identity(pid, None, discriminator)
                        .expect("persist child identity before running");
                    started_tx.send(pid).expect("report child");
                    child.wait().expect("reap child");
                    reaped_tx.send(()).expect("report reaped");
                    release_rx.recv().expect("allow worker completion");
                    Ok(serde_json::json!({ "exit_code": 0 }))
                },
            );

        let queued = JobStore::open_without_reconciliation(&path).expect("reopen queued store");
        assert_eq!(
            queued.get(runner.job_id).expect("queued job").status,
            JobStatus::Queued
        );
        assert!(queued
            .events(runner.job_id)
            .expect("events")
            .iter()
            .any(|event| event
                .data
                .as_ref()
                .is_some_and(|data| data["phase"] == "child_reserved")));

        spawn_gate.wait();
        let pid = started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("child starts");
        let running = JobStore::open_without_reconciliation(&path).expect("reopen running store");
        assert_eq!(
            running.get(runner.job_id).expect("running job").status,
            JobStatus::Running
        );
        #[cfg(target_os = "linux")]
        assert!(running
            .events(runner.job_id)
            .expect("events")
            .iter()
            .any(|event| event
                .data
                .as_ref()
                .is_some_and(|data| data["process"]["root_pid"] == pid
                    && data["process"]["start_discriminator"]["kind"]
                        == "linux_proc_stat_starttime_ticks")));

        let blocked = super::adopt_orphaned_lease_with_operations(
            "lease-dead",
            || Ok(fake_dead_status(fake_daemon(u32::MAX, "lease-dead"))),
            |_| false,
            || Ok(Some(())),
            || {
                JobStore::open_without_reconciliation(&path)?
                    .reconcile_dead_daemon_lease_jobs("lease-dead")
            },
            || unreachable!("live child must block replacement"),
        )
        .expect_err("live local child blocks replacement");
        assert!(blocked.message.contains("active child process"));
        assert!(crate::core::process::pid_is_running(pid));

        crate::core::process::terminate_isolated_process_group(pid)
            .expect("terminate process group");
        reaped_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("child reaped");
        let starts = Arc::new(Mutex::new(0_usize));
        let start_count = Arc::clone(&starts);
        let recovered = super::adopt_orphaned_lease_with_operations(
            "lease-dead",
            || Ok(fake_dead_status(fake_daemon(u32::MAX, "lease-dead"))),
            |_| false,
            || Ok(Some(())),
            || {
                JobStore::open_without_reconciliation(&path)?
                    .reconcile_dead_daemon_lease_jobs("lease-dead")
            },
            move || {
                *start_count.lock().expect("starts") += 1;
                Ok(fake_daemon(4343, "replacement"))
            },
        )
        .expect("dead child permits exactly one replacement");
        assert_eq!(recovered.active_jobs_terminalized, 1);
        assert_eq!(*starts.lock().expect("starts"), 1);
        let replay = JobStore::open_without_reconciliation(&path)
            .expect("reopen terminal store")
            .reconcile_dead_daemon_lease_jobs("lease-dead")
            .expect("identical recovery replay is idempotent");
        assert_eq!(replay.matching_count(), 0);
        assert_eq!(*starts.lock().expect("starts"), 1);
        release_tx.send(()).expect("release worker");
        runner.handle.join().expect("worker thread");
    });
}

#[cfg(target_os = "linux")]
#[test]
fn legacy_child_recovery_exact_evidence_is_idempotent_and_conflicts_fail_closed() {
    with_isolated_home(|_| {
        let (store, job, endpoint) = legacy_recovery_fixture("lease-dead");
        let recovered = super::recover_missing_child_identity(
            "lease-dead",
            u32::MAX,
            &endpoint,
            job.id,
            u32::MAX,
            1,
        )
        .expect("absent child identity recovers exactly one job");
        let events = store.events(job.id).expect("events");
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
        assert_eq!(store.events(job.id).expect("events").len(), events.len());

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
fn exact_lease_adoption_reconciles_terminal_children_idempotently() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("store")
            .with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("start job");
        store
            .append_event(
                job.id,
                JobEventKind::Result,
                None,
                Some(serde_json::json!({ "exit_code": 0, "output": "retained" })),
            )
            .expect("record terminal result");

        let adopt = || {
            super::adopt_orphaned_lease_with_operations(
                "lease-dead",
                || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
                |_| false,
                || Ok(Some(())),
                || {
                    JobStore::open_without_reconciliation(&path)?
                        .reconcile_dead_daemon_lease_jobs("lease-dead")
                },
                || Ok(fake_daemon(4343, "lease-replacement")),
            )
        };

        let first = adopt().expect("terminal child permits adoption");
        let recovered = JobStore::open_without_reconciliation(&path).expect("reopen store");
        let event_count = recovered.events(job.id).expect("events").len();
        let second = adopt().expect("repeat adoption is a no-op for terminal child");
        let recovered = JobStore::open_without_reconciliation(&path).expect("reopen store");

        assert_eq!(first.active_jobs_terminalized, 1);
        assert_eq!(second.active_jobs_terminalized, 0);
        assert_eq!(
            recovered.get(job.id).expect("job").status,
            JobStatus::Succeeded
        );
        assert_eq!(recovered.events(job.id).expect("events").len(), event_count);
    });
}

#[test]
fn exact_adoption_keeps_legacy_missing_identity_blocked() {
    let legacy = JobStore::default().with_daemon_lease("lease-dead".to_string());
    let job = legacy.create("runner.exec");
    legacy.start(job.id).expect("start legacy job");

    let normal = super::adopt_orphaned_lease_with_operations(
        "lease-dead",
        || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
        |_| false,
        || Ok(Some(())),
        || legacy.reconcile_dead_daemon_lease_jobs("lease-dead"),
        || Ok(fake_daemon(4343, "replacement")),
    );
    assert!(normal
        .expect_err("normal adoption rejects missing identity")
        .message
        .contains("recorded child PID"));
    assert_eq!(legacy.get(job.id).expect("job").status, JobStatus::Running);

    let blocked = super::adopt_orphaned_lease_with_operations(
        "lease-dead",
        || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
        |_| false,
        || Ok(Some(())),
        || legacy.reconcile_dead_daemon_lease_jobs("lease-dead"),
        || unreachable!("missing child identity must block replacement"),
    );
    assert!(blocked
        .expect_err("legacy identity remains blocked")
        .message
        .contains("no authoritative terminal result"));
    assert_eq!(legacy.get(job.id).expect("job").status, JobStatus::Running);
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
#[allow(unreachable_code, unused_variables)]
fn dead_matching_lease_reconciles_jobs_before_starting_replacement() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("daemon jobs path");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("create durable store")
            .with_daemon_lease("lease-dead".to_string());
        let job = store.create("runner.exec");
        record_test_local_child(&store, job.id, u32::MAX);
        let daemon = fake_daemon(4343, "lease-replacement");

        #[cfg(not(target_os = "linux"))]
        {
            let blocked = super::reconcile_dead_lease_and_ensure_running_with_operations(
                Duration::from_millis(50),
                super::super::acquire_daemon_operation_lock_for_ensure,
                "lease-dead",
                || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
                |_| false,
                || super::reconcile_dead_daemon_lease_jobs("lease-dead"),
                || unreachable!("unsupported identity must block replacement"),
            )
            .expect_err("unsupported process identity blocks recovery");
            assert!(blocked.message.contains("active child process"));
            return;
        }

        let started = reconcile_dead_lease_and_ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            "lease-dead",
            || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
            |_| false,
            || super::reconcile_dead_daemon_lease_jobs("lease-dead"),
            || {
                let recovered = JobStore::open_without_reconciliation(&path)
                    .expect("read reconciled jobs before start");
                assert_eq!(
                    recovered.get(job.id).expect("reconciled job").status,
                    JobStatus::Failed,
                    "scoped reconciliation completes before replacement startup"
                );
                Ok(daemon.clone())
            },
        )
        .expect("dead lease is reconciled and replaced");

        assert_eq!(started.lease_id, "lease-replacement");
        let reconciled =
            JobStore::open_without_reconciliation(&path).expect("read reconciled durable store");
        let orphaned = reconciled.get(job.id).expect("durable job persists");
        assert_eq!(orphaned.status, JobStatus::Failed);
        assert_eq!(
            orphaned.stale_reason.as_deref(),
            Some("daemon lease owner process was not running")
        );
        let events = reconciled
            .events(job.id)
            .expect("durable evidence persists");
        let classification = events
            .iter()
            .find_map(|event| {
                (event.kind == JobEventKind::Error)
                    .then(|| event.data.as_ref()?.get("classification"))?
            })
            .expect("control-plane-loss classification");
        assert_eq!(classification["kind"], "orphaned_after_control_plane_loss");
    });
}

#[test]
fn legacy_unowned_job_blocks_replacement_start() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("daemon jobs path");
        let store = JobStore::open_without_reconciliation(&path).expect("create durable store");
        let job = store.create("runner.exec");
        store.start(job.id).expect("job starts");

        let error = reconcile_dead_lease_and_ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            "lease-dead",
            || Ok(fake_dead_status(fake_daemon(4242, "lease-dead"))),
            |_| false,
            || super::reconcile_dead_daemon_lease_jobs("lease-dead"),
            || unreachable!("legacy job must prevent replacement startup"),
        )
        .expect_err("legacy job blocks recovery");

        assert!(error.message.contains("legacy unowned active job"));
        assert!(error.message.contains(&job.id.to_string()));
    });
}

#[test]
#[allow(unreachable_code, unused_variables)]
fn state_loss_exact_lease_recovery_terminalizes_only_matching_jobs_and_starts_once() {
    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let store = JobStore::open_without_reconciliation(&path)
            .expect("store")
            .with_daemon_lease("exact-lease".to_string());
        let job = store.create("runner.exec");
        record_test_local_child(&store, job.id, u32::MAX);
        let starts = Arc::new(Mutex::new(0));
        let start_count = Arc::clone(&starts);

        #[cfg(not(target_os = "linux"))]
        {
            let blocked = super::recover_missing_lease_state_with_operations(
                "exact-lease",
                4242,
                recorded_endpoint(),
                || Ok(leaseless_status(1)),
                |_| false,
                |_| Ok("recorded endpoint was unreachable".to_string()),
                || Ok(Some(())),
                || {
                    let raw = std::fs::read(&path).expect("store bytes");
                    let snapshot = super::snapshot_job_store(&path, &raw).expect("snapshot");
                    let recovered = JobStore::open_without_reconciliation_from_bytes(&path, &raw)
                        .expect("recovery store");
                    Ok((
                        snapshot,
                        recovered.reconcile_dead_daemon_lease_jobs("exact-lease")?,
                    ))
                },
                || unreachable!("unsupported identity must block replacement"),
            )
            .expect_err("unsupported process identity blocks recovery");
            assert!(blocked.message.contains("active child process"));
            return;
        }
        let result = super::recover_missing_lease_state_with_operations(
            "exact-lease",
            4242,
            recorded_endpoint(),
            || Ok(leaseless_status(1)),
            |_| false,
            |_| Ok("recorded endpoint was unreachable".to_string()),
            || Ok(Some(())),
            || {
                let raw = std::fs::read(&path).expect("store bytes");
                let snapshot = super::snapshot_job_store(&path, &raw).expect("snapshot");
                let recovered = JobStore::open_without_reconciliation_from_bytes(&path, &raw)
                    .expect("recovery store");
                let diagnostics = recovered.daemon_lease_job_diagnostics("exact-lease");
                assert!(diagnostics.other_lease_job_ids.is_empty());
                Ok((
                    snapshot,
                    recovered.reconcile_dead_daemon_lease_jobs("exact-lease")?,
                ))
            },
            move || {
                *start_count.lock().expect("starts") += 1;
                Ok(fake_daemon(4343, "replacement"))
            },
        )
        .expect("exact recovery");
        assert_eq!(result.affected_job_ids, vec![job.id]);
        assert_eq!(*starts.lock().expect("starts"), 1);
        assert!(std::path::Path::new(&result.evidence_snapshot_path).exists());
        assert_eq!(
            JobStore::open_without_reconciliation(&path)
                .expect("reopen store")
                .get(job.id)
                .expect("job")
                .status,
            JobStatus::Failed
        );
    });
}

#[test]
fn state_loss_exact_recovery_refuses_live_pid_lock_endpoint_and_mixed_lease() {
    let refusal = |status: DaemonStatus, pid_live: bool, owner_available: bool| {
        super::recover_missing_lease_state_with_operations(
            "exact-lease",
            4242,
            recorded_endpoint(),
            || Ok(status),
            move |_| pid_live,
            |_| Ok("recorded endpoint was unreachable".to_string()),
            move || Ok(owner_available.then_some(())),
            || unreachable!("refused recovery must not mutate jobs"),
            || unreachable!("refused recovery must not start a daemon"),
        )
        .expect_err("unsafe state must fail closed")
    };
    assert!(refusal(leaseless_status(1), true, true)
        .message
        .contains("still running"));
    assert!(refusal(leaseless_status(1), false, false)
        .message
        .contains("owner lock"));
    let mut endpoint_live = leaseless_status(1);
    endpoint_live.reachable = true;
    assert!(refusal(endpoint_live, false, true)
        .message
        .contains("unreachable endpoint"));
    let replay = super::recover_missing_lease_state_with_operations(
        "exact-lease",
        4242,
        recorded_endpoint(),
        || Ok(fake_dead_status(fake_daemon(4343, "replacement"))),
        |_| false,
        |_| Ok("recorded endpoint was unreachable".to_string()),
        || Ok(Some(())),
        || unreachable!("replay must not reconcile jobs"),
        || unreachable!("replay must not start another daemon"),
    )
    .expect_err("replacement state makes recovery replay fail closed");
    assert!(replay.message.contains("absent daemon state"));

    with_isolated_home(|_| {
        let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
        let exact = JobStore::open_without_reconciliation(&path)
            .expect("store")
            .with_daemon_lease("exact-lease".to_string());
        let exact_job = exact.create("runner.exec");
        exact.start(exact_job.id).expect("start exact");
        let other = JobStore::open_without_reconciliation(&path)
            .expect("store")
            .with_daemon_lease("other-lease".to_string());
        let other_job = other.create("runner.exec");
        other.start(other_job.id).expect("start other");
        let error = super::recover_missing_lease_state_with_operations(
            "exact-lease",
            4242,
            recorded_endpoint(),
            || Ok(leaseless_status(2)),
            |_| false,
            |_| Ok("recorded endpoint was unreachable".to_string()),
            || Ok(Some(())),
            || {
                let raw = std::fs::read(&path).expect("store bytes");
                let recovered =
                    JobStore::open_without_reconciliation_from_bytes(&path, &raw).expect("store");
                let diagnostics = recovered.daemon_lease_job_diagnostics("exact-lease");
                if !diagnostics.other_lease_job_ids.is_empty() {
                    return Err(crate::core::Error::validation_invalid_argument(
                        "lease_id",
                        "mixed exact leases",
                        None,
                        None,
                    ));
                }
                unreachable!("mixed lease must not reconcile")
            },
            || unreachable!("mixed lease must not start"),
        )
        .expect_err("mixed leases must fail closed");
        assert!(error.message.contains("mixed exact leases"));
        assert_eq!(
            exact.get(exact_job.id).expect("exact job").status,
            JobStatus::Running
        );
        assert_eq!(
            other.get(other_job.id).expect("other job").status,
            JobStatus::Running
        );
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

#[test]
fn state_loss_receipt_survives_start_failure_and_replay_starts_once() {
    with_isolated_home(|_| {
        let receipt_path = crate::core::paths::daemon_state_loss_recovery_receipt_file("lease-old")
            .expect("receipt path");
        let job_id = uuid::Uuid::new_v4();
        let mut receipt = super::StateLossRecoveryReceipt {
            lease_id: "lease-old".to_string(),
            recorded_pid: 4242,
            recorded_endpoint: "127.0.0.1:7421".to_string(),
            affected_job_ids: vec![job_id],
            evidence_snapshot_path: "/evidence/jobs.snapshot".to_string(),
            ownership_proof: vec!["owner lock acquired".to_string()],
            phase: super::StateLossRecoveryPhase::Reconciled,
            replacement: None,
            replacement_startup_token: None,
        };
        super::write_state_loss_receipt(&receipt_path, &receipt).expect("persist receipt");
        let error = super::complete_state_loss_replacement(&mut receipt, &receipt_path, || {
            Err(crate::core::Error::internal_unexpected(
                "replacement failed",
            ))
        })
        .expect_err("failed replacement preserves receipt");
        assert_eq!(error.details["state_loss_recovery"]["phase"], "reconciled");
        let durable = super::read_state_loss_receipt(&receipt_path)
            .expect("read receipt")
            .expect("receipt");
        assert_eq!(durable.affected_job_ids, vec![job_id]);
        assert_eq!(durable.phase, super::StateLossRecoveryPhase::Reconciled);
        let replay = super::complete_state_loss_replacement(&mut receipt, &receipt_path, || {
            Ok(fake_daemon(4343, "lease-new"))
        })
        .expect("replay replacement");
        assert_eq!(replay.replacement.lease_id, "lease-new");
        let durable = super::read_state_loss_receipt(&receipt_path)
            .expect("read receipt")
            .expect("receipt");
        assert_eq!(
            durable.phase,
            super::StateLossRecoveryPhase::ReplacementStarted
        );
        assert_eq!(durable.affected_job_ids, vec![job_id]);
    });
}

#[test]
fn state_loss_receipt_refuses_mismatched_replay_inputs() {
    let receipt = super::StateLossRecoveryReceipt {
        lease_id: "lease-old".to_string(),
        recorded_pid: 4242,
        recorded_endpoint: "127.0.0.1:7421".to_string(),
        affected_job_ids: Vec::new(),
        evidence_snapshot_path: "/evidence/jobs.snapshot".to_string(),
        ownership_proof: Vec::new(),
        phase: super::StateLossRecoveryPhase::Reconciled,
        replacement: None,
        replacement_startup_token: None,
    };
    let endpoint = "127.0.0.1:7422".parse().expect("endpoint");
    assert!(super::validate_state_loss_receipt(&receipt, "lease-old", 4242, endpoint).is_err());
}

#[test]
fn state_loss_replay_allows_zero_active_jobs_only_with_a_matching_receipt() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let endpoint = listener.local_addr().expect("endpoint");
    drop(listener);
    let status = leaseless_status(0);
    assert!(
        super::validate_state_loss_preconditions("lease-old", 4242, endpoint, &status, None)
            .is_err()
    );
    let receipt = super::StateLossRecoveryReceipt {
        lease_id: "lease-old".to_string(),
        recorded_pid: 4242,
        recorded_endpoint: endpoint.to_string(),
        affected_job_ids: Vec::new(),
        evidence_snapshot_path: "/evidence/jobs.snapshot".to_string(),
        ownership_proof: Vec::new(),
        phase: super::StateLossRecoveryPhase::Reconciled,
        replacement: None,
        replacement_startup_token: None,
    };
    assert!(super::validate_state_loss_preconditions(
        "lease-old",
        4242,
        endpoint,
        &status,
        Some(&receipt),
    )
    .is_ok());
    assert!(super::validate_state_loss_receipt(&receipt, "lease-old", 4242, endpoint).is_ok());
}

#[test]
fn replacement_starting_replay_adopts_only_the_persisted_startup_token() {
    with_isolated_home(|_| {
        let receipt_path = crate::core::paths::daemon_state_loss_recovery_receipt_file("lease-old")
            .expect("receipt path");
        let mut receipt = super::StateLossRecoveryReceipt {
            lease_id: "lease-old".to_string(),
            recorded_pid: 4242,
            recorded_endpoint: "127.0.0.1:7421".to_string(),
            affected_job_ids: Vec::new(),
            evidence_snapshot_path: "/snapshot".to_string(),
            ownership_proof: Vec::new(),
            phase: super::StateLossRecoveryPhase::Reconciled,
            replacement: None,
            replacement_startup_token: None,
        };
        let error = super::start_state_loss_replacement_with(&mut receipt, &receipt_path, |_| {
            Err(crate::core::Error::internal_unexpected(
                "interrupted after launch intent",
            ))
        })
        .expect_err("interruption leaves replayable starting receipt");
        assert_eq!(
            error.details["state_loss_recovery"]["phase"],
            "replacement_starting"
        );
        let token = receipt
            .replacement_startup_token
            .clone()
            .expect("startup token");
        let mut status = fake_status(Some(fake_daemon(4343, "lease-new")), true);
        status.state.as_mut().expect("state").startup_token = token;
        let adopted =
            super::replay_replacement_starting(receipt, &receipt_path, &status, "127.0.0.1:0")
                .expect("matching started daemon is adopted without another start");
        assert_eq!(adopted.replacement.lease_id, "lease-new");

        let mut mismatched = super::read_state_loss_receipt(&receipt_path)
            .expect("receipt")
            .expect("receipt");
        mismatched.phase = super::StateLossRecoveryPhase::ReplacementStarting;
        mismatched.replacement = None;
        mismatched.replacement_startup_token = Some("expected-token".to_string());
        let status = fake_status(Some(fake_daemon(4344, "other-lease")), true);
        assert!(super::replay_replacement_starting(
            mismatched,
            &receipt_path,
            &status,
            "127.0.0.1:0"
        )
        .is_err());
    });
}

#[test]
fn dead_lease_reconciliation_reattaches_live_or_refuses_mismatched_daemon() {
    with_isolated_home(|_| {
        let live_daemon = fake_daemon(4242, "lease-dead");
        let live = reconcile_dead_lease_and_ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            "lease-dead",
            || Ok(fake_dead_status(live_daemon.clone())),
            |_| true,
            || unreachable!("live daemon must not reconcile"),
            || unreachable!("live daemon must not start replacement"),
        )
        .expect("live replacement from a concurrent reconnect is reattached");
        assert_eq!(live, live_daemon);

        let mismatched = reconcile_dead_lease_and_ensure_running_with_operations(
            Duration::from_millis(50),
            super::super::acquire_daemon_operation_lock_for_ensure,
            "lease-expected",
            || Ok(fake_dead_status(fake_daemon(4242, "lease-other"))),
            |_| false,
            || unreachable!("mismatched lease must not reconcile"),
            || unreachable!("mismatched lease must not start replacement"),
        )
        .expect_err("mismatched lease must fail closed");
        assert!(mismatched
            .message
            .contains("does not match expected stale lease"));
    });
}

fn ensure_with_fake_operations(
    barrier: Arc<Barrier>,
    state: Arc<Mutex<FakeEnsureState>>,
    daemon: super::DaemonStartResult,
) -> std::thread::JoinHandle<crate::core::error::Result<super::DaemonStartResult>> {
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
) -> std::thread::JoinHandle<crate::core::error::Result<super::DaemonStartResult>> {
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

fn legacy_recovery_fixture(lease_id: &str) -> (JobStore, crate::core::api_jobs::Job, String) {
    let path = crate::core::paths::daemon_jobs_file().expect("jobs path");
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
    let state_path = crate::core::paths::daemon_state_file().expect("state path");
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

fn recorded_endpoint() -> SocketAddr {
    "127.0.0.1:4242".parse().expect("recorded endpoint")
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
