//! Per-attempt Cook timeline for `agent-task status` (#17893).
//!
//! Derived read-only from what each attempt already recorded: its phase spans,
//! classified by the shared Cook usage buckets (the same classification
//! `runs show` / `runs profile` use), and its persisted gate reports. A field
//! with no recorded data is absent, never zero.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_gate::phases::{
    format_minutes, slowest_gate_phase, AgentTaskGatePhase, MAX_GATE_PHASES,
};
use crate::agent_task_lifecycle::AgentTaskLifecycleStore;
use homeboy_core::Result;

/// One Cook attempt's wall time by stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookAttemptTiming {
    pub attempt: u32,
    /// Admission and staging before the agent ran (the `setup` usage bucket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_ms: Option<u64>,
    /// The `agent` usage bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_ms: Option<u64>,
    /// Executed gate commands from the attempt's gate reports, else the
    /// `verify` usage bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_ms: Option<u64>,
    /// The slowest phase any of the attempt's gates reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_gate_phase: Option<AgentTaskGatePhase>,
    /// The `finalize` usage bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalize_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl CookAttemptTiming {
    /// `attempt 2: queued 0.4m · agent 3.6m · gate 4.8m (slowest: acceptance 3.6m) · finalize 1.1m → passed`
    pub fn summary_line(&self) -> String {
        let mut parts = Vec::new();
        for (label, ms) in [("queued", self.queued_ms), ("agent", self.agent_ms)] {
            if let Some(ms) = ms {
                parts.push(format!("{label} {}", format_minutes(ms)));
            }
        }
        if let Some(ms) = self.gate_ms {
            let slowest = self
                .slowest_gate_phase
                .as_ref()
                .map(|phase| {
                    format!(
                        " (slowest: {} {})",
                        phase.name,
                        format_minutes(phase.elapsed_ms)
                    )
                })
                .unwrap_or_default();
            parts.push(format!("gate {}{slowest}", format_minutes(ms)));
        }
        if let Some(ms) = self.finalize_ms {
            parts.push(format!("finalize {}", format_minutes(ms)));
        }
        let mut line = format!("attempt {}:", self.attempt);
        if parts.is_empty() {
            line.push_str(" no timing recorded");
        } else {
            line.push(' ');
            line.push_str(&parts.join(" · "));
        }
        if let Some(outcome) = &self.outcome {
            line.push_str(&format!(" → {outcome}"));
        }
        line
    }
}

/// Every recorded attempt of `cook_id`, in attempt order. Empty when the id
/// names no Cook. Opens the stores read-only and writes nothing.
pub fn cook_attempt_timeline_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<Vec<CookAttemptTiming>> {
    let Ok(index) = store.read_cook_index(cook_id) else {
        return Ok(Vec::new());
    };
    let mut attempts = index.attempts;
    attempts.sort_by_key(|attempt| attempt.attempt);
    // A re-registered attempt keeps its latest run.
    attempts.dedup_by(|later, earlier| {
        if later.attempt == earlier.attempt {
            std::mem::swap(later, earlier);
            true
        } else {
            false
        }
    });
    let observation = store.open_observation_readonly()?;
    attempts
        .iter()
        .map(|attempt| {
            let spans = observation.phase_spans(&attempt.run_id)?.spans;
            let record = store.read_record_bounded(&attempt.run_id).ok();
            let promotion = record.as_ref().and_then(|record| {
                super::recorded_promotion_report(&record.metadata)
                    .ok()
                    .flatten()
            });
            Ok(attempt_timing(
                attempt.attempt,
                &spans,
                promotion.as_ref(),
                record.as_ref().map(record_state),
            ))
        })
        .collect()
}

/// [`cook_attempt_timeline_in_store`] against the environment's stores.
pub fn cook_attempt_timeline(cook_id: &str) -> Result<Vec<CookAttemptTiming>> {
    cook_attempt_timeline_in_store(&AgentTaskLifecycleStore::from_environment()?, cook_id)
}

fn record_state(record: &crate::agent_task_lifecycle::AgentTaskRunRecord) -> String {
    serde_json::to_value(record.state())
        .ok()
        .and_then(|state| state.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn attempt_timing(
    attempt: u32,
    spans: &[homeboy_engine_primitives::phase_timing::PhaseSpan],
    promotion: Option<&Value>,
    state: Option<String>,
) -> CookAttemptTiming {
    let mut buckets = std::collections::BTreeMap::<&str, f64>::new();
    for usage in homeboy_core::performance_hotspots::usage_buckets(spans) {
        for bucket in usage.buckets {
            *buckets.entry(bucket.bucket).or_default() += bucket.wall_ms;
        }
    }
    let bucket_ms = |name: &str| {
        buckets
            .get(name)
            .filter(|ms| **ms > 0.0)
            .map(|ms| ms.round() as u64)
    };
    let gates: Vec<&Value> = promotion
        .and_then(|report| report.get("deterministic_gates"))
        .and_then(Value::as_array)
        .map(|gates| gates.iter().collect())
        .unwrap_or_default();
    // A reused verdict ran in an earlier attempt; this attempt spent nothing.
    let executed_gate_ms = gates
        .iter()
        .filter_map(|gate| gate.get("verdict"))
        .filter(|verdict| verdict["reused"] != Value::Bool(true))
        .filter_map(|verdict| verdict["elapsed_ms"].as_u64())
        .reduce(|total, ms| total + ms);
    let phases: Vec<AgentTaskGatePhase> = gates
        .iter()
        .filter_map(|gate| {
            serde_json::from_value::<Vec<AgentTaskGatePhase>>(gate.get("phases")?.clone()).ok()
        })
        .flat_map(|phases| phases.into_iter().take(MAX_GATE_PHASES))
        .collect();
    CookAttemptTiming {
        attempt,
        queued_ms: bucket_ms("setup"),
        agent_ms: bucket_ms("agent"),
        gate_ms: executed_gate_ms.or_else(|| bucket_ms("verify")),
        slowest_gate_phase: slowest_gate_phase(&phases).cloned(),
        finalize_ms: bucket_ms("finalize"),
        outcome: gate_outcome(&gates).or(state),
    }
}

/// The attempt's gate verdict, when it reached its gates.
fn gate_outcome(gates: &[&Value]) -> Option<String> {
    let statuses: Vec<&str> = gates
        .iter()
        .filter_map(|gate| gate["status"].as_str())
        .collect();
    if statuses.is_empty() {
        return None;
    }
    let outcome = if statuses
        .iter()
        .any(|status| matches!(*status, "failed" | "accepted_inherited_failure"))
    {
        "gate_failed"
    } else if statuses
        .iter()
        .any(|status| matches!(*status, "unavailable" | "deferred"))
    {
        "gate_unavailable"
    } else {
        "passed"
    };
    Some(outcome.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_lifecycle::AgentTaskRunRecord;
    use homeboy_engine_primitives::phase_timing::PhaseTimer;
    use homeboy_lifecycle_contract::RunExecutionState;
    use serde_json::json;
    use std::time::Duration;

    fn record_attempt(
        store: &AgentTaskLifecycleStore,
        cook_id: &str,
        attempt: u32,
        spans: &[(&str, u64)],
        gates: Value,
    ) {
        let run_id = format!("{cook_id}-attempt-{attempt}");
        let mut record = AgentTaskRunRecord::new_queued(
            run_id.clone(),
            "plan".to_string(),
            store.controller_plan_path(&run_id).display().to_string(),
            "2026-11-21T00:00:00Z".to_string(),
        );
        record.metadata = json!({
            "cook_id": cook_id,
            "cook_delivery": {
                "schema": super::super::cook_delivery::COOK_DELIVERY_SCHEMA,
                "artifacts": {"promotion": {"deterministic_gates": gates}},
            },
        });
        record.set_state_for_test(RunExecutionState::Succeeded);
        store.write_record(&record).expect("write attempt record");
        store
            .record_cook_attempt(cook_id, attempt, &run_id)
            .expect("index attempt");
        let mut timer = PhaseTimer::new();
        for (id, ms) in spans {
            timer.record_ok(*id, Duration::from_millis(*ms));
        }
        store
            .open_observation_initialized()
            .expect("observation store")
            .record_phase_spans(&run_id, "fixture", &timer.into_report())
            .expect("record spans");
    }

    #[test]
    fn cook_attempt_timeline_splits_agent_gate_and_finalize_per_attempt() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_environment().expect("store");
            let cook = "timeline-cook";
            record_attempt(
                &store,
                cook,
                1,
                &[
                    ("stage.workspace", 24_000),
                    ("provider.command", 216_000),
                    // `promote` contains the gate; it must not be counted again.
                    ("promote", 300_000),
                    ("gate.test", 100_000),
                    ("publish.aggregate", 66_000),
                ],
                json!([
                    {"id": "gate-1", "status": "succeeded",
                     "verdict": {"elapsed_ms": 180_000, "reused": false},
                     "phases": [{"name": "tests", "elapsed_ms": 109_000},
                                {"name": "acceptance", "elapsed_ms": 216_000}]},
                    {"id": "gate-2", "status": "succeeded",
                     "verdict": {"elapsed_ms": 108_000, "reused": false}},
                    {"id": "gate-3", "status": "succeeded",
                     "verdict": {"elapsed_ms": 50_000, "reused": true}},
                ]),
            );
            record_attempt(
                &store,
                cook,
                2,
                &[("provider.command", 120_000), ("gate.test", 60_000)],
                json!([{"id": "gate-1", "status": "failed"}]),
            );

            let timeline = cook_attempt_timeline_in_store(&store, cook).expect("timeline");

            assert_eq!(
                timeline,
                vec![
                    CookAttemptTiming {
                        attempt: 1,
                        queued_ms: Some(24_000),
                        agent_ms: Some(216_000),
                        gate_ms: Some(288_000),
                        slowest_gate_phase: Some(AgentTaskGatePhase {
                            name: "acceptance".to_string(),
                            elapsed_ms: 216_000,
                        }),
                        finalize_ms: Some(66_000),
                        outcome: Some("passed".to_string()),
                    },
                    CookAttemptTiming {
                        attempt: 2,
                        queued_ms: None,
                        agent_ms: Some(120_000),
                        gate_ms: Some(60_000),
                        slowest_gate_phase: None,
                        finalize_ms: None,
                        outcome: Some("gate_failed".to_string()),
                    },
                ]
            );
            assert_eq!(
                timeline[0].summary_line(),
                "attempt 1: queued 0.4m · agent 3.6m · gate 4.8m (slowest: acceptance 3.6m) · finalize 1.1m → passed"
            );
            let absent = serde_json::to_value(&timeline[1]).unwrap();
            assert!(absent.get("queued_ms").is_none());
            assert!(absent.get("finalize_ms").is_none());
            assert!(cook_attempt_timeline_in_store(&store, "no-such-cook")
                .unwrap()
                .is_empty());
        });
    }
}
