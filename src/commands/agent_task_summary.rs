use serde_json::Value;

use super::agent_task::{AgentTaskArgs, AgentTaskCommand, AgentTaskControllerCommand};
use super::summary_json::{array_len, string_value, u64_value, usize_value, value_at};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTaskSummaryKind {
    Cook,
    Status,
    Logs,
    Review,
    Controller,
}

pub(crate) fn agent_task_summary_kind(args: &AgentTaskArgs) -> Option<AgentTaskSummaryKind> {
    match &args.command {
        AgentTaskCommand::Cook(_) => Some(AgentTaskSummaryKind::Cook),
        AgentTaskCommand::Status(_) => Some(AgentTaskSummaryKind::Status),
        AgentTaskCommand::Logs(_) => Some(AgentTaskSummaryKind::Logs),
        AgentTaskCommand::Review(_) => Some(AgentTaskSummaryKind::Review),
        AgentTaskCommand::Controller(controller_args) => match &controller_args.command {
            AgentTaskControllerCommand::Status(_)
            | AgentTaskControllerCommand::RunNext(_)
            | AgentTaskControllerCommand::Run(_)
            | AgentTaskControllerCommand::Resume(_) => Some(AgentTaskSummaryKind::Controller),
            AgentTaskControllerCommand::FromSpec(args) if args.resume => {
                Some(AgentTaskSummaryKind::Controller)
            }
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn render_agent_task_summary(
    kind: AgentTaskSummaryKind,
    payload: &Value,
) -> Option<String> {
    match kind {
        AgentTaskSummaryKind::Cook => render_cook_summary(payload),
        AgentTaskSummaryKind::Status => render_status_summary(payload),
        AgentTaskSummaryKind::Logs => render_logs_summary(payload),
        AgentTaskSummaryKind::Review => render_review_summary(payload),
        AgentTaskSummaryKind::Controller => render_controller_summary(payload),
    }
}

fn render_cook_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let raw_state = string_value(payload, &["state"])
        .or_else(|| string_value(payload, &["record", "state"]))
        .unwrap_or("unknown");
    let tasks_planned = usize_value(payload, &["task_count"])
        .or_else(|| array_len(payload, &["record", "tasks"]))
        .unwrap_or(0);
    let tasks_attempted = aggregate_outcome_count(payload).unwrap_or(0);
    let aggregate_path = string_value(payload, &["aggregate_path"])
        .or_else(|| string_value(payload, &["record", "aggregate_path"]));
    let metrics = code_production_metrics(payload);
    let state = effective_run_state(raw_state, tasks_attempted, metrics.non_empty_patches);
    let artifact_count = aggregate_artifact_count(payload);
    let first_artifact = string_value(
        payload,
        &["aggregate", "outcomes", "0", "artifacts", "0", "path"],
    )
    .or_else(|| {
        string_value(
            payload,
            &["aggregate", "outcomes", "0", "artifacts", "0", "id"],
        )
    });

    let mut lines = vec![
        "Agent task cook".to_string(),
        format!("Run: {run_id}"),
        format!("Status: {state}"),
        format!("Tasks planned: {tasks_planned}"),
        format!("Tasks attempted: {tasks_attempted}"),
    ];
    lines.extend(code_production_lines(&metrics));
    if let Some(path) = aggregate_path {
        lines.push(format!("Aggregate: {path}"));
    }
    lines.push(format!("Artifacts: {artifact_count}"));
    if let Some(artifact) = first_artifact {
        lines.push(format!("First artifact: {artifact}"));
    }
    if metrics.non_empty_patches > 0 {
        lines.push(format!("Next: homeboy agent-task review {run_id}"));
    } else {
        lines.push(format!("Next: homeboy agent-task logs {run_id}"));
    }
    Some(finish(lines))
}

fn render_status_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let raw_state = string_value(payload, &["state"]).unwrap_or("unknown");
    let tasks_planned = array_len(payload, &["tasks"]).unwrap_or(0);
    let tasks_attempted = status_attempted_task_count(payload);
    let metrics = code_production_metrics(payload);
    let state = effective_run_state(raw_state, tasks_attempted, metrics.non_empty_patches);
    let artifact_count = array_len(payload, &["artifact_refs"]).unwrap_or(0);
    let aggregate_path = string_value(payload, &["aggregate_path"]);

    let mut lines = vec![
        "Agent task status".to_string(),
        format!("Run: {run_id}"),
        format!("Status: {state}"),
        format!("Tasks planned: {tasks_planned}"),
        format!("Tasks attempted: {tasks_attempted}"),
    ];
    lines.extend(code_production_lines(&metrics));
    if let Some(diagnostic) = first_actionable_diagnostic(payload) {
        lines.push(format!("Diagnostic: {diagnostic}"));
    }
    lines.push(format!("Artifacts: {artifact_count}"));
    if let Some(path) = aggregate_path.filter(|_| metrics.non_empty_patches > 0) {
        lines.push(format!("Aggregate: {path}"));
        lines.push(format!("Next: homeboy agent-task review {run_id}"));
    } else if state == "queued" {
        lines.push(format!("Next: homeboy agent-task run {run_id}"));
    } else {
        lines.push(format!("Next: homeboy agent-task logs {run_id}"));
    }
    Some(finish(lines))
}

fn render_logs_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let event_count = array_len(payload, &["events"]).unwrap_or(0);
    let mut lines = vec![
        "Agent task logs".to_string(),
        format!("Run: {run_id}"),
        format!("Events: {event_count}"),
    ];
    if let Some(diagnostic) = first_actionable_diagnostic(payload) {
        lines.push(format!("Diagnostic: {diagnostic}"));
    }
    Some(finish(lines))
}

fn render_review_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let state = string_value(payload, &["state"]).unwrap_or("unknown");
    let summary = value_at(payload, &["aggregate_review", "summary"]);
    let raw_apply_candidates = summary
        .and_then(|_| {
            usize_value(
                payload,
                &["aggregate_review", "summary", "apply_candidates"],
            )
        })
        .unwrap_or(0);
    let failed = summary
        .and_then(|_| usize_value(payload, &["aggregate_review", "summary", "failed"]))
        .unwrap_or(0);
    let metrics = code_production_metrics(payload);
    let promotable = metrics.non_empty_patches > 0;
    let patch = promotable
        .then(|| string_value(payload, &["promotion_candidates", "0", "artifact_id"]))
        .flatten();
    let patch_path = patch.and_then(|artifact_id| artifact_path(payload, artifact_id));
    let next = first_string(payload, &["next_actions"]);
    let command = promotable
        .then(|| command_line(payload, &["promotion_candidates", "0", "command"]))
        .flatten();

    let outcome = if promotable {
        "patch produced, not promoted"
    } else if raw_apply_candidates > 0 {
        "no-op: patch artifacts produced but empty"
    } else if failed > 0 || state == "failed" || state == "partial_failure" {
        "failed or partial failure"
    } else {
        "no patch candidates"
    };

    let mut lines = vec![
        "Agent task review".to_string(),
        format!("Run: {run_id}"),
        format!("Status: {state}"),
        format!("Outcome: {outcome}"),
    ];
    lines.extend(code_production_lines(&metrics));
    if let Some(diagnostic) = first_actionable_diagnostic(payload) {
        lines.push(format!("Diagnostic: {diagnostic}"));
    }
    if let Some(patch_path) = patch_path {
        lines.push(format!("Patch: {patch_path}"));
    } else if let Some(patch) = patch {
        lines.push(format!("Patch: {patch}"));
    }
    if let Some(command) = command {
        lines.push(format!("Next: {command}"));
    } else if let Some(next) = next {
        lines.push(format!("Next: {next}"));
    }
    Some(finish(lines))
}

fn render_controller_summary(payload: &Value) -> Option<String> {
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
    value_at(payload, &["diagnostics", "pending_actions"])
        .and_then(Value::as_array)
        .or_else(|| {
            value_at(payload, &["resume", "diagnostics", "pending_actions"])
                .and_then(Value::as_array)
        })?
        .iter()
        .find_map(|action| first_string(action, &["recovery_commands"]))
}

fn first_actionable_diagnostic(payload: &Value) -> Option<&str> {
    string_value(payload, &["diagnostic_summary", "message"])
        .or_else(|| first_diagnostic_message(payload, &["aggregate", "outcomes"]))
        .or_else(|| first_diagnostic_message(payload, &["aggregate_review", "tasks"]))
}

fn first_diagnostic_message<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(payload, path)?
        .as_array()?
        .iter()
        .find_map(|item| {
            value_at(item, &["diagnostics"])?
                .as_array()?
                .iter()
                .find_map(|diagnostic| string_value(diagnostic, &["message"]))
        })
}

fn aggregate_outcome_count(payload: &Value) -> Option<usize> {
    array_len(payload, &["aggregate", "outcomes"])
}

fn aggregate_artifact_count(payload: &Value) -> usize {
    value_at(payload, &["aggregate", "outcomes"])
        .and_then(Value::as_array)
        .map(|outcomes| {
            outcomes
                .iter()
                .map(|outcome| array_len(outcome, &["artifacts"]).unwrap_or(0))
                .sum()
        })
        .unwrap_or_else(|| array_len(payload, &["artifact_refs"]).unwrap_or(0))
}

const APPLY_ARTIFACT_KINDS: &[&str] = &[
    "patch",
    "diff",
    "change_artifact",
    "workspace_patch",
    "artifact",
];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CodeProductionMetrics {
    non_empty_patches: usize,
    empty_patches: usize,
    unknown_size_patches: usize,
    diff_bytes: u64,
    changed_files: usize,
}

fn code_production_lines(metrics: &CodeProductionMetrics) -> Vec<String> {
    let patch_candidates = if metrics.unknown_size_patches > 0 {
        format!(
            "Patch candidates: {} non-empty / {} empty / {} unknown",
            metrics.non_empty_patches, metrics.empty_patches, metrics.unknown_size_patches
        )
    } else {
        format!(
            "Patch candidates: {} non-empty / {} empty",
            metrics.non_empty_patches, metrics.empty_patches
        )
    };
    vec![
        patch_candidates,
        format!("Changed files: {}", metrics.changed_files),
        format!("Diff bytes: {}", metrics.diff_bytes),
    ]
}

fn code_production_metrics(payload: &Value) -> CodeProductionMetrics {
    let mut metrics = CodeProductionMetrics::default();
    for patch in collect_patch_artifacts(payload) {
        match patch.size_bytes {
            Some(size) if size > 0 => {
                metrics.non_empty_patches += 1;
                metrics.diff_bytes += size;
                metrics.changed_files += patch.changed_files;
            }
            Some(_) => metrics.empty_patches += 1,
            None => metrics.unknown_size_patches += 1,
        }
    }
    metrics
}

struct PatchArtifact {
    size_bytes: Option<u64>,
    changed_files: usize,
}

fn collect_patch_artifacts(payload: &Value) -> Vec<PatchArtifact> {
    if let Some(inventory) =
        value_at(payload, &["aggregate_review", "artifact_inventory"]).and_then(Value::as_array)
    {
        let candidate_ids = review_apply_candidate_ids(payload);
        return inventory
            .iter()
            .filter_map(|item| {
                let artifact_id = string_value(item, &["artifact_id"])?;
                let task_id = string_value(item, &["task_id"])?;
                if !candidate_ids.contains(&(task_id, artifact_id)) {
                    return None;
                }
                if !is_apply_kind(item) {
                    return None;
                }
                Some(PatchArtifact {
                    size_bytes: u64_value(item, &["size_bytes"]),
                    changed_files: metadata_changed_files(item),
                })
            })
            .collect();
    }

    if let Some(outcomes) = value_at(payload, &["aggregate", "outcomes"]).and_then(Value::as_array)
    {
        return outcomes
            .iter()
            .flat_map(|outcome| {
                value_at(outcome, &["artifacts"])
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|artifact| is_display_apply_artifact(artifact))
                    .map(|artifact| PatchArtifact {
                        size_bytes: u64_value(artifact, &["size_bytes"]),
                        changed_files: metadata_changed_files(artifact),
                    })
            })
            .collect();
    }

    if let Some(references) = value_at(payload, &["artifact_refs"]).and_then(Value::as_array) {
        return references
            .iter()
            .filter(|reference| is_apply_kind(reference))
            .map(|reference| PatchArtifact {
                size_bytes: u64_value(reference, &["size_bytes"]),
                changed_files: metadata_changed_files(reference),
            })
            .collect();
    }

    Vec::new()
}

fn review_apply_candidate_ids(payload: &Value) -> Vec<(&str, &str)> {
    let mut ids = Vec::new();
    let Some(candidates) =
        value_at(payload, &["aggregate_review", "apply_candidates"]).and_then(Value::as_array)
    else {
        return ids;
    };
    for candidate in candidates {
        let Some(task_id) = string_value(candidate, &["task_id"]) else {
            continue;
        };
        let Some(artifact_ids) = value_at(candidate, &["artifact_ids"]).and_then(Value::as_array)
        else {
            continue;
        };
        for artifact_id in artifact_ids {
            if let Some(artifact_id) = artifact_id.as_str() {
                ids.push((task_id, artifact_id));
            }
        }
    }
    ids
}

fn is_apply_kind(artifact: &Value) -> bool {
    let Some(kind) = string_value(artifact, &["kind"]) else {
        return false;
    };
    APPLY_ARTIFACT_KINDS.contains(&kind)
}

fn is_display_apply_artifact(artifact: &Value) -> bool {
    if !is_apply_kind(artifact) {
        return false;
    }
    !(artifact_flag(artifact, "rejected") || artifact_flag(artifact, "false_positive"))
}

fn artifact_flag(artifact: &Value, key: &str) -> bool {
    value_at(artifact, &["metadata"])
        .and_then(|metadata| metadata.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn metadata_changed_files(artifact: &Value) -> usize {
    if let Some(files) =
        value_at(artifact, &["metadata", "changed_files"]).and_then(Value::as_array)
    {
        return files.len();
    }
    value_at(artifact, &["metadata", "changed_file_count"])
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .unwrap_or(0)
}

/// A run whose lifecycle state is `succeeded` but that produced zero promotion
/// candidates did not actually patch anything. Surface that honestly as
/// `no_patch_produced` instead of advertising success (#4610).
fn effective_run_state(raw_state: &str, tasks_attempted: usize, patch_candidates: usize) -> &str {
    if raw_state == "succeeded" && tasks_attempted > 0 && patch_candidates == 0 {
        "no_patch_produced"
    } else {
        raw_state
    }
}

fn status_attempted_task_count(payload: &Value) -> usize {
    value_at(payload, &["tasks"])
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter(|task| {
                    matches!(
                        string_value(task, &["state"]),
                        Some("running" | "succeeded" | "failed" | "cancelled" | "timed_out")
                    )
                })
                .count()
        })
        .unwrap_or(0)
}

fn first_string<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(payload, path)?.as_array()?.first()?.as_str()
}

fn command_line(payload: &Value, path: &[&str]) -> Option<String> {
    let command = value_at(payload, path)?.as_array()?;
    let parts: Vec<_> = command.iter().filter_map(Value::as_str).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn artifact_path<'a>(payload: &'a Value, artifact_id: &str) -> Option<&'a str> {
    value_at(payload, &["aggregate_review", "artifact_inventory"])?
        .as_array()?
        .iter()
        .find(|artifact| string_value(artifact, &["artifact_id"]) == Some(artifact_id))
        .and_then(|artifact| string_value(artifact, &["path"]))
}

fn finish(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cook_summary_leads_with_run_status_and_review_next_step() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "task_count": 1,
            "aggregate_path": "/tmp/aggregate.json",
            "aggregate": {
                "outcomes": [{
                    "task_id": "homeboy-4345",
                    "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 128 }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.starts_with("Agent task cook\nRun: homeboy-4345\nStatus: succeeded"));
        assert!(summary.contains("Tasks planned: 1\n"));
        assert!(summary.contains("Tasks attempted: 1\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Diff bytes: 128\n"));
        assert!(summary.contains("First artifact: /tmp/patch.diff\n"));
        assert!(summary.contains("Next: homeboy agent-task review homeboy-4345\n"));
        assert!(!summary.contains("{\n"));
    }

    #[test]
    fn cook_summary_reports_no_patch_produced_when_all_cells_are_empty() {
        // Reproduces the #4610 cook summary: 3 succeeded cells, but every patch
        // artifact is 0 bytes. The summary must not advertise success.
        let payload = json!({
            "run_id": "agent-task-abe47e4d",
            "state": "succeeded",
            "task_count": 3,
            "aggregate_path": "/tmp/aggregate.json",
            "aggregate_review": {
                "summary": { "apply_candidates": 0 }
            },
            "aggregate": {
                "outcomes": [
                    { "task_id": "cell-1", "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }] },
                    { "task_id": "cell-2", "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }] },
                    { "task_id": "cell-3", "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }] }
                ]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: no_patch_produced\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 3 empty\n"));
        assert!(summary.contains("Next: homeboy agent-task logs agent-task-abe47e4d\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn cook_summary_treats_unknown_size_patch_as_zero_candidates() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": {
                "outcomes": [{
                    "task_id": "homeboy-4345",
                    "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff" }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: no_patch_produced\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty / 1 unknown\n"));
        assert!(summary.contains("Next: homeboy agent-task logs homeboy-4345\n"));
    }

    #[test]
    fn cook_summary_counts_empty_patch_artifact_as_empty_not_candidate() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "task_count": 1,
            "aggregate_review": {
                "summary": { "apply_candidates": 0 }
            },
            "aggregate": {
                "outcomes": [{
                    "task_id": "homeboy-4345",
                    "artifacts": [{ "id": "empty-patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Patch candidates: 0 non-empty / 1 empty\n"));
        assert!(summary.contains("Diff bytes: 0\n"));
        assert!(summary.contains("Next: homeboy agent-task logs homeboy-4345\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn cook_summary_surfaces_changed_files_and_diff_bytes_from_metadata() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": {
                "outcomes": [{
                    "task_id": "homeboy-4345",
                    "artifacts": [{
                        "id": "patch",
                        "kind": "patch",
                        "path": "/tmp/patch.diff",
                        "size_bytes": 256,
                        "metadata": { "changed_files": ["src/lib.rs", "src/main.rs"] }
                    }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Changed files: 2\n"));
        assert!(summary.contains("Diff bytes: 256\n"));
    }

    #[test]
    fn cook_summary_does_not_count_provider_failures_as_patch_candidates() {
        let payload = json!({
            "run_id": "agent-task-22bb7835",
            "state": "failed",
            "task_count": 4,
            "aggregate_path": "/tmp/aggregate.json",
            "aggregate": {
                "outcomes": [
                    { "task_id": "cell-1", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] },
                    { "task_id": "cell-2", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] },
                    { "task_id": "cell-3", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] },
                    { "task_id": "cell-4", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] }
                ]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Tasks planned: 4\n"));
        assert!(summary.contains("Tasks attempted: 4\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
        assert!(summary.contains("Artifacts: 0\n"));
        assert!(summary.contains("Next: homeboy agent-task logs agent-task-22bb7835\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn review_summary_surfaces_patch_candidate_before_next_command() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "aggregate_review": {
                "summary": {
                    "apply_candidates": 1,
                    "failed": 0
                },
                "apply_candidates": [{
                    "task_id": "homeboy-4345",
                    "decision": "apply_candidate",
                    "reason": "succeeded with reviewable patch/artifact output",
                    "artifact_ids": ["patch.diff"]
                }],
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "patch.diff",
                    "kind": "patch",
                    "path": "/tmp/patch.diff",
                    "size_bytes": 128
                }]
            },
            "promotion_candidates": [{
                "artifact_id": "patch.diff",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "patch.diff"]
            }]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.starts_with("Agent task review\nRun: homeboy-4345\nStatus: succeeded"));
        assert!(summary.contains("Outcome: patch produced, not promoted\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Diff bytes: 128\n"));
        assert!(summary.contains("Patch: /tmp/patch.diff\n"));
        assert!(summary
            .contains("Next: homeboy agent-task promote homeboy-4345 --artifact-id patch.diff\n"));
        assert!(!summary.contains("promotion_candidates"));
    }

    #[test]
    fn review_summary_does_not_treat_stale_promotion_candidates_as_patches() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "failed",
            "aggregate_review": {
                "summary": {
                    "apply_candidates": 0,
                    "failed": 1
                },
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "empty-patch",
                    "kind": "patch",
                    "path": "/tmp/patch.diff",
                    "size_bytes": 0
                }]
            },
            "promotion_candidates": [{
                "artifact_id": "empty-patch",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "empty-patch"]
            }]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: failed or partial failure\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
        assert!(!summary.contains("patch produced"));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
    }

    #[test]
    fn review_summary_marks_no_op_when_apply_candidates_are_empty_patches() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "aggregate_review": {
                "summary": {
                    "apply_candidates": 3,
                    "failed": 0
                },
                "apply_candidates": [
                    { "task_id": "cell-1", "decision": "apply_candidate", "reason": "succeeded with reviewable patch/artifact output", "artifact_ids": ["sample-patch-1"] },
                    { "task_id": "cell-2", "decision": "apply_candidate", "reason": "succeeded with reviewable patch/artifact output", "artifact_ids": ["sample-patch-2"] },
                    { "task_id": "cell-3", "decision": "apply_candidate", "reason": "succeeded with reviewable patch/artifact output", "artifact_ids": ["sample-patch-3"] }
                ],
                "artifact_inventory": [
                    { "task_id": "cell-1", "artifact_id": "sample-patch-1", "kind": "patch", "path": "/tmp/patch-1.diff", "size_bytes": 0 },
                    { "task_id": "cell-2", "artifact_id": "sample-patch-2", "kind": "patch", "path": "/tmp/patch-2.diff", "size_bytes": 0 },
                    { "task_id": "cell-3", "artifact_id": "sample-patch-3", "kind": "patch", "path": "/tmp/patch-3.diff", "size_bytes": 0 }
                ]
            },
            "promotion_candidates": [{
                "artifact_id": "sample-patch-1",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "sample-patch-1"]
            }],
            "next_actions": ["inspect task summaries before retrying or reporting"]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: no-op: patch artifacts produced but empty\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 3 empty\n"));
        assert!(summary.contains("Diff bytes: 0\n"));
        assert!(!summary.contains("Patch: "));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
        assert!(summary.contains("Next: inspect task summaries before retrying or reporting"));
    }

    #[test]
    fn review_summary_treats_unknown_size_patch_as_not_promotable() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "aggregate_review": {
                "summary": { "apply_candidates": 1, "failed": 0 },
                "apply_candidates": [{
                    "task_id": "homeboy-4345",
                    "decision": "apply_candidate",
                    "reason": "succeeded with reviewable patch/artifact output",
                    "artifact_ids": ["unmeasured-patch"]
                }],
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "unmeasured-patch",
                    "kind": "patch",
                    "path": "/tmp/patch.diff"
                }]
            },
            "promotion_candidates": [{
                "artifact_id": "unmeasured-patch",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "unmeasured-patch"]
            }]
        });

        let metrics = code_production_metrics(&payload);

        assert_eq!(metrics.non_empty_patches, 0);
        assert_eq!(metrics.empty_patches, 0);
        assert_eq!(metrics.unknown_size_patches, 1);
        assert_eq!(metrics.diff_bytes, 0);

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty / 1 unknown\n"));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
    }

    #[test]
    fn review_summary_surfaces_first_outcome_diagnostic() {
        let payload = json!({
            "run_id": "agent-task-d1622a44",
            "state": "failed",
            "aggregate_review": {
                "summary": { "apply_candidates": 0, "failed": 1 },
                "tasks": [{
                    "task_id": "agent-task-d1622a44",
                    "status": "provider_error",
                    "diagnostics": [{
                        "class": "provider_discovery",
                        "message": "Requested provider \"codex\" is not registered. Registered provider plugins: []"
                    }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains(
            "Diagnostic: Requested provider \"codex\" is not registered. Registered provider plugins: []\n"
        ));
    }

    #[test]
    fn status_summary_points_queued_runs_at_run_command() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "queued",
            "tasks": [{ "task_id": "homeboy-4345" }],
            "artifact_refs": []
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.starts_with("Agent task status\nRun: homeboy-4345\nStatus: queued"));
        assert!(summary.contains("Tasks planned: 1\n"));
        assert!(summary.contains("Tasks attempted: 0\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
        assert!(summary.contains("Artifacts: 0\n"));
        assert!(summary.contains("Next: homeboy agent-task run homeboy-4345\n"));
    }

    #[test]
    fn status_summary_agrees_no_patch_candidates_means_logs_next_step() {
        let payload = json!({
            "run_id": "agent-task-22bb7835",
            "state": "failed",
            "aggregate_path": "/tmp/aggregate.json",
            "tasks": [
                { "task_id": "cell-1", "state": "failed" },
                { "task_id": "cell-2", "state": "failed" },
                { "task_id": "cell-3", "state": "failed" },
                { "task_id": "cell-4", "state": "failed" }
            ],
            "artifact_refs": []
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Tasks planned: 4\n"));
        assert!(summary.contains("Tasks attempted: 4\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
        assert!(summary.contains("Next: homeboy agent-task logs agent-task-22bb7835\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn status_summary_surfaces_code_production_breakdown_alongside_raw_artifact_count() {
        let mut artifact_refs = vec![
            json!({ "task_id": "cell-1", "kind": "patch", "uri": "artifact://cell-1/patch.diff", "size_bytes": 512 }),
            json!({ "task_id": "cell-2", "kind": "patch", "uri": "artifact://cell-2/patch.diff", "size_bytes": 0 }),
        ];
        for index in 0..40 {
            artifact_refs.push(json!({
                "task_id": "cell-1",
                "kind": "provider-transcript",
                "uri": format!("artifact://cell-1/transcript-{index}.log"),
                "size_bytes": 1024
            }));
        }

        let payload = json!({
            "run_id": "agent-task-deadbeef",
            "state": "succeeded",
            "tasks": [{ "task_id": "cell-1", "state": "succeeded" }],
            "artifact_refs": artifact_refs
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Artifacts: 42\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 1 empty\n"));
        assert!(summary.contains("Diff bytes: 512\n"));
    }

    #[test]
    fn status_summary_flags_no_op_when_all_patch_artifacts_are_empty() {
        let payload = json!({
            "run_id": "agent-task-deadbeef",
            "state": "succeeded",
            "tasks": [{ "task_id": "cell-1", "state": "succeeded" }],
            "artifact_refs": [
                { "task_id": "cell-1", "kind": "patch", "uri": "artifact://cell-1/patch-1.diff", "size_bytes": 0 },
                { "task_id": "cell-2", "kind": "patch", "uri": "artifact://cell-2/patch-2.diff", "size_bytes": 0 },
                { "task_id": "cell-3", "kind": "patch", "uri": "artifact://cell-3/patch-3.diff", "size_bytes": 0 },
                { "task_id": "cell-1", "kind": "provider-transcript", "uri": "artifact://cell-1/transcript.log", "size_bytes": 4096 }
            ]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Artifacts: 4\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 3 empty\n"));
        assert!(summary.contains("Diff bytes: 0\n"));
        assert!(summary.contains("Next: homeboy agent-task logs agent-task-deadbeef\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn status_summary_surfaces_diagnostic_summary() {
        let payload = json!({
            "run_id": "agent-task-d1622a44",
            "state": "failed",
            "tasks": [{ "task_id": "agent-task-d1622a44", "state": "failed" }],
            "artifact_refs": [],
            "diagnostic_summary": {
                "task_id": "agent-task-d1622a44",
                "class": "provider_discovery",
                "message": "Requested provider \"codex\" is not registered. Registered provider plugins: []"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains(
            "Diagnostic: Requested provider \"codex\" is not registered. Registered provider plugins: []\n"
        ));
    }

    #[test]
    fn logs_summary_surfaces_diagnostic_summary() {
        let payload = json!({
            "run_id": "agent-task-d1622a44",
            "events": [{
                "task_id": "agent-task-d1622a44",
                "state": "failed",
                "attempt": 1,
                "message": "Embedded agent runtime failed."
            }],
            "diagnostic_summary": {
                "task_id": "agent-task-d1622a44",
                "class": "provider_discovery",
                "message": "Requested provider \"codex\" is not registered. Registered provider plugins: []"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Logs, &payload).unwrap();

        assert!(summary.starts_with("Agent task logs\nRun: agent-task-d1622a44\nEvents: 1\n"));
        assert!(summary.contains(
            "Diagnostic: Requested provider \"codex\" is not registered. Registered provider plugins: []\n"
        ));
    }

    #[test]
    fn controller_status_summary_surfaces_operator_resume_context() {
        let payload = json!({
            "schema": "homeboy/agent-task-loop-controller-status/v1",
            "controller": {
                "loop_id": "loop-123",
                "phase": "triage",
                "state": "running",
                "entities": {
                    "entity-1": {
                        "human_ready": true,
                        "run_refs": [{ "run_id": "agent-task-1" }],
                        "artifact_refs": [{
                            "uri": "artifact://agent-task-1/report.json",
                            "kind": "report",
                            "label": "summary report"
                        }]
                    },
                    "entity-2": { "human_ready": false }
                },
                "task_lineage": [{
                    "run_id": "agent-task-1",
                    "artifact_refs": [{ "uri": "artifact://agent-task-1/log.txt", "kind": "log" }]
                }],
                "next_actions": [
                    { "action_id": "action-1", "action": { "action": "spawn_task" }, "status": "completed" },
                    { "action_id": "action-2", "action": { "action": "spawn_task" }, "status": "pending" }
                ]
            },
            "diagnostics": {
                "pending_actions": [{
                    "action_id": "action-2",
                    "recovery_commands": ["homeboy agent-task controller run loop-123 --action-id action-2"]
                }]
            }
        });

        let summary =
            render_agent_task_summary(AgentTaskSummaryKind::Controller, &payload).unwrap();

        assert!(summary.starts_with(
            "Agent task controller\nLoop: loop-123\nState: running\nCurrent step: triage / action-2\n"
        ));
        assert!(
            summary.contains("Actions: 1 pending / 0 running / 1 completed / 0 failed / 2 total\n")
        );
        assert!(summary.contains("Entities: 2 total / 1 human-ready\n"));
        assert!(summary.contains("Runs: 1\n"));
        assert!(summary.contains("Artifacts: 2\n"));
        assert!(summary.contains("Artifact: summary report: artifact://agent-task-1/report.json\n"));
        assert!(summary
            .contains("Next: homeboy agent-task controller run loop-123 --action-id action-2\n"));
        assert!(!summary.contains("schema"));
    }

    #[test]
    fn controller_resume_summary_surfaces_last_failure_and_generic_resume_command() {
        let payload = json!({
            "schema": "homeboy/agent-task-loop-controller-resume-result/v1",
            "loop_id": "loop-456",
            "claimed": true,
            "results": [
                { "action_id": "action-1", "status": "completed" },
                {
                    "action_id": "action-2",
                    "status": "failed",
                    "failure_summary": {
                        "action_id": "action-2",
                        "run_id": "agent-task-2",
                        "diagnostic": "executor returned exit code 1"
                    }
                }
            ],
            "controller": {
                "loop_id": "loop-456",
                "phase": "verify",
                "state": "running",
                "entities": {},
                "task_lineage": [],
                "next_actions": [
                    { "action_id": "action-1", "action": { "action": "spawn_task" }, "status": "completed" },
                    { "action_id": "action-2", "action": { "action": "spawn_task" }, "status": "failed" },
                    { "action_id": "action-3", "action": { "action": "wait" }, "status": "pending" }
                ]
            }
        });

        let summary =
            render_agent_task_summary(AgentTaskSummaryKind::Controller, &payload).unwrap();

        assert!(summary.contains("Current step: verify / action-3\n"));
        assert!(
            summary.contains("Actions: 1 pending / 0 running / 1 completed / 1 failed / 3 total\n")
        );
        assert!(summary.contains("Last failure: action-2: executor returned exit code 1\n"));
        assert!(summary.contains("Next: homeboy agent-task controller resume loop-456\n"));
    }

    #[test]
    fn code_production_metrics_skips_rejected_and_non_apply_artifacts_in_cook_outcomes() {
        let payload = json!({
            "aggregate": {
                "outcomes": [{
                    "task_id": "cell-1",
                    "artifacts": [
                        { "id": "real-patch", "kind": "patch", "size_bytes": 64 },
                        { "id": "empty-patch", "kind": "patch", "size_bytes": 0 },
                        { "id": "rejected-patch", "kind": "patch", "size_bytes": 64, "metadata": { "rejected": true } },
                        { "id": "false-positive", "kind": "diff", "size_bytes": 64, "metadata": { "false_positive": true } },
                        { "id": "transcript", "kind": "provider-transcript", "size_bytes": 4096 }
                    ]
                }]
            }
        });

        let metrics = code_production_metrics(&payload);

        assert_eq!(metrics.non_empty_patches, 1);
        assert_eq!(metrics.empty_patches, 1);
        assert_eq!(metrics.unknown_size_patches, 0);
        assert_eq!(metrics.diff_bytes, 64);
    }
}
