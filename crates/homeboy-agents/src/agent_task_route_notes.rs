//! Operator-facing provider-route notes for a durable agent-task run (#15881).
//!
//! A primary provider route skipped as unready (`credentials_unusable`,
//! `auth_failure`, …) historically left only an event-log line and
//! `metadata.provider_readiness_routing.skipped` evidence, so operators could
//! not see that a cook ran on a fallback model. Both render surfaces read one
//! shared projection: one bounded, deduped line per skipped route, prefixed
//! once with the route the run actually executed.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::agent_task_lifecycle::AgentTaskRunRecord;
use crate::agent_task_scheduler::{AgentTaskPlan, ProviderRouteEvidence};

const MAX_ROUTE_NOTES: usize = 4;
const NOTE_MAX_CHARS: usize = 200;

/// One bounded, deduped line per provider route the run skipped before
/// dispatch, prefixed once with the model that actually ran. For example:
///
/// ```text
/// Ran on openai/gpt-6-luna; anthropic/claude-sonnet-5-5 skipped: credentials_unusable (Refresh the provider-owned OpenCode credentials, then retry.)
/// xai/grok-4.7 skipped: usage_capped (Wait for the provider usage cap to reset.)
/// ```
pub(crate) fn provider_route_notes(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
) -> Vec<String> {
    let Some(skipped) = plan.map(readiness_route_skips) else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    for route in dedupe(skipped) {
        if notes.len() >= MAX_ROUTE_NOTES {
            break;
        }
        let mut note = format!(
            "{}/{} skipped: {}",
            route.backend,
            route.model.as_deref().unwrap_or("default model"),
            route.state
        );
        if let Some(remediation) = route.remediation.as_deref() {
            note.push_str(&format!(" ({remediation})"));
        }
        notes.push(bound_note(note));
    }
    if let Some(ran_on) = executed_route_model(record) {
        if let Some(first) = notes.first_mut() {
            *first = bound_note(format!("Ran on {ran_on}; {first}"));
        }
    }
    notes.into_iter().take(MAX_ROUTE_NOTES).collect()
}

/// Full route identity, ordered as the plan discovered it. A `BTreeSet` keeps
/// the dedupe bounded without changing the evidence's own order semantics.
fn dedupe(skipped: Vec<ProviderRouteEvidence>) -> Vec<ProviderRouteEvidence> {
    let mut seen = BTreeSet::new();
    skipped
        .into_iter()
        .filter(|route| {
            seen.insert((
                route.backend.clone(),
                route.selector.clone(),
                route.model.clone(),
                route.state.clone(),
                route.reason.clone(),
                route.remediation.clone(),
            ))
        })
        .collect()
}

fn readiness_route_skips(plan: &AgentTaskPlan) -> Vec<ProviderRouteEvidence> {
    plan.tasks
        .iter()
        .flat_map(|task| {
            task.metadata
                .pointer("/provider_readiness_routing/skipped")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}

/// The concrete model this run actually executed under, from the durable
/// executor evidence, or the last durable provider execution.
fn executed_route_model(record: &AgentTaskRunRecord) -> Option<String> {
    if let Some(model) = record
        .latest_executor_evidence
        .as_ref()
        .and_then(|evidence| evidence.model.as_deref())
        .filter(|model| !model.trim().is_empty())
    {
        return Some(model.trim().to_string());
    }
    record
        .metadata
        .get("provider_executions")
        .and_then(Value::as_array)?
        .last()
        .and_then(|execution| execution.get("model").and_then(Value::as_str))
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
}

fn bound_note(note: String) -> String {
    if note.chars().count() <= NOTE_MAX_CHARS {
        return note;
    }
    let mut bounded: String = note
        .chars()
        .take(NOTE_MAX_CHARS.saturating_sub(1))
        .collect();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{AgentTaskEvidenceRef, AgentTaskRequest};
    use crate::agent_task_lifecycle::AgentTaskLatestExecutorEvidence;
    use crate::agent_task_scheduler::AgentTaskPlan;
    use serde_json::json;

    fn base_record() -> AgentTaskRunRecord {
        serde_json::from_value(json!({
            "schema": crate::agent_task_lifecycle::schemas::RUN,
            "run_id": "run-1",
            "plan_id": "plan-1",
            "state": "succeeded",
            "submitted_at": "2026-01-01T00:00:00Z",
            "plan_path": "plan.json",
        }))
        .expect("record fixture")
    }

    fn plan_with_skips() -> AgentTaskPlan {
        let request: AgentTaskRequest = serde_json::from_value(json!({
            "task_id": "task-1",
            "executor": { "backend": "opencode" },
            "instructions": "do the thing",
            "metadata": {
                "provider_readiness_routing": {
                    "skipped": [{
                        "backend": "opencode",
                        "selector": null,
                        "model": "anthropic/claude-sonnet-5-5",
                        "state": "credentials_unusable",
                        "reason": "OPENCODE_API_KEY is missing",
                        "reset_at": null,
                        "classification": "credentials",
                        "retryable": true,
                        "remediation": "Refresh the provider-owned OpenCode credentials, then retry.",
                        "cache_identity": null,
                        "provider_identity": null,
                    }],
                },
            },
        }))
        .expect("request fixture");
        AgentTaskPlan::new("plan-1", vec![request])
    }

    #[test]
    fn renders_one_line_per_deduped_skipped_route_with_the_executed_model() {
        let mut record = base_record();
        record.latest_executor_evidence = Some(AgentTaskLatestExecutorEvidence {
            task_id: "task-1".to_string(),
            backend: "opencode".to_string(),
            selector: None,
            model: Some("openai/gpt-6-luna".to_string()),
            input_ref: AgentTaskEvidenceRef {
                kind: "executor-input".to_string(),
                uri: "homeboy://executor-input".to_string(),
                label: None,
            },
            normalized_output_ref: AgentTaskEvidenceRef {
                kind: "executor-result".to_string(),
                uri: "homeboy://executor-result".to_string(),
                label: None,
            },
            outcome_ref: AgentTaskEvidenceRef {
                kind: "executor-outcome".to_string(),
                uri: "homeboy://executor-outcome".to_string(),
                label: None,
            },
            provider_run_id: None,
            component_contracts: Vec::new(),
            runtime_component_paths: Vec::new(),
            expected_artifacts: Vec::new(),
            typed_artifact_expectations: Vec::new(),
        });
        let notes = provider_route_notes(&record, Some(&plan_with_skips()));
        assert_eq!(
            notes,
            vec!["Ran on openai/gpt-6-luna; opencode/anthropic/claude-sonnet-5-5 skipped: credentials_unusable (Refresh the provider-owned OpenCode credentials, then retry.)"
                .to_string()]
        );
    }

    /// An executor failure or dedupe mistake must not render an unbounded
    /// status block: notes are capped and each line is bounded.
    #[test]
    fn notes_are_bounded_without_an_executed_model_or_duplicate_routes() {
        let record = base_record();
        let mut plan = plan_with_skips();
        // Duplicate the same skip across two tasks; it must render once.
        let mut duplicated = plan.tasks[0].clone();
        duplicated.task_id = "task-2".to_string();
        plan.tasks.push(duplicated);
        let notes = provider_route_notes(&record, Some(&plan));
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("opencode/anthropic/claude-sonnet-5-5 skipped:"));
    }
}
