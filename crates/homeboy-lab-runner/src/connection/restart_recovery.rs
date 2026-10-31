//! Restart safety for the runner's one daemon.
//!
//! A refresh restarts the daemon in place: the same state directory under a
//! new lease. Pollers that lost the stopped lease's tunnel must not reattach
//! it while the restart is in flight, and must find the restarted daemon once
//! it is published. Both sides serialize through one runner-scoped recovery
//! lock, and every job, run and artifact routes through the runner's one
//! current session, so publishing the restarted session is the whole rebind.

use super::*;

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
    crate::runner_state_lock::with_lock(
        crate::runner_state_lock::runner_state_dir_in_root(&config_root, runner_id)
            .join("restart-recovery.lock"),
        runner_id,
        "restart_recovery",
        operation,
    )
}

/// Publish the restarted daemon as the runner's one current session after an
/// in-place restart of `previous_lease`.
///
/// The restarted daemon re-adopted every job, run and artifact from its state
/// directory, and those route through the current session, so nothing is
/// moved: the session the restart connected is the new owner of all of it.
/// Republishing an already-published session is a no-op, so a retried rebind
/// is idempotent.
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
    crate::runner_state_lock::with_runner_registry_lock(runner_id, || {
        if read_session(runner_id)?.as_ref() != Some(restarted) {
            write_session(restarted)?;
        }
        crate::generation_store::adopt_restarted_lease_locked(
            runner_id,
            previous_lease,
            &restarted_lease,
            restarted,
        )
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn session(lease: &str, tunnel_pid: Option<u32>) -> RunnerSession {
        let mut session = crate::test_support::direct_ssh_session(lease);
        session.runner_id = "runner-a".to_string();
        session.tunnel_pid = tunnel_pid;
        session
    }

    /// A poller that lost its tunnel while a restart holds the recovery lock
    /// waits for the restart to publish its session, then sees that session.
    #[test]
    fn a_poller_waits_for_the_restart_to_publish_the_restarted_session() {
        crate::test_support::with_isolated_home(|_| {
            write_session(&session("lease-old", None)).expect("connected session");
            let restart_holds_lock = Arc::new(std::sync::Barrier::new(2));
            let published = Arc::new(AtomicBool::new(false));
            let restart = {
                let restart_holds_lock = Arc::clone(&restart_holds_lock);
                let published = Arc::clone(&published);
                std::thread::spawn(move || {
                    with_restart_recovery_lock("runner-a", || {
                        restart_holds_lock.wait();
                        std::thread::sleep(Duration::from_millis(150));
                        let restarted = session("lease-new", None);
                        write_session(&restarted)?;
                        rebind_restarted_lease("runner-a", "lease-old", &restarted)?;
                        published.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                })
            };
            restart_holds_lock.wait();

            let observed = with_restart_recovery_lock("runner-a", || {
                assert!(
                    published.load(Ordering::SeqCst),
                    "the poller entered recovery before the restart published its session"
                );
                read_session("runner-a")
            })
            .expect("poller recovery")
            .expect("current session");

            restart
                .join()
                .expect("restart thread")
                .expect("restart succeeds");
            assert_eq!(
                observed.remote_daemon_lease_id.as_deref(),
                Some("lease-new")
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
            let ledger = crate::generation_store::read("runner-a")
                .expect("read ledger")
                .expect("ledger");
            assert_eq!(
                ledger.generations.keys().collect::<Vec<_>>(),
                vec!["lease-new"],
                "the stopped lease is not left behind"
            );
            assert_eq!(ledger.admission_owner, "lease-new");
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
