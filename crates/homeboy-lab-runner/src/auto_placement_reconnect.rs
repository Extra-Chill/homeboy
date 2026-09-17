//! Bounded reconnect attempted before `--placement auto` degrades to local
//! execution (#14730).
//!
//! [`crate::refresh_lab_runner_readiness_for_admission`] is a pure
//! observation: it is the answer many read-only callers (`homeboy status`,
//! `runner status`, diagnostics) depend on being safe to call from any
//! context, including one that must never open a tunnel as a side effect of
//! merely asking a question (`status_with_admission_projection_until_in_roots`
//! documents this split explicitly). Reconnecting is a distinct *admission*
//! action, so it belongs only on the path that is actually about to commit to
//! a placement — this module is that one bounded, safety-gated attempt.
//!
//! The safety gate is [`crate::RunnerStatusReport::admission_action`], which
//! already encodes the exact reasoning documented at `session.rs:975`:
//! retained generations and terminal ownership uncertainty return `None`
//! (no automatic action), and only a plain, unblocked `runner.connect`
//! recommendation is treated as "reachable, session merely stale" here. Any
//! other recommendation (orphan-lease adoption, reconciliation, a refresh) is
//! left exactly as before — named to the operator, never auto-executed by a
//! placement decision.
//!
//! One case is deliberately re-admitted on top of that gate: a daemon
//! freshness probe that could not even reach the runner
//! (`DaemonStaleReasonCode::TransportUnreachable`) also returns `None` from
//! `admission_action` — unreachable and ownership-uncertain both count as "no
//! typed proof survived". But unreachability carries no job or lease evidence
//! to misinterpret, so attempting `connect()` anyway cannot mutate anything;
//! it can only fail cleanly with a bounded, reportable cause. Treating it as
//! eligible turns a silent skip into the "attempted and failed" outcome the
//! issue asks for, without touching the reasoning that fences a *reachable*
//! but unprovable daemon.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use homeboy_core::error::Result;

use crate::{connection, runner_probe_gate, LabRunnerReadiness, LabRunnerReadinessState};

/// Bound on the reconnect attempted before an `--placement auto` dispatch
/// commits to local execution. Longer than the read-only probe budget
/// ([`crate::readonly_probe::readonly_probe_timeout`]) because establishing a
/// session — not merely observing one — may spawn a tunnel and verify the
/// remote daemon, but short enough that a dead runner costs seconds, not
/// minutes.
pub const DEFAULT_AUTO_PLACEMENT_RECONNECT_TIMEOUT: Duration = Duration::from_secs(20);

const AUTO_PLACEMENT_RECONNECT_PROBE: &str = "auto_placement_reconnect";

/// What happened when `--placement auto` tried to restore a disconnected
/// runner before falling back to local. Always constructed, including when no
/// attempt was made, so a degraded run is self-describing rather than silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoPlacementReconnect {
    pub attempted: bool,
    pub succeeded: bool,
    /// The runner an attempt targeted, when one was made.
    pub runner_id: Option<String>,
    /// One line: why no attempt was made, or why the attempt that was made
    /// failed. `None` exactly when `succeeded` is `true`.
    pub reason: Option<String>,
}

impl AutoPlacementReconnect {
    fn none(reason: Option<String>) -> Self {
        Self {
            attempted: false,
            succeeded: false,
            runner_id: None,
            reason,
        }
    }

    fn attempt_failed(runner_id: String, reason: String) -> Self {
        Self {
            attempted: true,
            succeeded: false,
            runner_id: Some(runner_id),
            reason: Some(reason),
        }
    }

    fn attempt_succeeded(runner_id: String) -> Self {
        Self {
            attempted: true,
            succeeded: true,
            runner_id: Some(runner_id),
            reason: None,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "attempted": self.attempted,
            "succeeded": self.succeeded,
            "runner_id": self.runner_id,
            "reason": self.reason,
        })
    }
}

/// Before `--placement auto` degrades to local, attempt one bounded reconnect
/// when the readiness refresh found the configured Lab runner(s) disconnected.
///
/// Returns the readiness to actually route on — refreshed after a successful
/// reconnect, unchanged otherwise — paired with a diagnostic of what was
/// tried. Never called for any state but [`LabRunnerReadinessState::Disconnected`]:
/// every other state (including `Absent`, where there is nothing configured to
/// reconnect to) already resolves to its own answer.
pub fn attempt_auto_placement_reconnect(
    observed: &LabRunnerReadiness,
) -> (LabRunnerReadiness, AutoPlacementReconnect) {
    attempt_auto_placement_reconnect_until(
        observed,
        Instant::now() + DEFAULT_AUTO_PLACEMENT_RECONNECT_TIMEOUT,
    )
}

/// Deadline-aware form of [`attempt_auto_placement_reconnect`].
pub fn attempt_auto_placement_reconnect_until(
    observed: &LabRunnerReadiness,
    deadline: Instant,
) -> (LabRunnerReadiness, AutoPlacementReconnect) {
    attempt_with(
        observed,
        deadline,
        reconnect_eligible_candidates,
        reconnect_bounded,
        crate::refresh_lab_runner_readiness_for_admission,
    )
}

/// The real control flow, parameterized over its three effectful dependencies
/// so every branch — eligible-and-succeeds, eligible-and-fails,
/// ownership-blocked, nothing-configured — is deterministically testable
/// without a real runner or SSH transport.
fn attempt_with(
    observed: &LabRunnerReadiness,
    deadline: Instant,
    candidates_fn: impl FnOnce(Instant) -> Result<Vec<ReconnectCandidate>>,
    reconnect_fn: impl FnOnce(&str, Instant) -> Result<()>,
    refresh_fn: impl FnOnce() -> Result<LabRunnerReadiness>,
) -> (LabRunnerReadiness, AutoPlacementReconnect) {
    if observed.state != LabRunnerReadinessState::Disconnected {
        return (observed.clone(), AutoPlacementReconnect::none(None));
    }

    let candidates = match candidates_fn(deadline) {
        Ok(candidates) => candidates,
        Err(error) => {
            return (
                observed.clone(),
                AutoPlacementReconnect::none(Some(format!(
                    "could not enumerate configured Lab runners: {}",
                    error.message
                ))),
            );
        }
    };

    let Some(runner_id) = candidates
        .into_iter()
        .find_map(|candidate| match candidate {
            ReconnectCandidate::Eligible(runner_id) => Some(runner_id),
            ReconnectCandidate::Blocked { .. } => None,
        })
    else {
        return (
            observed.clone(),
            AutoPlacementReconnect::none(Some(
                "every configured Lab runner is disconnected and blocked from automatic \
                 reconnect (retained generations or terminal ownership uncertainty); see \
                 `homeboy runner status --full`"
                    .to_string(),
            )),
        );
    };

    match reconnect_fn(&runner_id, deadline) {
        Ok(()) => match refresh_fn() {
            Ok(refreshed) => (
                refreshed,
                AutoPlacementReconnect::attempt_succeeded(runner_id),
            ),
            // The reconnect itself succeeded; a failed post-reconnect refresh
            // must not claim it as a placement win. Report the attempt as
            // failed and keep routing on the pre-reconnect observation.
            Err(error) => (
                observed.clone(),
                AutoPlacementReconnect::attempt_failed(
                    runner_id,
                    format!(
                        "reconnected, but the post-reconnect readiness refresh failed: {}",
                        error.message
                    ),
                ),
            ),
        },
        Err(error) => (
            observed.clone(),
            AutoPlacementReconnect::attempt_failed(runner_id, error.message),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReconnectCandidate {
    Eligible(String),
    Blocked { runner_id: String },
}

/// Configured Lab runners observed disconnected, classified by whether a
/// bounded reconnect is safe to attempt.
///
/// Eligible exactly when [`crate::RunnerStatusReport::admission_action`]
/// recommends a plain `runner.connect`, or when the only reason no action was
/// recommended is that the runner could not be reached at all (see the module
/// doc). Every other reason — a reachable daemon with no typed ownership
/// proof, retained generations — is `Blocked`, preserved exactly.
fn reconnect_eligible_candidates(deadline: Instant) -> Result<Vec<ReconnectCandidate>> {
    let preferred = homeboy_core::defaults::load_config().lab.preferred_runner;
    let mut runner_ids = crate::configured_lab_runner_ids()?;
    runner_ids.sort_by_key(|runner_id| {
        (
            Some(runner_id.as_str()) != preferred.as_deref(),
            runner_id.clone(),
        )
    });

    Ok(runner_ids
        .into_iter()
        .take(crate::DETACHED_QUEUE_REFRESH_LIMIT)
        .filter_map(|runner_id| {
            let status = connection::status_until(&runner_id, deadline).ok()?;
            if status.is_connected() {
                // Already connected: readiness would not have reported this
                // runner as part of a `Disconnected` state.
                return None;
            }
            Some(if is_eligible_for_reconnect(&status) {
                ReconnectCandidate::Eligible(runner_id)
            } else {
                ReconnectCandidate::Blocked { runner_id }
            })
        })
        .collect())
}

fn is_eligible_for_reconnect(status: &crate::RunnerStatusReport) -> bool {
    if status
        .admission_action()
        .is_some_and(|action| action.id == "runner.connect")
    {
        return true;
    }
    status.daemon_freshness.as_ref().is_some_and(|freshness| {
        freshness.stale_reason_code
            == Some(homeboy_core::daemon::DaemonStaleReasonCode::TransportUnreachable)
    })
}

/// Run the real reconnect on a bounded budget, single-flighted through
/// [`runner_probe_gate`] so a fan-out of several `--placement auto` dispatches
/// discovering the same disconnected runner in one controller process attempt
/// exactly one reconnect rather than each opening their own tunnel.
fn reconnect_bounded(runner_id: &str, deadline: Instant) -> Result<()> {
    let runner_id_owned = runner_id.to_string();
    runner_probe_gate::deduplicated_probe_until(
        runner_id,
        AUTO_PLACEMENT_RECONNECT_PROBE,
        "connect",
        deadline,
        move || connect_within_deadline(&runner_id_owned, deadline),
    )
}

fn connect_within_deadline(runner_id: &str, deadline: Instant) -> Result<()> {
    let runner_id = runner_id.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = connection::connect(&runner_id);
        let _ = tx.send(result);
    });

    let remaining = deadline
        .checked_duration_since(Instant::now())
        .unwrap_or(Duration::ZERO);
    match rx.recv_timeout(remaining) {
        Ok(Ok((report, _exit_code))) if report.connected => Ok(()),
        Ok(Ok((report, _exit_code))) => Err(homeboy_core::error::Error::internal_unexpected(
            report
                .failure_message
                .unwrap_or_else(|| "runner connect did not report a live session".to_string()),
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(homeboy_core::error::Error::new(
            homeboy_core::error::ErrorCode::RemoteCommandTimeout,
            "bounded reconnect did not finish before the auto-placement deadline",
            serde_json::json!({ "stage": "auto_placement_reconnect" }),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readiness(state: LabRunnerReadinessState) -> LabRunnerReadiness {
        LabRunnerReadiness {
            state,
            selected_runner_id: None,
            available_runner_ids: Vec::new(),
            reasons: Vec::new(),
            remediation_commands: Vec::new(),
        }
    }

    /// Any state but `Disconnected` is untouched: there is either already a
    /// usable runner, nothing configured, or a state some other bounded
    /// refresh already owns (`Stale`).
    #[test]
    fn non_disconnected_states_are_never_touched() {
        for state in [
            LabRunnerReadinessState::Absent,
            LabRunnerReadinessState::ConnectedReady,
            LabRunnerReadinessState::ConnectedIneligible,
            LabRunnerReadinessState::Stale,
            LabRunnerReadinessState::CapacityBlocked,
        ] {
            let observed = readiness(state);
            let (resolved, attempt) = attempt_auto_placement_reconnect(&observed);
            assert_eq!(resolved, observed);
            assert!(!attempt.attempted);
            assert!(!attempt.succeeded);
        }
    }

    /// With no Lab runner configured at all, `Disconnected` still resolves
    /// without attempting anything (there is nothing to reconnect to) and
    /// says so rather than silently doing nothing. Isolated so this reads the
    /// test's empty config, never the ambient machine's runner registry.
    #[test]
    fn disconnected_with_nothing_configured_makes_no_attempt() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let observed = readiness(LabRunnerReadinessState::Disconnected);
            let (resolved, attempt) = attempt_auto_placement_reconnect(&observed);

            assert_eq!(resolved, observed);
            assert!(!attempt.attempted);
            assert!(attempt.reason.is_some());
        });
    }

    #[test]
    fn diagnostic_serializes_every_field() {
        let attempt = AutoPlacementReconnect::attempt_failed(
            "homeboy-lab".to_string(),
            "connection refused".to_string(),
        );
        let json = attempt.to_json();

        assert_eq!(json["attempted"], true);
        assert_eq!(json["succeeded"], false);
        assert_eq!(json["runner_id"], "homeboy-lab");
        assert_eq!(json["reason"], "connection refused");
    }

    fn connected_ready(runner_id: &str) -> LabRunnerReadiness {
        LabRunnerReadiness {
            state: LabRunnerReadinessState::ConnectedReady,
            selected_runner_id: Some(runner_id.to_string()),
            available_runner_ids: vec![runner_id.to_string()],
            reasons: Vec::new(),
            remediation_commands: Vec::new(),
        }
    }

    /// Acceptance case: a reachable-but-stale runner (session disconnected,
    /// no ownership blocker) reconnects within the bounded deadline and the
    /// caller receives the *refreshed* readiness — the exact evidence that
    /// lets `--placement auto` dispatch to Lab instead of local (#14730).
    #[test]
    fn a_reachable_but_stale_runner_reconnects_and_becomes_dispatchable() {
        let observed = readiness(LabRunnerReadinessState::Disconnected);
        let deadline = Instant::now() + Duration::from_secs(5);

        let (resolved, attempt) = attempt_with(
            &observed,
            deadline,
            |_deadline| {
                Ok(vec![ReconnectCandidate::Eligible(
                    "homeboy-lab".to_string(),
                )])
            },
            |runner_id, _deadline| {
                assert_eq!(runner_id, "homeboy-lab");
                Ok(())
            },
            || Ok(connected_ready("homeboy-lab")),
        );

        assert!(attempt.attempted);
        assert!(attempt.succeeded);
        assert_eq!(attempt.runner_id.as_deref(), Some("homeboy-lab"));
        assert!(attempt.reason.is_none());
        assert_eq!(resolved.state, LabRunnerReadinessState::ConnectedReady);
        assert_eq!(resolved.selected_runner_id.as_deref(), Some("homeboy-lab"));
    }

    /// Acceptance case: an unreachable runner's reconnect attempt fails (here,
    /// simulating the bounded deadline expiring) and `auto` falls back to
    /// local while stating that a reconnect was attempted and failed — never
    /// silently.
    #[test]
    fn an_unreachable_runner_falls_back_to_local_and_names_the_failed_attempt() {
        let observed = readiness(LabRunnerReadinessState::Disconnected);
        let deadline = Instant::now() + Duration::from_secs(5);

        let (resolved, attempt) = attempt_with(
            &observed,
            deadline,
            |_deadline| {
                Ok(vec![ReconnectCandidate::Eligible(
                    "homeboy-lab".to_string(),
                )])
            },
            |_runner_id, _deadline| {
                Err(homeboy_core::error::Error::new(
                    homeboy_core::error::ErrorCode::RemoteCommandTimeout,
                    "bounded reconnect did not finish before the auto-placement deadline",
                    serde_json::json!({}),
                ))
            },
            || panic!("a failed reconnect must never consult post-reconnect readiness"),
        );

        assert!(attempt.attempted);
        assert!(!attempt.succeeded);
        assert_eq!(attempt.runner_id.as_deref(), Some("homeboy-lab"));
        assert!(attempt
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("deadline")));
        // Falling back to local means routing on the original observation,
        // unchanged — never a fabricated "ready" state.
        assert_eq!(resolved, observed);
    }

    /// Acceptance case: retained generations or terminal ownership
    /// uncertainty (session.rs:975) must still block dispatch exactly as
    /// before. No candidate is eligible, so no reconnect is attempted at all,
    /// and the existing operator action remains the answer.
    #[test]
    fn ownership_blocked_runners_are_never_attempted() {
        let observed = readiness(LabRunnerReadinessState::Disconnected);
        let deadline = Instant::now() + Duration::from_secs(5);

        let (resolved, attempt) = attempt_with(
            &observed,
            deadline,
            |_deadline| {
                Ok(vec![ReconnectCandidate::Blocked {
                    runner_id: "homeboy-lab".to_string(),
                }])
            },
            |_runner_id, _deadline| panic!("an ownership-blocked candidate must never be dialed"),
            || panic!("no successful reconnect occurred; readiness must not be re-refreshed"),
        );

        assert!(!attempt.attempted);
        assert!(!attempt.succeeded);
        assert!(attempt.runner_id.is_none());
        assert!(attempt.reason.is_some());
        assert_eq!(resolved, observed);
    }

    /// End-to-end against the *real* candidate classification and reconnect
    /// path (no injected fakes): a configured but unreachable Lab runner is
    /// still classified eligible (transport-unreachable carries no ownership
    /// evidence to misinterpret), the real bounded `connect()` attempt fails
    /// quickly against the unroutable address, and the whole call returns
    /// well inside its deadline reporting an attempted, failed reconnect
    /// rather than hanging or silently skipping (#14730 acceptance: "an
    /// unreachable runner falls back to local within a bounded deadline and
    /// states that a reconnect was attempted and failed").
    #[test]
    fn an_unreachable_configured_runner_is_attempted_and_bounded_end_to_end() {
        homeboy_core::test_support::with_isolated_home(|_| {
            homeboy_core::server::create(
                r#"{"id":"homeboy-lab","host":"192.0.2.1","user":"user"}"#,
                false,
            )
            .expect("create server");
            crate::create(
                r#"{"id":"homeboy-lab","kind":"ssh","server_id":"homeboy-lab"}"#,
                false,
            )
            .expect("enable runner capability");

            let observed = readiness(LabRunnerReadinessState::Disconnected);
            let deadline = Instant::now() + Duration::from_secs(5);
            let started = Instant::now();

            let (resolved, attempt) = attempt_auto_placement_reconnect_until(&observed, deadline);

            assert!(
                started.elapsed() < Duration::from_secs(10),
                "an unreachable runner must fail fast within its bounded deadline, took {:?}",
                started.elapsed()
            );
            assert!(
                attempt.attempted,
                "unreachable is a real attempt, not a skip"
            );
            assert!(!attempt.succeeded);
            assert_eq!(attempt.runner_id.as_deref(), Some("homeboy-lab"));
            assert!(attempt.reason.is_some());
            assert_eq!(resolved, observed);
        });
    }

    /// A successful reconnect whose post-reconnect readiness refresh itself
    /// fails must not be reported as a placement win — the safe answer stays
    /// local, with the failure surfaced rather than swallowed.
    #[test]
    fn a_successful_reconnect_with_a_failed_refresh_still_falls_back_to_local() {
        let observed = readiness(LabRunnerReadinessState::Disconnected);
        let deadline = Instant::now() + Duration::from_secs(5);

        let (resolved, attempt) = attempt_with(
            &observed,
            deadline,
            |_deadline| {
                Ok(vec![ReconnectCandidate::Eligible(
                    "homeboy-lab".to_string(),
                )])
            },
            |_runner_id, _deadline| Ok(()),
            || {
                Err(homeboy_core::error::Error::internal_unexpected(
                    "refresh exploded",
                ))
            },
        );

        assert!(attempt.attempted);
        assert!(!attempt.succeeded);
        assert!(attempt
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("refresh")));
        assert_eq!(resolved, observed);
    }
}
