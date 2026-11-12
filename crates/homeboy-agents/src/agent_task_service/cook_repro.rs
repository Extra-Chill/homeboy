//! Cook repro gates (#17653).
//!
//! A repro gate is a command that must fail on the Cook's pinned base before
//! any provider work starts, and pass on the candidate. Without one, green
//! gates only show the candidate did not break them, not that it fixed the
//! issue. Cook runs each repro against a detached checkout of the base using
//! the same gate runner and placement as its candidate gates, records the
//! result on the initial run, and stops with `repro_passes_on_base` when a
//! repro does not reproduce the issue. Each repro also runs as a visible
//! candidate gate, and the review dossier records whether it proved the fix.

use serde::{Deserialize, Serialize};

use super::cook::CookRequest;
use crate::agent_task_gate::{
    run_gate_command_with_timeout, AgentTaskGateReport, AgentTaskGateRevealPolicy,
    AgentTaskGateStatus, AgentTaskGateVisibility,
};
use crate::agent_task_lifecycle::AgentTaskLifecycleStore;
use crate::agent_task_promotion::AgentTaskPromotionReport;
use crate::agent_task_review_dossier::AgentTaskReviewRepro;
use homeboy_core::observation::store::coordination::{
    ExpectedRevision, RecordOutcome, RecordWrite,
};
use homeboy_core::{Error, Result};

/// Run metadata key mirroring [`CookReproBaseEvidence`] on the Cook's initial run.
pub(crate) const COOK_REPRO_BASE_METADATA: &str = "cook_repro_base";
const COOK_REPRO_BASE_SCHEMA: &str = "homeboy/agent-task-cook-repro-base/v1";

/// The typed Cook outcome for a repro that passes on its base.
pub(crate) const REPRO_PASSES_ON_BASE: &str = "repro_passes_on_base";

/// Durable evidence of each repro gate's run against the pinned base.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CookReproBaseEvidence {
    pub schema: String,
    pub base_sha: String,
    pub results: Vec<CookReproBaseResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CookReproBaseResult {
    pub command: String,
    pub exit_code: i32,
    pub status: AgentTaskGateStatus,
    /// True when the repro failed on base, i.e. it reproduces the issue.
    pub failed_on_base: bool,
    /// The full gate report from the shared gate runner.
    pub gate: AgentTaskGateReport,
}

impl CookReproBaseEvidence {
    /// Every declared repro failed on base.
    pub fn reproduces(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|result| result.failed_on_base)
    }

    /// The repro commands that passed on base and so do not reproduce the issue.
    pub fn passing_on_base(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|result| !result.failed_on_base)
            .map(|result| result.command.as_str())
            .collect()
    }
}

/// Run every declared repro gate against a detached checkout of `base_sha`.
pub(crate) fn run_repro_on_base(
    options: &CookRequest,
    base_sha: &str,
) -> Result<CookReproBaseEvidence> {
    let repository_root = options
        .workspace
        .source_worktree_path
        .clone()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "repro",
                "--repro needs a controller-local source checkout to run against the Cook base before dispatch",
                Some(options.workspace.to_worktree.clone()),
                None,
            )
        })?;
    let gate_workspace = super::cook_promotion::component_workspace_path(options)?
        .unwrap_or_else(|| repository_root.clone());
    // Same gate policy and placement the candidate gates use.
    let gates = super::cook_recipe::admitted_gate_policy(options)?;
    let mut environment = gates.gate_environment.clone();
    if environment.admitted_component_id.is_none() {
        environment.admitted_component_id =
            super::cook::cook_repository_identity_component_id(&options.identity.initial_plan);
    }
    let component_id = environment.admitted_component_id.clone();
    let results = crate::agent_task_promotion::with_gate_supervision(
        gates.declared_supervision(),
        || {
            super::cook_baseline::with_detached_base_checkout(
                &repository_root,
                &gate_workspace,
                base_sha,
                component_id.as_deref(),
                |base_workspace| {
                    super::cook_baseline::hydrate_base_gate_workspace(
                        base_workspace,
                        &environment,
                        component_id.as_deref(),
                        &gates.repro,
                        gates.gate_timeout(),
                    )?;
                    gates
                        .repro
                        .iter()
                        .enumerate()
                        .map(|(index, command)| {
                            let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
                            let gate = (|| {
                                let runtime =
                                    homeboy_core::engine::invocation::InvocationGuard::acquire(
                                        &run_dir,
                                        &homeboy_core::engine::invocation::InvocationRequirements::default(),
                                    )?;
                                run_gate_command_with_timeout(
                                    base_workspace,
                                    index + 1,
                                    command,
                                    AgentTaskGateVisibility::Visible,
                                    AgentTaskGateRevealPolicy::FullEvidence,
                                    &runtime.context().tmp_dir,
                                    gates.gate_timeout(),
                                    &environment,
                                    &gates.gate_package_artifacts,
                                )
                            })();
                            run_dir.finish(gate.is_ok());
                            repro_base_result(command, gate?)
                        })
                        .collect::<Result<Vec<_>>>()
                },
            )
        },
    )?;
    Ok(CookReproBaseEvidence {
        schema: COOK_REPRO_BASE_SCHEMA.to_string(),
        base_sha: base_sha.to_string(),
        results,
    })
}

/// A repro proves the issue only by running: a pass means it does not
/// reproduce, a failure means it does. A gate that produced no evidence
/// (unavailable runner, deferred placement, skipped) is an infrastructure
/// failure that names its cause, never a pass on base.
fn repro_base_result(command: &str, gate: AgentTaskGateReport) -> Result<CookReproBaseResult> {
    let failed_on_base = match gate.status {
        AgentTaskGateStatus::Failed => true,
        AgentTaskGateStatus::Succeeded => false,
        status => {
            return Err(Error::internal_unexpected(format!(
                "repro `{command}` did not run on base (gate {status:?}): {}",
                gate.stderr.trim()
            )))
        }
    };
    Ok(CookReproBaseResult {
        command: command.to_string(),
        exit_code: gate.exit_code,
        status: gate.status,
        failed_on_base,
        gate,
    })
}

/// Durable per-Cook evidence store, so the record survives attempt dispatch.
const COOK_REPRO_BASE_RECORDS: &str = "cook-repro-base-v1";

/// Record the base repro result on the Cook and, as operator-visible
/// evidence, on its initial run.
pub(crate) fn record_repro_base_evidence(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
    evidence: &CookReproBaseEvidence,
) -> Result<()> {
    let payload = serde_json::to_value(evidence).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize Cook repro base evidence".to_string()),
        )
    })?;
    let store = lifecycle_store.open_observation_initialized()?;
    let expected = match store.coordination_record(COOK_REPRO_BASE_RECORDS, cook_id)? {
        Some(record) => ExpectedRevision::Revision(record.revision),
        None => ExpectedRevision::Absent,
    };
    let state = if evidence.reproduces() {
        "reproduces"
    } else {
        REPRO_PASSES_ON_BASE
    };
    if let RecordOutcome::Conflict { .. } = store.put_coordination_record(
        COOK_REPRO_BASE_RECORDS,
        cook_id,
        expected,
        RecordWrite {
            state,
            payload: &payload,
            expires_at_ms: None,
        },
    )? {
        return Err(Error::internal_unexpected(format!(
            "Cook {cook_id} repro base evidence changed concurrently"
        )));
    }
    lifecycle_store.record_metadata_value(run_id, COOK_REPRO_BASE_METADATA, payload)
}

pub(crate) fn recorded_repro_base_evidence(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Option<CookReproBaseEvidence> {
    let record = lifecycle_store
        .open_observation_initialized()
        .ok()?
        .coordination_record(COOK_REPRO_BASE_RECORDS, cook_id)
        .ok()??;
    serde_json::from_value(record.payload).ok()
}

/// The operator-facing stop reason, including the next action.
pub(crate) fn repro_passes_on_base_stop_reason(evidence: &CookReproBaseEvidence) -> String {
    format!(
        "repro gate passed on base `{}` before dispatch, so it does not reproduce the issue: {}. Next action: change --repro/--repro-file to a command that fails on this base and passes once the issue is fixed, then start Cook again",
        evidence.base_sha,
        evidence
            .passing_on_base()
            .iter()
            .map(|command| format!("`{command}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Whether the candidate is repro-verified: every repro failed on base and
/// passed, as a visible candidate-bound gate, on the promoted candidate.
pub(crate) fn candidate_repro_verdict(
    repro: &[String],
    base: Option<&CookReproBaseEvidence>,
    promotion: &AgentTaskPromotionReport,
) -> AgentTaskReviewRepro {
    if repro.is_empty() {
        return AgentTaskReviewRepro::None;
    }
    let failed_on_base = base.is_some_and(|base| {
        base.reproduces()
            && repro.iter().all(|command| {
                base.results
                    .iter()
                    .any(|result| &result.command == command && result.failed_on_base)
            })
    });
    let passed_on_candidate = repro
        .iter()
        .all(|command| promotion.has_visible_passed_gate_for_command(command));
    if failed_on_base && passed_on_candidate {
        AgentTaskReviewRepro::Verified
    } else {
        AgentTaskReviewRepro::FailedOnCandidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_repro_that_ran_decides_whether_base_reproduces() {
        for (status, expected) in [
            ("failed", Some(true)),
            ("succeeded", Some(false)),
            ("unavailable", None),
            ("deferred", None),
            ("skipped", None),
        ] {
            let gate: AgentTaskGateReport = serde_json::from_value(serde_json::json!({
                "id": "gate-1",
                "status": status,
                "command": ["sh", "-c", "repro"],
                "exit_code": 0,
                "stdout": "",
                "stderr": "could not reach runner daemon",
            }))
            .expect("gate report");
            match (repro_base_result("repro", gate), expected) {
                (Ok(result), Some(failed)) => assert_eq!(result.failed_on_base, failed, "{status}"),
                (Err(error), None) => {
                    assert!(error.message.contains("did not run on base"), "{status}");
                    assert!(
                        error.message.contains("could not reach runner daemon"),
                        "{status}"
                    );
                }
                (other, _) => panic!("{status}: {other:?}"),
            }
        }
    }
}
