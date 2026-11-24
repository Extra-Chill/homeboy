//! The caller-facing Cook summary (#18817): one projection of a run record
//! that every Cook read surface emits under `cook_summary`. Status, phase,
//! terminality and `why_no_pr` are projections of [`CookRunState::of`] and
//! [`legal_actions`]; this module never classifies the run itself (#18499).

use homeboy_control_plane_contract::{
    ControlPlaneAction, CookSummary, CookSummaryBenchCompare, CookSummaryGate,
    COOK_SUMMARY_FAILING_TEST_LIMIT, COOK_SUMMARY_GATE_LIMIT, COOK_SUMMARY_SCHEMA,
};
use serde_json::Value;

use super::cook_delivery::gate_name;
use super::{CookDelivery, CookDeliveryArtifacts};
use crate::agent_task_lifecycle::{
    legal_actions, AgentTaskLifecycleStore, AgentTaskRunRecord, CookRunState,
};

/// Project `record` into the stable Cook summary, with one accounting line
/// per attempt of its Cook read from the ambient durable Cook index. Callers
/// that own a lifecycle store use [`cook_summary_in_store`].
pub fn cook_summary(record: &AgentTaskRunRecord) -> CookSummary {
    match AgentTaskLifecycleStore::from_environment() {
        Ok(store) => cook_summary_in_store(&store, record),
        Err(_) => cook_summary_with_attempts(record, &[]),
    }
}

/// Project `record` into the stable Cook summary, reading its Cook's attempt
/// history from the Cook index in `store`.
pub(crate) fn cook_summary_in_store(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
) -> CookSummary {
    cook_summary_with_attempts(record, &cook_attempt_records(store, record))
}

/// Project `record` with the Cook's attempt records, oldest first. With no
/// attempt history, `record` stands as its Cook's only attempt.
pub(crate) fn cook_summary_with_attempts(
    record: &AgentTaskRunRecord,
    attempts: &[(u32, AgentTaskRunRecord)],
) -> CookSummary {
    let mut summary = project_summary(record);
    summary.attempts = if attempts.is_empty() {
        let attempt = record
            .metadata
            .get("cook_attempt")
            .and_then(Value::as_u64)
            .and_then(|attempt| u32::try_from(attempt).ok())
            .unwrap_or(1);
        vec![attempt_line(attempt, record, true)]
    } else {
        attempts
            .iter()
            .enumerate()
            .map(|(index, (attempt, attempt_record))| {
                attempt_line(*attempt, attempt_record, index + 1 == attempts.len())
            })
            .collect()
    };
    summary.bounded()
}

/// Every attempt the Cook index of `record`'s Cook names, oldest first.
/// `record` stands in for its own attempt; unreadable attempts are skipped.
fn cook_attempt_records(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
) -> Vec<(u32, AgentTaskRunRecord)> {
    let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(index) = crate::agent_task_lifecycle::cook_index_in_store(store, cook_id) else {
        return Vec::new();
    };
    let mut entries = index.attempts;
    entries.sort_by_key(|entry| entry.attempt);
    entries
        .into_iter()
        .filter_map(|entry| {
            let attempt_record = if entry.run_id == record.run_id {
                record.clone()
            } else {
                crate::agent_task_lifecycle::exact_record_bounded_in_store(store, &entry.run_id)
                    .ok()?
            };
            Some((entry.attempt, attempt_record))
        })
        .collect()
}

/// Run-record metadata key holding the agent's own reason for an attempt that
/// produced no candidate (#18916).
pub(crate) const NO_CHANGE_REASON_KEY: &str = "agent_no_change_reason";

/// Longest no-change reason, in characters, an attempt line carries.
const ATTEMPT_REASON_LIMIT: usize = 160;

/// `reason` collapsed to one line and bounded to [`ATTEMPT_REASON_LIMIT`]
/// characters, or `None` when it is blank.
pub(crate) fn bounded_attempt_reason(reason: &str) -> Option<String> {
    let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if reason.is_empty() {
        return None;
    }
    if reason.chars().count() <= ATTEMPT_REASON_LIMIT {
        return Some(reason);
    }
    let mut bounded = reason
        .chars()
        .take(ATTEMPT_REASON_LIMIT - 1)
        .collect::<String>();
    bounded.push('…');
    Some(bounded)
}

/// `attempt N: <candidate sha> · gates <verdicts> · <disposition>`, with the
/// agent's bounded reason appended (`<disposition>: <reason>`) when the
/// attempt produced no candidate.
fn attempt_line(attempt: u32, record: &AgentTaskRunRecord, latest: bool) -> String {
    let delivery = CookDelivery::from_run_metadata(&record.metadata);
    let candidate_sha = delivery
        .as_ref()
        .and_then(|delivery| delivery.candidate.as_ref())
        .and_then(|candidate| candidate.candidate.as_ref())
        .map(|candidate| candidate.candidate_sha.clone())
        .or_else(|| {
            record
                .candidate
                .as_ref()
                .map(|candidate| candidate.candidate_sha.clone())
        })
        .filter(|sha| !sha.trim().is_empty());
    let gates = summary_gates(record, delivery.as_ref())
        .into_iter()
        .map(|gate| format!("{}={}", gate.name, gate.state))
        .collect::<Vec<_>>();
    let gates = if gates.is_empty() {
        "none".to_string()
    } else {
        gates.join(", ")
    };
    let state = CookRunState::of(record);
    let disposition = if delivery.as_ref().and_then(CookDelivery::pr_url).is_some() {
        "published".to_string()
    } else if !latest {
        if candidate_sha.is_some() {
            "superseded"
        } else {
            "discarded"
        }
        .to_string()
    } else if candidate_sha.is_none() && state.terminal {
        "discarded".to_string()
    } else {
        state.status.to_string()
    };
    let sha = candidate_sha
        .map(|sha| sha.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "none".to_string());
    let reason = (sha == "none")
        .then(|| {
            record
                .metadata
                .get(NO_CHANGE_REASON_KEY)
                .and_then(Value::as_str)
                .and_then(bounded_attempt_reason)
        })
        .flatten()
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    format!("attempt {attempt}: {sha} · gates {gates} · {disposition}{reason}")
}

fn project_summary(record: &AgentTaskRunRecord) -> CookSummary {
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
    let why_no_pr = pr_url
        .is_none()
        .then(|| why_no_pr(&state, delivery.as_ref()))
        .flatten();
    CookSummary {
        schema: COOK_SUMMARY_SCHEMA.to_string(),
        status: state.status.to_string(),
        phase: state.phase.as_str().to_string(),
        terminal: state.terminal,
        head_sha: head_sha.filter(|sha| !sha.trim().is_empty()),
        base_sha: base_sha.filter(|sha| !sha.trim().is_empty()),
        gates: summary_gates(record, delivery.as_ref()),
        attempts: Vec::new(),
        pr_url,
        why_no_pr,
        delivery: publication
            .map(|publication| publication.delivery.clone())
            .unwrap_or_default(),
    }
}

/// The summary of the run `run_id` names, read without initializing or
/// reconciling durable state. `None` when no record is readable.
pub fn cook_summary_for_run(run_id: &str) -> Option<CookSummary> {
    let store = AgentTaskLifecycleStore::from_environment().ok()?;
    crate::agent_task_lifecycle::exact_record_bounded_in_store(&store, run_id)
        .ok()
        .map(|record| cook_summary_in_store(&store, &record))
}

/// Why a terminal run has no pull request: the reason [`CookRunState`] owns,
/// refined by the delivery blocker that stopped it and the recovery action
/// [`legal_actions`] makes available. `None` while the run is in flight.
fn why_no_pr(state: &CookRunState, delivery: Option<&CookDelivery>) -> Option<String> {
    let reason = state.no_pull_request_reason()?;
    let mut why = match delivery.and_then(|delivery| delivery.blocker.as_ref()) {
        Some(blocker) => format!(
            "{}: {}: {}",
            state.status,
            blocker.code.as_str(),
            blocker.message
        ),
        None => format!("{}: {reason}", state.status),
    };
    if legal_actions(state).contains(&ControlPlaneAction::Recover) {
        why.push_str(" (recover available)");
    }
    Some(why)
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
                bench_compare: raw.and_then(|raw| bench_compare(raw)),
                name,
            }
        })
        .collect()
}

/// A bench gate's verdict and artifact, from the evidence its gate report
/// carries (#18869).
fn bench_compare(gate: &Value) -> Option<CookSummaryBenchCompare> {
    let evidence: homeboy_core::bench_compare::BenchCompareGateEvidence =
        serde_json::from_value(gate.get("bench_compare")?.clone()).ok()?;
    Some(CookSummaryBenchCompare {
        passed: evidence.passed,
        fail_on: evidence.fail_on.as_str().to_string(),
        routes: evidence.summary.routes,
        regressed: evidence.summary.regressed,
        improved: evidence.summary.improved,
        new_blockers: evidence.summary.new_blockers,
        missing_on_head: evidence.summary.missing_on_head,
        artifact_path: evidence.artifact_path,
    })
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
