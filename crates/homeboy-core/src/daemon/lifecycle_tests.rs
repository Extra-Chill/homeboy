//! Snapshot and totality tests for [`observe`] (#15557, slice C1).
//!
//! Each snapshot rebuilds, on disk, the state one `tests/cli_binary/daemon_chaos.rs`
//! scenario leaves behind, and pins the whole generation view:
//!
//! | Chaos case | Snapshot |
//! |---|---|
//! | 1 checkpointed in-daemon job (#15420) | `job_custody_covers_every_recovery_disposition` (`DriverRecovery` ⇒ `resumable`) |
//! | 2 uncheckpointed job of a killed daemon | `killed_daemon_with_an_uncheckpointed_job_needs_attestation` |
//! | 3 binary replaced under an idle daemon | `replaced_executable_is_reported_as_replaced` |
//! | 5 foreground `daemon serve` (#15436) | `tokenless_live_lease_is_externally_supervised` |
//! | 7 restart in place (#15456, #15558) | `in_place_restart_proves_the_registered_lease_dead` |
//! | 8 dead generation with a stale job | `draining_dead_generation_with_a_dead_child_is_orphaned_with_proof` |
//! | 9 process group SIGKILL, zombies left | `zombie_lease_pid_is_dead_not_live` |
//!
//! Cases 4 and 6 are about in-flight work timing, not persisted state.

use super::*;
use crate::api_jobs::JobStore;
use crate::test_support::with_isolated_home;
use serde_json::{json, Value};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn write_lease(dir: &Path, lease_id: &str, pid: u32, startup_token: &str) {
    std::fs::create_dir_all(dir).expect("generation dir");
    let state = crate::daemon::DaemonState {
        schema: crate::daemon::DAEMON_LEASE_SCHEMA.to_string(),
        lease_id: lease_id.to_string(),
        startup_token: startup_token.to_string(),
        address: "127.0.0.1:1".to_string(),
        pid,
        state_path: dir.join("state.json").display().to_string(),
        started_at: "2026-10-06T00:00:00Z".to_string(),
        last_seen_at: "2026-10-06T00:00:00Z".to_string(),
        build_identity: crate::build_identity::current(),
        binary_sha256: crate::daemon::current_binary_sha256().expect("binary hash"),
        runtime_paths: crate::daemon::DaemonRuntimeSnapshot {
            loaded_at: "2026-10-06T00:00:00Z".to_string(),
            paths: Vec::new(),
        },
    };
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_vec(&state).expect("lease json"),
    )
    .expect("write lease");
}

/// `entries`: (lease id, state dir, drain state).
fn write_registry(router: &Path, admission_owner: &str, entries: &[(&str, &Path, &str)]) {
    std::fs::create_dir_all(router).expect("router dir");
    let generations: serde_json::Map<String, Value> = entries
        .iter()
        .map(|(lease_id, dir, drain)| {
            (
                lease_id.to_string(),
                json!({
                    "endpoint": {
                        "lease_id": lease_id,
                        "address": "127.0.0.1:1",
                        "state_dir": dir.display().to_string(),
                        "build_identity": "test",
                    },
                    "active_jobs": 0,
                    "drain_state": drain,
                }),
            )
        })
        .collect();
    let registry = json!({
        "schema": "homeboy.daemon.generations.v1",
        "generations": {
            "admission_owner": admission_owner,
            "generations": generations,
            "job_owners": {},
        },
        "completed_jobs": [],
    });
    std::fs::write(
        router.join("generations.json"),
        serde_json::to_vec_pretty(&registry).expect("registry json"),
    )
    .expect("write registry");
}

/// A PID that existed and has been reaped.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn true");
    let pid = child.id();
    child.wait().expect("reap");
    pid
}

/// An exited, unreaped child. The caller reaps it by dropping the guard.
struct Zombie(Child);

impl Zombie {
    fn spawn() -> Self {
        let child = Command::new("true").spawn().expect("spawn true");
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !homeboy_engine_primitives::command::process_is_zombie(pid) {
            assert!(
                Instant::now() < deadline,
                "child {pid} never became a zombie"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Zombie {
    fn drop(&mut self) {
        let _ = self.0.wait();
    }
}

fn generations(view: &DaemonView) -> Value {
    serde_json::to_value(&view.generations).expect("view serializes")
}

fn dir(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn tokenless_live_lease_is_externally_supervised() {
    with_isolated_home(|home| {
        let router = home.path().join("daemon");
        write_lease(&router, "SERVE", std::process::id(), "");

        let view = observe(&router);

        assert_eq!(view.registry, RegistryObservation::Absent);
        assert_eq!(view.admission_owner, None);
        assert_eq!(
            generations(&view),
            json!([{
                "lease_id": "SERVE",
                "state_dir": dir(&router),
                "registered": false,
                "lifecycle": { "state": "admitting" },
                "process": { "pid": std::process::id(), "liveness": "live" },
                "supervision": "external",
                "binary": { "state": "current" },
                "jobs": [],
            }])
        );
    });
}

#[test]
fn supervised_live_lease_in_a_draining_generation() {
    with_isolated_home(|home| {
        let router = home.path().join("daemon");
        write_lease(&router, "OLD", std::process::id(), "launcher-credential");
        write_registry(&router, "OLD", &[("OLD", &router, "draining")]);

        let view = observe(&router);

        assert_eq!(view.registry, RegistryObservation::Present);
        assert_eq!(view.admission_owner.as_deref(), Some("OLD"));
        assert_eq!(
            generations(&view),
            json!([{
                "lease_id": "OLD",
                "state_dir": dir(&router),
                "registered": true,
                "lifecycle": { "state": "draining" },
                "process": { "pid": std::process::id(), "liveness": "live" },
                "supervision": "homeboy_supervised",
                "binary": { "state": "current" },
                "jobs": [],
            }])
        );
    });
}

#[test]
fn in_place_restart_proves_the_registered_lease_dead() {
    with_isolated_home(|home| {
        // The registry still routes to DEAD, but its directory now holds the
        // restarted daemon's lease. That is the #15558 admission bug's input.
        let router = home.path().join("daemon");
        write_lease(&router, "LIVE", std::process::id(), "");
        write_registry(&router, "DEAD", &[("DEAD", &router, "admitting")]);

        let view = observe(&router);

        assert_eq!(view.admission_owner.as_deref(), Some("DEAD"));
        assert_eq!(
            generations(&view),
            json!([
                {
                    "lease_id": "DEAD",
                    "state_dir": dir(&router),
                    "registered": true,
                    "lifecycle": {
                        "state": "dead",
                        "proof": { "kind": "same_dir_replaced", "current_lease_id": "LIVE" },
                    },
                    "process": { "liveness": "unknown" },
                    "supervision": "unknown",
                    "binary": { "state": "unknown" },
                    "jobs": [],
                },
                {
                    "lease_id": "LIVE",
                    "state_dir": dir(&router),
                    "registered": false,
                    "lifecycle": { "state": "admitting" },
                    "process": { "pid": std::process::id(), "liveness": "live" },
                    "supervision": "external",
                    "binary": { "state": "current" },
                    "jobs": [],
                },
            ])
        );
    });
}

#[cfg(target_os = "linux")]
#[test]
fn zombie_lease_pid_is_dead_not_live() {
    with_isolated_home(|home| {
        let zombie = Zombie::spawn();
        let router = home.path().join("daemon");
        write_lease(&router, "KILLED", zombie.pid(), "launcher-credential");
        write_registry(&router, "KILLED", &[("KILLED", &router, "admitting")]);

        let view = observe(&router);

        assert_eq!(
            generations(&view),
            json!([{
                "lease_id": "KILLED",
                "state_dir": dir(&router),
                "registered": true,
                "lifecycle": { "state": "dead", "proof": { "kind": "pid_dead" } },
                "process": { "pid": zombie.pid(), "liveness": "zombie" },
                "supervision": "homeboy_supervised",
                "binary": { "state": "unknown" },
                "jobs": [],
            }])
        );
        assert!(
            view.unleased_candidates
                .iter()
                .all(|candidate| candidate.pid != zombie.pid()),
            "a zombie is never an unleased candidate"
        );
    });
}

#[cfg(target_os = "linux")]
#[test]
fn replaced_executable_is_reported_as_replaced() {
    with_isolated_home(|home| {
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .expect("a sleep binary");
        let installed = home.path().join("installed-daemon");
        std::fs::copy(sleep, &installed).expect("install copy");
        let mut child = spawn_retrying_text_busy(&installed);
        std::fs::remove_file(&installed).expect("replace the installed binary");

        let router = home.path().join("daemon");
        write_lease(&router, "UPGRADED", child.id(), "launcher-credential");
        let view = observe(&router);
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(view.generations.len(), 1);
        assert_eq!(view.generations[0].binary, BinaryFreshness::Replaced);
        assert_eq!(
            view.generations[0].lifecycle,
            GenerationLifecycle::Admitting
        );
    });
}

/// A freshly copied executable can briefly report `ETXTBSY` while another
/// test thread's fork still holds an inherited write descriptor to it.
#[cfg(target_os = "linux")]
fn spawn_retrying_text_busy(path: &Path) -> Child {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Command::new(path).arg("60").spawn() {
            Ok(child) => return child,
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                assert!(Instant::now() < deadline, "executable stayed busy");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("spawn {}: {error}", path.display()),
        }
    }
}

#[test]
fn killed_daemon_with_an_uncheckpointed_job_needs_attestation() {
    with_isolated_home(|home| {
        let router = home.path().join("daemon");
        write_lease(&router, "KILLED", dead_pid(), "launcher-credential");
        write_registry(&router, "KILLED", &[("KILLED", &router, "admitting")]);
        let store = JobStore::open(router.join("jobs.json"))
            .expect("job store")
            .with_daemon_lease("KILLED".to_string());
        let job = store.create("runner.exec");
        store.start(job.id).expect("running job");

        let view = observe(&router);

        assert_eq!(
            generations(&view)[0]["jobs"],
            json!([{
                "job_id": job.id,
                "operation": "runner.exec",
                "disposition": "blocking_ambiguous",
                "custody": { "state": "orphaned", "proof": null },
            }])
        );
        assert_eq!(
            generations(&view)[0]["lifecycle"],
            json!({ "state": "dead", "proof": { "kind": "pid_dead" } })
        );
    });
}

#[test]
fn draining_dead_generation_with_a_dead_child_is_orphaned_with_proof() {
    with_isolated_home(|home| {
        let router = home.path().join("daemon");
        let old = router.join("generations").join("old");
        write_lease(&router, "LIVE", std::process::id(), "launcher-credential");
        write_lease(&old, "OLD", dead_pid(), "launcher-credential");
        write_registry(
            &router,
            "LIVE",
            &[("LIVE", &router, "admitting"), ("OLD", &old, "draining")],
        );
        let store = JobStore::open(old.join("jobs.json"))
            .expect("job store")
            .with_daemon_lease("OLD".to_string());
        let job = store.create("runner.exec");
        store
            .reserve_local_child_at_with_runner_capacity(
                job.id,
                crate::api_jobs::timestamp_ms(),
                None,
            )
            .expect("reserve child");
        store
            .start_with_reserved_child_identity(
                job.id,
                dead_pid(),
                None,
                crate::api_jobs::LocalChildStartDiscriminator::LinuxProcStatStarttimeTicks {
                    ticks: 1,
                },
            )
            .expect("record child");

        let view = observe(&router);
        let old_view = view
            .generations
            .iter()
            .find(|generation| generation.lease_id.as_deref() == Some("OLD"))
            .expect("old generation");

        assert_eq!(
            old_view.lifecycle,
            GenerationLifecycle::Dead {
                proof: DeathProof::PidDead
            },
            "a dead lease outranks its registry drain state"
        );
        assert_eq!(old_view.jobs.len(), 1);
        assert_eq!(
            old_view.jobs[0].custody,
            JobCustody::Orphaned {
                proof: Some(DeathProof::PidDead)
            }
        );
        let live = view
            .generations
            .iter()
            .find(|generation| generation.lease_id.as_deref() == Some("LIVE"))
            .expect("live generation");
        assert_eq!(live.lifecycle, GenerationLifecycle::Admitting);
        assert!(live.jobs.is_empty(), "jobs stay with their own generation");
    });
}

#[test]
fn termination_evidence_for_the_exact_lease_is_the_death_proof() {
    with_isolated_home(|home| {
        let router = home.path().join("daemon");
        write_lease(&router, "STOPPED", dead_pid(), "launcher-credential");
        crate::daemon::write_termination_evidence(&crate::daemon::DaemonTerminationEvidence {
            classification: crate::daemon::DaemonTerminationClassification::UnexpectedExit,
            observed_at: "2026-10-06T00:00:00Z".to_string(),
            lease_id: Some("STOPPED".to_string()),
            pid: None,
            binary_identity: None,
            active_jobs: 0,
            resource_evidence: String::new(),
            os_evidence: String::new(),
            exit_code: None,
            signal: Some(9),
            supervisor_signal: None,
            stdout: None,
            stderr: None,
            stop_requested: false,
        })
        .expect("termination evidence");

        let view = observe(&router);

        assert_eq!(
            view.generations[0].lifecycle,
            GenerationLifecycle::Dead {
                proof: DeathProof::TerminationEvidence
            }
        );
    });
}

#[test]
fn registered_generation_without_a_lease_is_stopped() {
    with_isolated_home(|home| {
        let router = home.path().join("daemon");
        std::fs::create_dir_all(&router).unwrap();
        write_registry(&router, "GONE", &[("GONE", &router, "admitting")]);

        let view = observe(&router);

        assert_eq!(view.generations.len(), 1);
        assert_eq!(view.generations[0].lease_id.as_deref(), Some("GONE"));
        assert_eq!(view.generations[0].lifecycle, GenerationLifecycle::Stopped);
    });
}

#[test]
fn empty_router_directory_has_no_generations() {
    with_isolated_home(|home| {
        let view = observe(&home.path().join("never-created"));
        assert_eq!(view.registry, RegistryObservation::Absent);
        assert!(view.generations.is_empty());
    });
}

fn evidence(disposition: DaemonActiveJobRecoveryDisposition) -> DaemonActiveJobRecoveryEvidence {
    DaemonActiveJobRecoveryEvidence {
        job_id: Uuid::nil(),
        operation: "test".to_string(),
        status: JobStatus::Running,
        daemon_lease_id: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        started_at_ms: None,
        terminal_evidence: None,
        child_pid: None,
        child_started_at: None,
        controller_owned: false,
        linked_durable_run_id: None,
        linked_durable_run_state: None,
        linked_durable_run_terminal_status: None,
        disposition,
    }
}

#[test]
fn job_custody_covers_every_recovery_disposition() {
    use DaemonActiveJobRecoveryDisposition as D;
    let mut terminal = evidence(D::TerminalEvidence);
    terminal.linked_durable_run_terminal_status = Some(JobStatus::Succeeded);
    let cases = [
        (
            terminal,
            JobCustody::TerminalEvidence {
                status: Some(JobStatus::Succeeded),
            },
        ),
        (evidence(D::DriverRecovery), JobCustody::Resumable),
        (evidence(D::ProtectedLive), JobCustody::Running),
        (
            evidence(D::MissingChildIdentityRecoverable),
            JobCustody::Running,
        ),
        (
            evidence(D::DeadChild),
            JobCustody::Orphaned {
                proof: Some(DeathProof::PidDead),
            },
        ),
        (
            evidence(D::ReusedChildPid),
            JobCustody::Orphaned {
                proof: Some(DeathProof::PidDead),
            },
        ),
        (
            evidence(D::BlockingAmbiguous),
            JobCustody::Orphaned { proof: None },
        ),
    ];
    for (evidence, expected) in cases {
        assert_eq!(
            job_custody(&evidence),
            expected,
            "{:?}",
            evidence.disposition
        );
    }
}

/// Contents that each state file is corrupted with. `None` writes a directory
/// in place of the file, so every read of it fails with an I/O error.
const CORRUPTIONS: &[Option<&[u8]>] = &[
    Some(b""),
    Some(b"{\"schema\":\"homeboy.daemon"),
    Some(b"\x00\xff\xfe{]"),
    Some(b"null"),
    Some(b"[]"),
    Some(b"{}"),
    Some(b"{\"schema\":7,\"lease_id\":[],\"pid\":-1}"),
    None,
];

#[test]
fn observe_is_total_over_missing_and_corrupt_state_files() {
    with_isolated_home(|home| {
        for (index, file) in ["state.json", "generations.json", "jobs.json"]
            .into_iter()
            .enumerate()
        {
            for (case, corruption) in CORRUPTIONS.iter().enumerate() {
                let router = home.path().join(format!("router-{index}-{case}"));
                write_lease(&router, "LEASE", dead_pid(), "launcher-credential");
                write_registry(&router, "LEASE", &[("LEASE", &router, "admitting")]);
                let target = router.join(file);
                let _ = std::fs::remove_file(&target);
                match corruption {
                    Some(bytes) => std::fs::write(&target, bytes).unwrap(),
                    None => std::fs::create_dir_all(&target).unwrap(),
                }

                let view = observe(&router);

                serde_json::to_value(&view).expect("every view serializes");
                let label = format!("{file} corrupted with case {case}");
                match file {
                    "generations.json" => assert!(
                        matches!(view.registry, RegistryObservation::Unreadable { .. }),
                        "{label}: {:?}",
                        view.registry
                    ),
                    "state.json" => assert!(
                        view.generations.iter().all(|generation| matches!(
                            generation.lifecycle,
                            GenerationLifecycle::Unreadable { .. }
                        )),
                        "{label}: {:?}",
                        view.generations
                    ),
                    _ => assert!(
                        view.generations.iter().all(|generation| matches!(
                            generation.lifecycle,
                            GenerationLifecycle::Unreadable { .. }
                                | GenerationLifecycle::Dead { .. }
                        )),
                        "{label}: {:?}",
                        view.generations
                    ),
                }
            }
            // And with the file missing entirely.
            let router = home.path().join(format!("router-{index}-missing"));
            write_lease(&router, "LEASE", dead_pid(), "launcher-credential");
            write_registry(&router, "LEASE", &[("LEASE", &router, "admitting")]);
            let _ = std::fs::remove_file(router.join(file));
            serde_json::to_value(observe(&router)).expect("view serializes");
        }
    });
}
