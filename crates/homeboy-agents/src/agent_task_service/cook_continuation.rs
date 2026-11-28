//! Engine-owned Cook continuation work-job orchestration and failure reporting.

use super::CookRecipeStore;
use homeboy_core::{Error, Result};
use std::time::Duration;

pub fn run_terminal_cook_continuation(
    store: &CookRecipeStore,
    cook_id: &str,
    run_id: &str,
    dispatcher: &super::CookDispatcherResolver<'_>,
    execute: impl FnOnce(&str, &str) -> Result<(i32, super::CookDisposition, Option<serde_json::Value>)>,
    checkpoint: serde_json::Value,
    progress: serde_json::Value,
) -> Result<super::WorkJobStep> {
    use super::{CookContinuationState, ScheduledContinuationClaim, WorkJobStep};

    let claim = match store.claim_scheduled_continuation_for(cook_id, run_id)? {
        ScheduledContinuationClaim::Claimed(claim) => *claim,
        // A retryable failure spaced its next automatic attempt. Keep this job
        // supervising the durable continuation until the deadline passes
        // instead of failing it, which would leave the candidate stranded
        // (#15831).
        ScheduledContinuationClaim::RetryAfter(wait) => {
            return Ok(WorkJobStep::Continue {
                checkpoint,
                progress,
                wait: wait.min(TERMINAL_COOK_RETRY_POLL_MAX),
            });
        }
        ScheduledContinuationClaim::Unavailable => {
            // A second consumer can own the continuation while this WorkJob is
            // recovered. Only a completed claim is a completed job; an active
            // claim must remain supervised until that consumer settles its
            // durable state.
            let state = super::continuation_state_in_store(store, cook_id, run_id)?;
            return match state {
                CookContinuationState::Pending | CookContinuationState::Claimed => {
                    Ok(WorkJobStep::Continue {
                        checkpoint,
                        progress,
                        wait: Duration::from_millis(250),
                    })
                }
                CookContinuationState::Completed => Ok(WorkJobStep::Complete(
                    serde_json::json!({ "claimed": false, "completed": true }),
                )),
                CookContinuationState::Failed => {
                    announce_failed_terminal_continuation(store, cook_id, run_id);
                    Err(Error::internal_unexpected(format!(
                        "terminal Cook continuation is {state:?} without a claim"
                    )))
                }
                CookContinuationState::Absent => Err(Error::internal_unexpected(format!(
                    "terminal Cook continuation is {state:?} without a claim"
                ))),
            };
        }
    };
    let mut pass_failure = None;
    let outcome = store.consume_claimed_with_dispatcher(claim, dispatcher, |cook_id, run_id| {
        execute(cook_id, run_id).map(|(code, disposition, failure)| {
            pass_failure = failure;
            (code, disposition)
        })
    });
    let failure = match outcome {
        Ok(0) => {
            if super::continuation_state_in_store(store, cook_id, run_id)?
                != CookContinuationState::Completed
            {
                return Ok(WorkJobStep::Continue {
                    checkpoint,
                    progress,
                    wait: Duration::from_secs(1),
                });
            }
            return Ok(WorkJobStep::Complete(serde_json::json!({
                "claimed": true,
                "cook_id": cook_id,
                "run_id": run_id,
                "exit_code": 0,
            })));
        }
        Ok(exit_code) => terminal_continuation_exit_error(exit_code, pass_failure),
        Err(error) => error,
    };
    let state = super::continuation_state_in_store(store, cook_id, run_id)?;
    terminal_continuation_failure_step(state, failure, || {
        announce_failed_terminal_continuation(store, cook_id, run_id);
    })
}

/// The failure of a Cook pass that exited nonzero, naming the cause its report
/// declared. Without one the message still carries the exit status.
fn terminal_continuation_exit_error(exit_code: i32, failure: Option<serde_json::Value>) -> Error {
    let cause = failure
        .as_ref()
        .and_then(|failure| failure["message"].as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let mut error = Error::internal_unexpected(match cause {
        Some(cause) => {
            format!("terminal Cook continuation exited with status {exit_code}: {cause}")
        }
        None => format!("terminal Cook continuation exited with status {exit_code}"),
    });
    error.details["exit_code"] = serde_json::json!(exit_code);
    if let Some(failure) = failure {
        error.details["failure"] = failure;
    }
    error
}

/// Upper bound on one supervised wait while a continuation retry is backed
/// off, so cancellation stays responsive during a long deadline.
pub const TERMINAL_COOK_RETRY_POLL_MAX: Duration = Duration::from_secs(5);

/// Settle this work job after its claimed continuation did not finish.
///
/// The consumer has already transitioned the durable continuation. A retryable
/// failure leaves it `pending` under a new generation with a spaced retry
/// deadline, and that generation derives its own work intent, so this job
/// completes and hands the retry over. Failing it instead surfaced as an opaque
/// `ValidationInvalidArgument` work job while the Cook silently stayed
/// `candidate_recoverable` (#15831). A terminally failed continuation is
/// announced before the job fails, so the stop is never silent.
fn terminal_continuation_failure_step(
    state: super::CookContinuationState,
    failure: Error,
    announce_failed: impl FnOnce(),
) -> Result<super::WorkJobStep> {
    use super::{CookContinuationState, WorkJobStep};

    match state {
        CookContinuationState::Pending => Ok(WorkJobStep::Complete(serde_json::json!({
            "claimed": true,
            "retry_scheduled": true,
            "error_code": failure.code.as_str(),
            "message": failure.message,
        }))),
        CookContinuationState::Failed => {
            announce_failed();
            Err(failure)
        }
        _ => Err(failure),
    }
}

pub fn announce_failed_terminal_continuation(
    store: &super::CookRecipeStore,
    cook_id: &str,
    run_id: &str,
) {
    let diagnostic = super::failed_continuation_diagnostic_in_store(store, run_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| "cook continuation failed".to_string());
    crate::agent_task_notify::cook_continuation_failed(cook_id, run_id, &diagnostic);
}

/// Observe a worker's durable handoff without spawning a competing consumer.
pub fn terminal_cook_supervision_step(
    store: &CookRecipeStore,
    cook_id: &str,
    run_id: &str,
) -> Result<Option<super::WorkJobStep>> {
    use super::{CookContinuationState, ScheduledContinuationGate, WorkJobStep};
    let wait = match store.scheduled_continuation_gate(cook_id, run_id) {
        ScheduledContinuationGate::RetryAfter(wait) => Some(wait.min(TERMINAL_COOK_RETRY_POLL_MAX)),
        // A worker spawned before its checkpoint was persisted (or another
        // consumer) owns the claim. Observe; never start a second consumer.
        ScheduledContinuationGate::HeldByLiveOwner => Some(Duration::from_secs(1)),
        ScheduledContinuationGate::Settled(CookContinuationState::Completed) => {
            return Ok(Some(WorkJobStep::Complete(
                serde_json::json!({ "claimed": false, "completed": true }),
            )));
        }
        ScheduledContinuationGate::Settled(state) => {
            announce_failed_terminal_continuation(store, cook_id, run_id);
            return Err(Error::internal_unexpected(format!(
                "terminal Cook continuation is {state:?} without a claim"
            )));
        }
        ScheduledContinuationGate::Ready => None,
    };
    Ok(wait.map(|wait| WorkJobStep::Continue {
        checkpoint: serde_json::Value::Null,
        progress: serde_json::Value::Null,
        wait,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_continuation_exit_names_the_cause_its_cook_pass_declared() {
        let report = serde_json::json!({
            "status": "failed",
            "failure": {
                "code": "agent_task.gates_failed",
                "message": "deterministic gate `cargo test` failed",
            },
        });
        let failure = Some(report["failure"].clone());
        let error = terminal_continuation_exit_error(1, failure);
        assert_eq!(
        error.message,
        "terminal Cook continuation exited with status 1: deterministic gate `cargo test` failed"
    );
        assert_eq!(error.details["failure"]["code"], "agent_task.gates_failed");
        assert_eq!(error.details["exit_code"], 1);

        let bare = terminal_continuation_exit_error(1, None);
        assert_eq!(
            bare.message,
            "terminal Cook continuation exited with status 1"
        );
    }

    /// A claimed terminal continuation that failed retryably is handed to its
    /// next generation instead of failing the work job, and a terminally failed
    /// continuation is announced before the job fails (#15831).
    #[test]
    fn terminal_continuation_failure_step_hands_off_retries_and_announces_failure() {
        use super::super::{CookContinuationState, WorkJobStep};

        let failure = || {
            Error::validation_invalid_argument(
                "cook_continuation.artifact_projection",
                "artifact projection is not ready",
                None,
                None,
            )
            .with_retryable(true)
        };

        let mut announced = false;
        let step =
            terminal_continuation_failure_step(CookContinuationState::Pending, failure(), || {
                announced = true
            })
            .expect("a pending retry completes this job");
        assert!(matches!(
            step,
            WorkJobStep::Complete(ref result)
                if result["retry_scheduled"] == true
                    && result["error_code"] == "validation.invalid_argument"
        ));
        assert!(!announced, "a scheduled retry is not a terminal stop");

        let mut announced = false;
        let Err(error) =
            terminal_continuation_failure_step(CookContinuationState::Failed, failure(), || {
                announced = true
            })
        else {
            panic!("a failed continuation fails the job");
        };
        assert!(error.message.contains("artifact projection is not ready"));
        assert!(
            announced,
            "a terminal continuation failure must be announced"
        );

        let mut announced = false;
        assert!(
            terminal_continuation_failure_step(CookContinuationState::Claimed, failure(), || {
                announced = true
            })
            .is_err(),
            "an unexpected state still fails the job"
        );
        assert!(!announced);
    }
}
