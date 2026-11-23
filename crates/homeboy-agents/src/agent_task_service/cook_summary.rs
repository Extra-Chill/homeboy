//! The caller-facing Cook summary (#18817): one projection of a run record
//! that every Cook read surface emits under `cook_summary`.

use homeboy_control_plane_contract::{
    CookSummary, CookSummaryGate, COOK_SUMMARY_FAILING_TEST_LIMIT, COOK_SUMMARY_GATE_LIMIT,
    COOK_SUMMARY_SCHEMA,
};
use serde_json::Value;

use super::cook_delivery::gate_name;
use super::{CookDelivery, CookDeliveryArtifacts, CookDeliveryState};
use crate::agent_task_lifecycle::{
    AgentTaskLifecycleStore, AgentTaskRunRecord, CookRunPhase, CookRunState,
};

/// Project `record` into the stable Cook summary.
pub fn cook_summary(record: &AgentTaskRunRecord) -> CookSummary {
    let state = CookRunState::of(record);
    let delivery = CookDelivery::from_run_metadata(&record.metadata);
    let candidate = delivery
        .as_ref()
        .and_then(|delivery| delivery.candidate.as_ref());
    let publication = delivery
        .as_ref()
        .and_then(|delivery| delivery.publication.as_ref());
    let head_sha = publication
        .and_then(|publication| {
            publication
                .pushed_sha
                .clone()
                .or_else(|| publication.commit.clone())
        })
        .or_else(|| {
            candidate
                .and_then(|candidate| candidate.candidate.as_ref())
                .map(|candidate| candidate.candidate_sha.clone())
        })
        .or_else(|| {
            record
                .candidate
                .as_ref()
                .map(|candidate| candidate.candidate_sha.clone())
        });
    let base_sha = candidate
        .and_then(|candidate| {
            candidate.verified_base.clone().or_else(|| {
                candidate
                    .candidate
                    .as_ref()
                    .map(|candidate| candidate.base_sha.clone())
            })
        })
        .or_else(|| {
            record
                .candidate
                .as_ref()
                .map(|candidate| candidate.base_sha.clone())
        });
    let pr_url = delivery
        .as_ref()
        .and_then(CookDelivery::pr_url)
        .map(str::to_string);
    let why_no_pr = (pr_url.is_none() && state.terminal)
        .then(|| format!("{}: {}", state.status, why_no_pr(&state, delivery.as_ref())));
    CookSummary {
        schema: COOK_SUMMARY_SCHEMA.to_string(),
        status: state.status.clone(),
        phase: state.phase.as_str().to_string(),
        terminal: state.terminal,
        head_sha: head_sha.filter(|sha| !sha.trim().is_empty()),
        base_sha: base_sha.filter(|sha| !sha.trim().is_empty()),
        gates: summary_gates(record, delivery.as_ref()),
        pr_url,
        why_no_pr,
    }
    .bounded()
}

/// The summary of the run `run_id` names, read without initializing or
/// reconciling durable state. `None` when no record is readable.
pub fn cook_summary_for_run(run_id: &str) -> Option<CookSummary> {
    let store = AgentTaskLifecycleStore::from_environment().ok()?;
    crate::agent_task_lifecycle::exact_record_bounded_in_store(&store, run_id)
        .ok()
        .map(|record| cook_summary(&record))
}

/// Why a terminal run has no pull request, decided from its delivery facts
/// and phase. Never empty.
fn why_no_pr(state: &CookRunState, delivery: Option<&CookDelivery>) -> String {
    if state.phase == CookRunPhase::Continuing {
        return "continuation owner is still delivering the candidate".to_string();
    }
    if let Some(delivery) = delivery {
        if let Some(blocker) = &delivery.blocker {
            return format!("{}: {}", blocker.code.as_str(), blocker.message);
        }
        if delivery.is_no_change() {
            return "no change to publish".to_string();
        }
        if let Some(publication) = &delivery.publication {
            return format!(
                "publication receipt recorded status `{}` without a pull request",
                publication.status
            );
        }
        if delivery.state == CookDeliveryState::Applied {
            return "candidate applied; gates have not verified it, publication not attempted"
                .to_string();
        }
    }
    match state.phase {
        CookRunPhase::CandidateRecoverable => "publication not attempted",
        CookRunPhase::Succeeded if delivery.is_some() => {
            "candidate verified; publication not requested"
        }
        CookRunPhase::Succeeded => "no candidate delivery recorded",
        CookRunPhase::PartialFailure | CookRunPhase::Failed => {
            "run ended before a candidate was published"
        }
        CookRunPhase::Cancelled => "run was cancelled before publication",
        CookRunPhase::Unknown => "state written by a newer binary; publication unknown",
        CookRunPhase::Queued | CookRunPhase::Running | CookRunPhase::Continuing => {
            "publication not reached"
        }
    }
    .to_string()
}

fn summary_gates(
    record: &AgentTaskRunRecord,
    delivery: Option<&CookDelivery>,
) -> Vec<CookSummaryGate> {
    let Some(delivery) = delivery else {
        return Vec::new();
    };
    let artifacts = CookDeliveryArtifacts::from_run_metadata(&record.metadata);
    let raw_gates: Vec<&Value> = artifacts
        .promotion
        .as_ref()
        .and_then(|promotion| {
            promotion
                .get("deterministic_gates")
                .or_else(|| promotion.get("gate_results"))
        })
        .and_then(Value::as_array)
        .map(|gates| gates.iter().collect())
        .unwrap_or_default();
    delivery
        .gates
        .iter()
        .take(COOK_SUMMARY_GATE_LIMIT)
        .enumerate()
        .map(|(index, gate)| {
            let name = gate
                .id
                .clone()
                .unwrap_or_else(|| format!("gate-{}", index + 1));
            let raw = raw_gates
                .iter()
                .find(|raw| gate_name(raw).as_deref() == Some(name.as_str()))
                .or_else(|| raw_gates.get(index));
            CookSummaryGate {
                state: serde_json::to_value(gate.state)
                    .ok()
                    .and_then(|state| state.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".to_string()),
                failing_tests: raw.map(|raw| failing_tests(raw)).unwrap_or_default(),
                name,
            }
        })
        .collect()
}

fn failing_tests(gate: &Value) -> Vec<String> {
    gate.pointer("/failure_evidence/failed_tests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|test| test.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .take(COOK_SUMMARY_FAILING_TEST_LIMIT)
        .collect()
}

#[cfg(test)]
#[path = "cook_summary_tests.rs"]
mod tests;
