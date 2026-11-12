//! A requested stop of `daemon serve` ends the process within a bounded grace
//! period (#17891).
//!
//! SIGTERM (and SIGINT, and lifecycle/idle self-stops) only flip the serve
//! loop's shutdown flag; the drain that follows runs on the serving thread. If
//! that thread is wedged (blocked dispatching to saturated connection workers,
//! waiting on a store lock a controller job holds, or stuck anywhere else in the
//! drain), the signal was caught and nothing else happened: a service manager
//! never saw an exit and only SIGKILL worked.
//!
//! The deadline is independent of the serving thread. Once the flag is set it
//! waits [`SHUTDOWN_GRACE`], then records durable termination evidence and
//! ends the process. That abandons in-flight work exactly as SIGKILL would,
//! which is already crash-safe: controller work resumes from its checkpoint and
//! the kept lease drives dead-lease recovery on the next start (#15483, #15936).

use super::*;

/// Covers the serve drain (connection workers and helpers share one
/// [`HELPER_DRAIN_DEADLINE`] budget) plus the clean-stop bookkeeping, and stays
/// well inside a service manager's stop timeout (systemd's default is 90s).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(HELPER_DRAIN_DEADLINE.as_secs() + 5);

/// Bounds the best-effort evidence write the deadline makes before exiting.
const EVIDENCE_WRITE_BUDGET: Duration = Duration::from_secs(2);

const FLAG_POLL: Duration = Duration::from_millis(50);

/// Test-support builds may shorten the grace so subprocess tests stay fast.
#[cfg(feature = "test-support")]
pub const SHUTDOWN_GRACE_MS_TEST_ENV: &str = "HOMEBOY_TEST_DAEMON_SHUTDOWN_GRACE_MS";

fn shutdown_grace() -> Duration {
    #[cfg(feature = "test-support")]
    if let Some(ms) = std::env::var(SHUTDOWN_GRACE_MS_TEST_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    SHUTDOWN_GRACE
}

/// Arm the shutdown deadline for this process's `daemon serve`. Only the CLI
/// process boundary calls this: it ends the whole process, so in-process test
/// servers must never arm it.
pub fn arm_shutdown_deadline(shutdown: Arc<AtomicBool>) -> Result<()> {
    let grace = shutdown_grace();
    std::thread::Builder::new()
        .name("homeboy-shutdown-deadline".to_string())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                std::thread::sleep(FLAG_POLL);
            }
            std::thread::sleep(grace);
            exit_at_shutdown_deadline(grace)
        })
        .map(|_| ())
        .map_err(|error| {
            Error::internal_unexpected(format!("spawn daemon shutdown deadline: {error}"))
        })
}

fn exit_at_shutdown_deadline(grace: Duration) -> ! {
    eprintln!(
        "homeboy daemon: requested shutdown did not finish within {}ms; exiting at the shutdown deadline (in-flight work resumes on the next start)",
        grace.as_millis()
    );
    // The evidence write touches the filesystem and the job store; it must not
    // become the next thing the exit waits on.
    let (written_tx, written_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("homeboy-shutdown-evidence".to_string())
        .spawn(move || {
            if let Err(error) = record_deadline_termination(grace) {
                eprintln!(
                    "homeboy daemon: could not record shutdown-deadline evidence: {}",
                    error.message
                );
            }
            let _ = written_tx.send(());
        });
    let _ = written_rx.recv_timeout(EVIDENCE_WRITE_BUDGET);
    exit_process_now()
}

/// Record that this daemon's requested stop ended at the deadline. The lease
/// is deliberately kept: the next start sees a dead lease and recovers any work
/// this process abandoned.
fn record_deadline_termination(grace: Duration) -> Result<()> {
    let path = paths::daemon_state_file()?;
    if lifetime::state_dir_removed(&path) {
        return Ok(());
    }
    let state = validate_lease_file(&path)?
        .state
        .filter(|state| state.pid == std::process::id());
    let Some(state) = state else {
        return Ok(());
    };
    let scope = daemon_scope_for_state_path(&path);
    let active_jobs =
        JobStore::active_count_at_path(paths::observation_db()?, Some(&scope)).unwrap_or(0);
    write_termination_evidence(&deadline_termination_evidence(&state, active_jobs, grace))
}

pub(super) fn deadline_termination_evidence(
    state: &DaemonState,
    active_jobs: usize,
    grace: Duration,
) -> DaemonTerminationEvidence {
    DaemonTerminationEvidence {
        classification: DaemonTerminationClassification::CleanStop,
        observed_at: chrono::Utc::now().to_rfc3339(),
        lease_id: Some(state.lease_id.clone()),
        pid: Some(state.pid),
        binary_identity: Some(state.build_identity.display.clone()),
        active_jobs,
        resource_evidence:
            "unavailable: the shutdown deadline does not collect OS resource snapshots".to_string(),
        os_evidence: format!(
            "graceful shutdown requested by SIGINT, SIGTERM, or a lifecycle stop did not finish within {}ms; the process exited at the shutdown deadline",
            grace.as_millis()
        ),
        stop_requested: true,
    }
}

/// End the process without running anything that could block: no atexit
/// handlers, no stdio flushes behind a lock another thread may hold.
fn exit_process_now() -> ! {
    #[cfg(unix)]
    {
        // SAFETY: `_exit` has no preconditions; it never returns.
        unsafe { libc::_exit(0) }
    }
    #[cfg(not(unix))]
    {
        std::process::exit(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_covers_the_serve_drain_and_stays_inside_a_service_manager_stop_timeout() {
        assert!(SHUTDOWN_GRACE > HELPER_DRAIN_DEADLINE);
        assert!(SHUTDOWN_GRACE < Duration::from_secs(90));
    }

    #[test]
    fn deadline_evidence_is_a_requested_stop_that_keeps_the_lease_owner() {
        crate::test_support::with_isolated_home(|_| {
            let state = write_state("127.0.0.1:31017".parse().unwrap(), None).unwrap();
            let evidence = deadline_termination_evidence(&state, 2, Duration::from_millis(1500));
            assert_eq!(
                evidence.classification,
                DaemonTerminationClassification::CleanStop
            );
            assert!(evidence.stop_requested);
            assert_eq!(evidence.lease_id.as_deref(), Some(state.lease_id.as_str()));
            assert_eq!(evidence.active_jobs, 2);
            assert!(evidence.os_evidence.contains("1500ms"));
            assert!(evidence.os_evidence.contains("shutdown deadline"));
        });
    }
}
