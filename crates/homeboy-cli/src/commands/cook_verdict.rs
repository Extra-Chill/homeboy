//! The one Cook verdict every read surface projects (#15885).
//!
//! `runs show`, `runs dossier`, `agent-task status` and `agent-task diagnose`
//! used to classify the same Cook independently: `runs show` reported the
//! observation row's `pass`, `runs dossier` derived `stale_running_record`
//! from that same row, `diagnose` printed the raw lifecycle record state
//! (`Succeeded`), and only `agent-task status` read the canonical
//! control-plane projection (`candidate_recoverable`). Four classifiers over
//! one durable record disagreed.
//!
//! The canonical classifier already exists: the control-plane run resource
//! (`homeboy::agents::orchestration`), which applies the Cook observation
//! projection, stale-owner detection and promotion-verification overrides to
//! the durable lifecycle record. This module does not classify anything. It
//! reads that resource and projects it into one operator-facing verdict —
//! state, gate result, why it has not advanced, and the next legal action —
//! so every surface reports the same answer from the same read.

use homeboy::agents::agent_task_service::CookDeliveryBlockerCode;
use homeboy::core::engine::shell::quote_arg;
use homeboy::core::observation::RunRecord;
use homeboy_control_plane_contract::{ControlPlaneBlocker, ControlPlaneRun, ControlPlaneRunState};
use serde::Serialize;
use serde_json::Value;

pub(crate) const COOK_VERDICT_SCHEMA: &str = "homeboy/cook-verdict/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookVerdict {
    pub schema: &'static str,
    /// The durable agent-task run the verdict was read from.
    pub run_id: String,
    /// Canonical control-plane state, e.g. `candidate_recoverable`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<CookVerdictGate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<String>,
    /// Why the Cook has not completed, when it has not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The next legal action, as the same command `agent-task status` prints.
    pub next_action: String,
    pub status_command: String,
    pub diagnose_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookVerdictGate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: String,
}

impl CookVerdict {
    pub(crate) fn is_stale(&self) -> bool {
        self.state == state_label(ControlPlaneRunState::Stale)
    }

    pub(crate) fn has_failed_gate(&self) -> bool {
        self.gates.iter().any(|gate| gate_failed(&gate.state))
    }

    /// Dossier category for this verdict. Agent-task runs are categorized by
    /// their canonical state, never by the observation row's status.
    pub(crate) fn category(&self) -> Option<String> {
        if self.has_failed_gate() || self.candidate.as_deref().is_some_and(candidate_gate_failed) {
            return Some("gate_failure".to_string());
        }
        let category = match self.state.as_str() {
            "stale" => "stale_running_record",
            "candidate_recoverable" | "partial_recoverable" => "candidate_recoverable",
            "failed" | "partial_failure" | "timed_out" => "execution_failure",
            "cancelled" => "cancelled",
            _ => return None,
        };
        Some(category.to_string())
    }
}

/// Project the canonical control-plane run resource into the shared verdict.
pub(crate) fn cook_verdict(run: &ControlPlaneRun) -> CookVerdict {
    let run_id = run.run.as_str().to_string();
    let payload = serde_json::to_value(run).unwrap_or(Value::Null);
    let next_action =
        crate::commands::agent_task_summary::control_plane_next_action(&payload, &run_id);
    let gates = run
        .gates
        .iter()
        .map(|gate| CookVerdictGate {
            id: gate.id.clone(),
            state: gate.state.clone(),
        })
        .collect::<Vec<_>>();
    CookVerdict {
        schema: COOK_VERDICT_SCHEMA,
        state: state_label(run.state),
        phase: run.phase.clone(),
        candidate: run
            .candidate
            .as_ref()
            .map(|candidate| candidate.state.clone()),
        publication: run
            .publication
            .as_ref()
            .map(|publication| publication.state.clone()),
        reason: verdict_reason(run, &gates),
        gates,
        next_action,
        status_command: format!("homeboy agent-task status {}", quote_arg(&run_id)),
        diagnose_command: format!("homeboy agent-task diagnose {}", quote_arg(&run_id)),
        run_id,
    }
}

/// The durable agent-task run an observation row records, if it records one.
pub(crate) fn agent_task_run_id(run: &RunRecord) -> Option<&str> {
    let record = run.metadata_json.get("agent_task_run")?;
    Some(
        record
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|run_id| !run_id.trim().is_empty())
            .unwrap_or(run.id.as_str()),
    )
}

/// The verdict for an observation row backed by a durable agent-task record.
///
/// Reads through the same resolver `agent-task status` uses, so a logical
/// Cook id follows its selected attempt exactly as status does. Rows that are
/// not agent-task runs, or whose record cannot be read, have no verdict and
/// keep their observation status.
pub(crate) fn cook_verdict_for_observation_run(run: &RunRecord) -> Option<CookVerdict> {
    let run_id = agent_task_run_id(run)?;
    homeboy::agents::orchestration::run_from_current_environment(run_id)
        .ok()
        .map(|resource| cook_verdict(&resource))
}

fn state_label(state: ControlPlaneRunState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// A deterministic gate rejected the candidate: the gate status the promotion
/// recorded is the gate contract's own `Failed`, not a re-derived string list.
fn gate_failed(state: &str) -> bool {
    use homeboy::agents::agent_tasks::gate::AgentTaskGateStatus;
    serde_json::from_value::<AgentTaskGateStatus>(Value::String(state.to_string()))
        .is_ok_and(|status| status == AgentTaskGateStatus::Failed)
}

/// The promotion recorded that its deterministic gates rejected the candidate.
fn candidate_gate_failed(state: &str) -> bool {
    state == "gate_failed"
}

fn verdict_reason(run: &ControlPlaneRun, gates: &[CookVerdictGate]) -> Option<String> {
    use ControlPlaneRunState as State;
    if matches!(run.state, State::Succeeded | State::Skipped) {
        return None;
    }
    if let Some(blocker) = run.blocker.as_ref() {
        if let Some(reason) = delivery_blocker_reason(blocker) {
            return Some(reason);
        }
        let message = blocker
            .reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
            .unwrap_or(blocker.message.as_str());
        if !message.trim().is_empty() {
            return Some(match blocker.code.as_deref() {
                Some(code) => format!("{code}: {message}"),
                None => message.to_string(),
            });
        }
    }
    if let Some(gate) = gates.iter().find(|gate| gate_failed(&gate.state)) {
        return Some(format!(
            "deterministic gate `{}` reported `{}`; the candidate was not published",
            gate.id.as_deref().unwrap_or("unnamed"),
            gate.state
        ));
    }
    let candidate = run
        .candidate
        .as_ref()
        .map(|candidate| candidate.state.as_str());
    let publication = run
        .publication
        .as_ref()
        .map(|publication| publication.state.as_str());
    match run.state {
        State::Queued | State::Running | State::Blocked => None,
        State::Stale => Some(
            "the run owner stopped reporting progress; the durable record is stale".to_string(),
        ),
        State::CandidateRecoverable | State::PartialRecoverable => Some(match candidate {
            Some("verification_pending") => {
                "the candidate was promoted but its gates have not verified it; it was not published"
                    .to_string()
            }
            Some(candidate) if candidate_gate_failed(candidate) => format!(
                "candidate promotion reported `{candidate}`; the candidate was not published"
            ),
            Some(candidate) => format!(
                "candidate is `{candidate}` but was not applied, verified and published (publication: {})",
                publication.unwrap_or("not attempted")
            ),
            None => format!(
                "the Cook stopped before publishing (publication: {})",
                publication.unwrap_or("not attempted")
            ),
        }),
        _ => Some(match run.phase.as_deref() {
            Some(phase) => format!("the Cook ended `{}` during `{phase}`", state_label(run.state)),
            None => format!("the Cook ended `{}`", state_label(run.state)),
        }),
    }
}

/// A Cook delivery blocker reads as the step that stopped delivery, then the
/// next legal action the blocker recorded.
fn delivery_blocker_reason(blocker: &ControlPlaneBlocker) -> Option<String> {
    let code =
        serde_json::from_value::<CookDeliveryBlockerCode>(Value::String(blocker.code.clone()?))
            .ok()?;
    let mut reason = format!("{}: {}", code.as_str(), blocker.message);
    if let Some(next) = blocker
        .reason
        .as_deref()
        .filter(|next| !next.trim().is_empty())
    {
        reason.push_str(&format!("; next: {next}"));
    }
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::{ControlPlaneStateSummary, RunId};

    fn resource(state: ControlPlaneRunState) -> ControlPlaneRun {
        let mut run = ControlPlaneRun::new(RunId::new("agent-task-verdict").expect("run id"));
        run.state = state;
        run
    }

    #[test]
    fn verification_pending_candidate_explains_why_it_was_not_published() {
        let mut run = resource(ControlPlaneRunState::CandidateRecoverable);
        run.candidate = Some(ControlPlaneStateSummary {
            id: None,
            state: "verification_pending".to_string(),
        });
        let verdict = cook_verdict(&run);
        assert_eq!(verdict.state, "candidate_recoverable");
        assert_eq!(verdict.category().as_deref(), Some("candidate_recoverable"));
        assert!(verdict
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not published")));
        assert_eq!(
            verdict.next_action,
            "homeboy agent-task cook-continue agent-task-verdict"
        );
    }

    #[test]
    fn failed_gate_is_the_reason_and_the_category() {
        let mut run = resource(ControlPlaneRunState::CandidateRecoverable);
        run.candidate = Some(ControlPlaneStateSummary {
            id: None,
            state: "gate_failed".to_string(),
        });
        run.gates = vec![ControlPlaneStateSummary {
            id: Some("cargo-test".to_string()),
            state: "failed".to_string(),
        }];
        let verdict = cook_verdict(&run);
        assert_eq!(verdict.category().as_deref(), Some("gate_failure"));
        assert!(verdict
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("cargo-test")));
    }

    #[test]
    fn succeeded_cook_has_no_reason_and_no_category() {
        let verdict = cook_verdict(&resource(ControlPlaneRunState::Succeeded));
        assert_eq!(verdict.state, "succeeded");
        assert_eq!(verdict.reason, None);
        assert_eq!(verdict.category(), None);
        assert!(!verdict.is_stale());
    }

    #[test]
    fn stale_verdict_keeps_the_stale_running_record_category() {
        let verdict = cook_verdict(&resource(ControlPlaneRunState::Stale));
        assert!(verdict.is_stale());
        assert_eq!(verdict.category().as_deref(), Some("stale_running_record"));
    }
}
