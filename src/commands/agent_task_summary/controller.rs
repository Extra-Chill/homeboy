use serde_json::Value;

use super::super::summary_json::{array_len, string_value, value_at};
use super::{finish, first_string};

pub(super) fn render_controller_summary(payload: &Value) -> Option<String> {
    let controller = controller_payload(payload)?;
    let loop_id =
        string_value(controller, &["loop_id"]).or_else(|| string_value(payload, &["loop_id"]))?;
    let state = string_value(controller, &["state"]).unwrap_or("unknown");
    let phase = string_value(controller, &["phase"]).unwrap_or("unknown");
    let current_step = controller_current_step(controller).unwrap_or("none");
    let totals = controller_totals(controller);

    let mut lines = vec![
        "Agent task controller".to_string(),
        format!("Loop: {loop_id}"),
        format!("State: {state}"),
        format!("Current step: {phase} / {current_step}"),
        format!(
            "Actions: {} pending / {} running / {} completed / {} failed / {} total",
            totals.pending, totals.running, totals.completed, totals.failed, totals.total
        ),
        format!(
            "Entities: {} total / {} human-ready",
            totals.entities, totals.human_ready
        ),
        format!("Runs: {}", totals.runs),
        format!("Artifacts: {}", totals.artifacts),
    ];

    if let Some(failure) = controller_last_failure(payload, controller) {
        lines.push(format!("Last failure: {failure}"));
    }
    for artifact in controller_key_artifacts(controller).into_iter().take(3) {
        lines.push(format!("Artifact: {artifact}"));
    }
    if let Some(recovery) = first_controller_recovery_command(payload) {
        lines.push(format!("Next: {recovery}"));
    } else if totals.pending > 0 {
        lines.push(format!(
            "Next: homeboy agent-task controller resume {loop_id}"
        ));
    } else {
        lines.push(format!(
            "Next: homeboy agent-task controller status {loop_id}"
        ));
    }

    Some(finish(lines))
}

fn controller_payload<'a>(payload: &'a Value) -> Option<&'a Value> {
    value_at(payload, &["controller"])
        .or_else(|| value_at(payload, &["resume", "controller"]))
        .or_else(|| value_at(payload, &["from_spec", "controller"]))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ControllerTotals {
    total: usize,
    pending: usize,
    running: usize,
    completed: usize,
    failed: usize,
    entities: usize,
    human_ready: usize,
    runs: usize,
    artifacts: usize,
}

fn controller_totals(controller: &Value) -> ControllerTotals {
    let mut totals = ControllerTotals {
        entities: value_at(controller, &["entities"])
            .and_then(Value::as_object)
            .map(|entities| entities.len())
            .unwrap_or(0),
        runs: array_len(controller, &["task_lineage"]).unwrap_or(0),
        artifacts: controller_artifact_count(controller),
        ..ControllerTotals::default()
    };

    if let Some(entities) = value_at(controller, &["entities"]).and_then(Value::as_object) {
        totals.human_ready = entities
            .values()
            .filter(|entity| {
                value_at(entity, &["human_ready"])
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
    }

    if let Some(actions) = value_at(controller, &["next_actions"]).and_then(Value::as_array) {
        totals.total = actions.len();
        for action in actions {
            match string_value(action, &["status"]).unwrap_or("unknown") {
                "pending" => totals.pending += 1,
                "running" => totals.running += 1,
                "completed" | "already_satisfied" => totals.completed += 1,
                "failed"
                | "blocked_runner_unavailable"
                | "blocked_remote_materialization"
                | "blocked_local_fallback_denied" => totals.failed += 1,
                _ => {}
            }
        }
    }

    totals
}

fn controller_current_step(controller: &Value) -> Option<&str> {
    let actions = value_at(controller, &["next_actions"]).and_then(Value::as_array)?;
    actions
        .iter()
        .find(|action| string_value(action, &["status"]) == Some("running"))
        .or_else(|| {
            actions
                .iter()
                .find(|action| string_value(action, &["status"]) == Some("pending"))
        })
        .or_else(|| actions.last())
        .and_then(controller_action_label)
}

fn controller_action_label(action: &Value) -> Option<&str> {
    string_value(action, &["action_id"]).or_else(|| string_value(action, &["action", "action"]))
}

fn controller_last_failure<'a>(payload: &'a Value, controller: &'a Value) -> Option<String> {
    if let Some(summary) = first_failed_child_action(payload) {
        let diagnostic = string_value(summary, &["hydrated_root_cause"])
            .or_else(|| string_value(summary, &["top_diagnostic"]))?;
        let action_id = string_value(summary, &["action_id"]);
        let child_run_id = string_value(summary, &["child_run_id"]);
        return Some(match (action_id, child_run_id) {
            (Some(action_id), Some(run_id)) => format!("{action_id} ({run_id}): {diagnostic}"),
            (Some(action_id), None) => format!("{action_id}: {diagnostic}"),
            _ => diagnostic.to_string(),
        });
    }

    if let Some(summary) = value_at(payload, &["failure_summary"])
        .or_else(|| last_failure_summary(payload, &["results"]))
        .or_else(|| last_failure_summary(payload, &["resume", "results"]))
    {
        let diagnostic = string_value(summary, &["diagnostic"])?;
        let action_id = string_value(summary, &["action_id"]);
        return Some(match action_id {
            Some(action_id) => format!("{action_id}: {diagnostic}"),
            None => diagnostic.to_string(),
        });
    }

    value_at(controller, &["next_actions"])
        .and_then(Value::as_array)?
        .iter()
        .rev()
        .filter(|action| {
            matches!(
                string_value(action, &["status"]),
                Some(
                    "failed"
                        | "blocked_runner_unavailable"
                        | "blocked_remote_materialization"
                        | "blocked_local_fallback_denied"
                )
            )
        })
        .find_map(|action| {
            let diagnostic = value_at(action, &["diagnostics"])?
                .as_array()?
                .first()
                .and_then(|diagnostic| string_value(diagnostic, &["message"]))?;
            Some(match string_value(action, &["action_id"]) {
                Some(action_id) => format!("{action_id}: {diagnostic}"),
                None => diagnostic.to_string(),
            })
        })
}

fn first_failed_child_action(payload: &Value) -> Option<&Value> {
    value_at(payload, &["diagnostics", "failed_child_actions"])
        .and_then(Value::as_array)
        .or_else(|| value_at(payload, &["failed_child_actions"]).and_then(Value::as_array))?
        .first()
}

fn last_failure_summary<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a Value> {
    value_at(payload, path)?
        .as_array()?
        .iter()
        .rev()
        .find_map(|result| value_at(result, &["failure_summary"]))
}

fn controller_artifact_count(controller: &Value) -> usize {
    let entity_artifacts = value_at(controller, &["entities"])
        .and_then(Value::as_object)
        .map(|entities| {
            entities
                .values()
                .map(|entity| array_len(entity, &["artifact_refs"]).unwrap_or(0))
                .sum::<usize>()
        })
        .unwrap_or(0);
    let lineage_artifacts = value_at(controller, &["task_lineage"])
        .and_then(Value::as_array)
        .map(|lineage| {
            lineage
                .iter()
                .map(|item| array_len(item, &["artifact_refs"]).unwrap_or(0))
                .sum::<usize>()
        })
        .unwrap_or(0);
    entity_artifacts + lineage_artifacts
}

fn controller_key_artifacts(controller: &Value) -> Vec<String> {
    let mut artifacts = Vec::new();
    if let Some(entities) = value_at(controller, &["entities"]).and_then(Value::as_object) {
        for entity in entities.values() {
            collect_controller_artifact_lines(entity, &["artifact_refs"], &mut artifacts);
        }
    }
    if let Some(lineage) = value_at(controller, &["task_lineage"]).and_then(Value::as_array) {
        for item in lineage {
            collect_controller_artifact_lines(item, &["artifact_refs"], &mut artifacts);
        }
    }
    artifacts
}

fn collect_controller_artifact_lines(value: &Value, path: &[&str], artifacts: &mut Vec<String>) {
    let Some(refs) = value_at(value, path).and_then(Value::as_array) else {
        return;
    };
    for artifact in refs {
        let Some(uri) = string_value(artifact, &["uri"]) else {
            continue;
        };
        let label = string_value(artifact, &["label"])
            .or_else(|| string_value(artifact, &["kind"]))
            .unwrap_or("artifact");
        artifacts.push(format!("{label}: {uri}"));
    }
}

fn first_controller_recovery_command(payload: &Value) -> Option<&str> {
    if let Some(command) = first_failed_child_action(payload)
        .and_then(|action| string_value(action, &["next_command"]))
    {
        return Some(command);
    }

    value_at(payload, &["diagnostics", "pending_actions"])
        .and_then(Value::as_array)
        .or_else(|| {
            value_at(payload, &["resume", "diagnostics", "pending_actions"])
                .and_then(Value::as_array)
        })?
        .iter()
        .find_map(|action| first_string(action, &["recovery_commands"]))
}
