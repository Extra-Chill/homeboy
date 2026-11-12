//! Restart safety for the runner's one daemon.
//!
//! A refresh restarts the daemon in place: the same state directory under a
//! new lease. Pollers that lost the stopped lease's tunnel must not reattach
//! it while the restart is in flight, and must find the restarted daemon once
//! it is published. Both sides serialize through one runner-scoped recovery
//! lock, and every job, run and artifact routes through the runner's one
//! current session, so publishing the restarted session is the whole rebind.

use super::*;

const RESTART_BUDGET: Duration = Duration::from_secs(5 * 60);

#[derive(serde::Serialize, serde::Deserialize)]
struct RestartState {
    holder_pid: u32,
    phase: String,
    started_at: i64,
    budget_ms: u64,
    previous_lease: String,
}

impl RestartState {
    fn refusal(&self, runner_id: &str) -> Error {
        let mut error = Error::new(
            homeboy_core::error::ErrorCode::InternalUnexpected,
            format!(
                "runner {runner_id} restart {} (holder pid {})",
                self.phase, self.holder_pid
            ),
            serde_json::json!({
                "kind": "runner_restarting",
                "substrate_wait": "runner_restarting",
                "runner_id": runner_id,
                "restart": self,
            }),
        );
        error.retryable = Some(true);
        error
    }

    fn check(&self, runner_id: &str) -> Result<bool> {
        if chrono::Utc::now().timestamp_millis() - self.started_at >= self.budget_ms as i64 {
            return Err(self.refusal(runner_id));
        }
        Ok(true)
    }

    /// Whether a poller still waits on this restart: only while its holder is alive.
    fn in_progress(&self) -> bool {
        self.phase == "restarting" && homeboy_core::process::pid_is_running(self.holder_pid)
    }

    /// An unpublished restart stopped `previous_lease`; only that lease is refused.
    /// A session republished since (for example by `runner connect`) names a
    /// different lease and proceeds, so a failed or abandoned restart never
    /// wedges the runner.
    fn stopped(&self, current_lease: Option<&str>) -> bool {
        self.phase != "published" && current_lease.is_none_or(|lease| lease == self.previous_lease)
    }
}

/// Publish the restart lifecycle under the recovery lock, never runner-state.lock.
/// Failed state remains visible so pollers cannot revive a stopped lease.
pub(crate) fn with_restart<T>(
    runner_id: &str,
    previous_lease: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let root = paths::homeboy()?;
    let directory = crate::runner_state_lock::runner_state_dir_in_root(&root, runner_id);
    let state_path = directory.join("restart-state");
    crate::runner_state_lock::with_observed_lock(
        directory.join("restart-recovery.lock"),
        runner_id,
        "restart_recovery",
        crate::runner_state_lock::LOCK_TIMEOUT,
        || {
            if let Some(state) =
                crate::runner_state_lock::read_durable_json::<RestartState>(&state_path)?
            {
                if state.phase == "restarting" {
                    return state.check(runner_id);
                }
            }
            Ok(false)
        },
        || {
            let mut state = RestartState {
                holder_pid: std::process::id(),
                phase: "restarting".into(),
                started_at: chrono::Utc::now().timestamp_millis(),
                budget_ms: RESTART_BUDGET.as_millis() as u64,
                previous_lease: previous_lease.into(),
            };
            crate::runner_state_lock::write_durable_json(&state_path, &state)?;
            let result = operation().and_then(|value| state.check(runner_id).map(|_| value));
            state.phase = if result.is_ok() {
                "published"
            } else {
                "failed"
            }
            .into();
            crate::runner_state_lock::write_durable_json(&state_path, &state)?;
            match result {
                Ok(value) => Ok(value),
                Err(cause) => {
                    let mut error = state.refusal(runner_id);
                    error.message = format!("{}: {}", error.message, cause.message);
                    error.details["cause"] =
                        serde_json::json!({"message": cause.message, "details": cause.details});
                    Err(error)
                }
            }
        },
    )
}

/// Serialize daemon restart with poller reattachment for one runner.
///
/// `restart_daemon_in_place` holds this lock from the stop of the previous
/// lease until the restarted session is published. A poller that lost its
/// tunnel takes the same lock before reattaching, so it waits for the
/// restarted session instead of reattaching the stopped lease and reporting
/// its job lost. The key is the runner, never a lease: the lease is exactly
/// what changes across the restart.
///
/// Lock order is always this lock, then the runner state lock. Nothing that
/// holds the runner state lock takes this one.
pub(crate) fn with_restart_recovery_lock<T>(
    runner_id: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let config_root = paths::homeboy()?;
    let directory = crate::runner_state_lock::runner_state_dir_in_root(&config_root, runner_id);
    let state_path = directory.join("restart-state");
    crate::runner_state_lock::with_observed_lock(
        directory.join("restart-recovery.lock"),
        runner_id,
        "restart_recovery",
        crate::runner_state_lock::LOCK_TIMEOUT,
        || {
            let state = crate::runner_state_lock::read_durable_json::<RestartState>(&state_path)?;
            if let Some(state) = state.filter(RestartState::in_progress) {
                let active = state.check(runner_id)?;
                return Ok(active);
            }
            Ok(false)
        },
        || {
            if let Some(state) =
                crate::runner_state_lock::read_durable_json::<RestartState>(&state_path)?
            {
                // A failed restart must never let a poller reattach the stopped lease.
                let current = read_session(runner_id)?;
                if state.stopped(
                    current
                        .as_ref()
                        .and_then(|s| s.remote_daemon_lease_id.as_deref()),
                ) {
                    return Err(state.refusal(runner_id));
                }
            }
            operation()
        },
    )
}

/// Publish the restarted daemon as the runner's one current session after an
/// in-place restart of `previous_lease`.
///
/// The restarted daemon re-adopted every job, run and artifact from its state
/// directory, and those route through the current session, so nothing is
/// moved: the session the restart connected is the new owner of all of it.
/// Republishing an already-published session is a no-op, so a retried rebind
/// is idempotent. A current session that names neither the stopped lease nor
/// the restarted one belongs to a different daemon, and is never overwritten.
pub(crate) fn rebind_restarted_lease(
    runner_id: &str,
    previous_lease: &str,
    restarted: &RunnerSession,
) -> Result<()> {
    let restarted_lease = restarted
        .remote_daemon_lease_id
        .clone()
        .filter(|lease_id| !lease_id.is_empty())
        .ok_or_else(|| Error::internal_unexpected("restarted daemon session has no lease ID"))?;
    // Remote I/O must stay outside runner-state.lock: only local session
    // validation and publication belong in this critical section.
    crate::runner_state_lock::with_runner_registry_lock(runner_id, || {
        let current = read_session(runner_id)?;
        if current.as_ref() == Some(restarted) {
            return Ok(());
        }
        let current_lease = current
            .as_ref()
            .and_then(|session| session.remote_daemon_lease_id.as_deref());
        if current_lease.is_some_and(|lease| lease != previous_lease && lease != restarted_lease) {
            return Err(Error::validation_invalid_argument(
                "runner",
                format!(
                    "runner session moved to another daemon while lease `{previous_lease}` was restarting; refusing to publish the restarted lease"
                ),
                Some(runner_id.to_string()),
                None,
            ));
        }
        write_session(restarted)
    })
}

/// Persist a freshly reattached tunnel as the runner's current session and
/// return the session it replaced, whose stale tunnel the caller closes.
///
/// A reattachment only refreshes the local tunnel to the daemon the current
/// session already names. It never changes which daemon that is, so a lease
/// that no longer matches the current session is refused.
pub(super) fn persist_reconnected_session(
    runner_id: &str,
    reconnected: &RunnerSession,
) -> Result<RunnerSession> {
    crate::runner_state_lock::with_runner_registry_lock(runner_id, || {
        let current = read_session(runner_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                "runner has no current daemon session to reattach",
                Some(runner_id.to_string()),
                None,
            )
        })?;
        if current.remote_daemon_lease_id.is_none()
            || current.remote_daemon_lease_id != reconnected.remote_daemon_lease_id
        {
            return Err(Error::validation_invalid_argument(
                "runner",
                "reattached daemon lease does not match the current daemon session",
                Some(runner_id.to_string()),
                None,
            ));
        }
        if !replace_session_if_matches(&current, reconnected)? {
            return Err(Error::validation_invalid_argument(
                "runner",
                "runner session changed while its tunnel was being reattached",
                Some(runner_id.to_string()),
                None,
            ));
        }
        Ok(current)
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn session(lease: &str, tunnel_pid: Option<u32>) -> RunnerSession {
        let mut session = crate::test_support::direct_ssh_session(lease);
        session.runner_id = "runner-a".to_string();
        session.tunnel_pid = tunnel_pid;
        session
    }

    /// Exercise the same recovery entry point used by transport pollers.
    #[test]
    fn pollers_follow_the_restart_budget_and_outcome() {
        crate::test_support::with_isolated_home(|_| {
            for outcome in ["published", "failed", "timeout"] {
                write_session(&session("lease-old", None)).expect("connected session");
                let failure = outcome != "published";
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                let ready = barrier.clone();
                let restart = std::thread::spawn(move || {
                    with_restart("runner-a", "lease-old", || {
                        if outcome == "timeout" {
                            let path = crate::runner_state_lock::runner_state_dir_in_root(
                                &paths::homeboy()?,
                                "runner-a",
                            )
                            .join("restart-state");
                            let mut state: RestartState =
                                crate::runner_state_lock::read_durable_json(&path)?
                                    .expect("published restart state");
                            state.started_at -= RESTART_BUDGET.as_millis() as i64;
                            crate::runner_state_lock::write_durable_json(&path, &state)?;
                        }
                        ready.wait();
                        if failure {
                            std::thread::sleep(Duration::from_millis(100));
                            return Err(Error::internal_unexpected("simulated restart failure"));
                        }
                        // Deliberately exceed the old generic 30 second timeout.
                        std::thread::sleep(Duration::from_secs(31));
                        rebind_restarted_lease("runner-a", "lease-old", &session("lease-new", None))
                    })
                });
                barrier.wait();
                let state_path = crate::runner_state_lock::runner_state_dir_in_root(
                    &paths::homeboy().expect("root"),
                    "runner-a",
                )
                .join("restart-state");
                let state: RestartState = crate::runner_state_lock::read_durable_json(&state_path)
                    .expect("restart state")
                    .expect("observable restart");
                assert_eq!(state.phase, "restarting");
                assert_eq!(state.holder_pid, std::process::id());
                assert_eq!(state.previous_lease, "lease-old");
                assert!(state.started_at > 0);
                assert_eq!(state.budget_ms, RESTART_BUDGET.as_millis() as u64);
                let observed = with_restart_recovery_lock("runner-a", || read_session("runner-a"));
                if failure {
                    let error = observed.expect_err("restart failure refuses old lease");
                    assert_eq!(error.details["kind"], "runner_restarting");
                    assert_eq!(error.retryable, Some(true));
                    assert!(homeboy_core::substrate_wait::is_transient_error(&error));
                } else {
                    assert_eq!(
                        observed
                            .expect("wait succeeds")
                            .expect("session")
                            .remote_daemon_lease_id
                            .as_deref(),
                        Some("lease-new")
                    );
                }
                assert_eq!(restart.join().expect("restart thread").is_err(), failure);
            }

            // After the failed restart, a session republished under a new lease
            // (for example by `runner connect`) is not wedged by the stale state.
            write_session(&session("lease-connected", None)).expect("republished session");
            let observed = with_restart_recovery_lock("runner-a", || read_session("runner-a"))
                .expect("republished session proceeds")
                .expect("session");
            assert_eq!(
                observed.remote_daemon_lease_id.as_deref(),
                Some("lease-connected")
            );
        });
    }

    /// The recovery lock is keyed by runner: another runner's restart never
    /// blocks this runner's pollers.
    #[test]
    fn restart_recovery_lock_is_scoped_to_one_runner() {
        crate::test_support::with_isolated_home(|_| {
            with_restart_recovery_lock("runner-a", || {
                with_restart_recovery_lock("runner-b", || Ok(()))
            })
            .expect("independent runners do not contend");
        });
    }

    #[test]
    fn rebinding_a_restarted_lease_publishes_it_once_and_is_idempotent() {
        crate::test_support::with_isolated_home(|_| {
            write_session(&session("lease-old", None)).expect("connected session");
            let restarted = session("lease-new", Some(202));

            rebind_restarted_lease("runner-a", "lease-old", &restarted).expect("rebind");
            rebind_restarted_lease("runner-a", "lease-old", &restarted).expect("rebind again");

            assert_eq!(read_session("runner-a").expect("read"), Some(restarted));
        });
    }

    #[test]
    fn a_rebind_never_overwrites_a_session_of_another_daemon() {
        crate::test_support::with_isolated_home(|_| {
            let other = session("lease-other", Some(101));
            write_session(&other).expect("another daemon's session");

            let restarted = session("lease-new", Some(202));
            assert!(rebind_restarted_lease("runner-a", "lease-old", &restarted).is_err());
            assert_eq!(read_session("runner-a").expect("read"), Some(other));
        });
    }

    #[test]
    fn a_rebind_without_a_restarted_lease_is_refused() {
        crate::test_support::with_isolated_home(|_| {
            let mut restarted = session("lease-new", None);
            restarted.remote_daemon_lease_id = None;
            assert!(rebind_restarted_lease("runner-a", "lease-old", &restarted).is_err());
        });
    }

    #[test]
    fn a_reattached_tunnel_replaces_only_the_current_session_of_the_same_lease() {
        crate::test_support::with_isolated_home(|_| {
            let current = session("lease-a", Some(101));
            write_session(&current).expect("current session");

            let mut reattached = current.clone();
            reattached.tunnel_pid = Some(202);
            let replaced =
                persist_reconnected_session("runner-a", &reattached).expect("persist reattach");
            assert_eq!(replaced, current);
            assert_eq!(read_session("runner-a").expect("read"), Some(reattached));

            let other = session("lease-b", Some(303));
            assert!(persist_reconnected_session("runner-a", &other).is_err());
            assert_eq!(
                read_session("runner-a")
                    .expect("read")
                    .and_then(|session| session.tunnel_pid),
                Some(202),
                "a different lease never replaces the current session"
            );
        });
    }
}
