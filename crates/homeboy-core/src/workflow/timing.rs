//! Attempt timing belongs to the checkpoint, not the polling process.
use super::{InstanceState, StepPhase};
use homeboy_engine_primitives::phase_timing::{PhaseSpan, PhaseStatus, PhaseTimingReport};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkflowTiming {
    #[serde(default)]
    pub active: BTreeMap<String, u64>,
    #[serde(default)]
    pub completed: PhaseTimingReport,
}

pub fn capture(next: &mut InstanceState, now: u64) {
    let mut live = BTreeMap::new();
    for (id, step) in &next.steps {
        if step.reused {
            continue;
        }
        let phase = format!("workflow.step.{id}");
        if step.phase != StepPhase::Pending || step.attempt > 0 {
            live.insert(
                phase.clone(),
                (
                    step.attempt,
                    step.phase == StepPhase::Done,
                    step.outcome.clone(),
                ),
            );
        }
        for (index, item) in step.items.iter().enumerate() {
            if item.attempt > 0 {
                live.insert(
                    format!("{phase}.item.{index}"),
                    (
                        item.attempt,
                        item.phase == StepPhase::Done,
                        item.outcome.clone(),
                    ),
                );
            }
        }
    }
    for (phase, (attempt, done, outcome)) in live {
        // Approval has no submitted attempt; retain its waiting origin when approved.
        let key = format!("{phase}/{}", attempt.max(1));
        let old: Vec<_> = next
            .timing
            .active
            .keys()
            .filter(|k| k.starts_with(&format!("{phase}/")) && **k != key)
            .cloned()
            .collect();
        for key in old {
            finish(&mut next.timing, &key, &phase, now, true);
        }
        let already = next
            .timing
            .completed
            .spans
            .iter()
            .filter(|s| s.id == phase)
            .count();
        if already >= attempt.max(1) as usize {
            continue;
        }
        next.timing.active.entry(key.clone()).or_insert(now);
        if done || next.status.is_terminal() {
            finish(
                &mut next.timing,
                &key,
                &phase,
                now,
                outcome.as_deref() != Some("succeeded"),
            );
        }
    }
    if next.status.is_terminal() {
        for key in next.timing.active.keys().cloned().collect::<Vec<_>>() {
            let phase = key
                .rsplit_once('/')
                .map(|(phase, _)| phase)
                .unwrap_or(&key)
                .to_string();
            finish(&mut next.timing, &key, &phase, now, true);
        }
    }
}

fn finish(timing: &mut WorkflowTiming, key: &str, phase: &str, now: u64, failed: bool) {
    let Some(start) = timing.active.remove(key) else {
        return;
    };
    let Some(date) = chrono::DateTime::from_timestamp_millis(start as i64) else {
        return;
    };
    timing.completed.spans.push(PhaseSpan {
        id: phase.into(),
        status: if failed {
            PhaseStatus::Failed
        } else {
            PhaseStatus::Ok
        },
        duration_ms: Some(now.saturating_sub(start) as f64),
        started_at: date.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        homeboy_version: crate::build_identity::current().version,
    });
}
