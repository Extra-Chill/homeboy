use serde_json::Value;

use super::agent_task::{AgentTaskArgs, AgentTaskCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTaskSummaryKind {
    Cook,
    Status,
    Review,
}

pub(crate) fn agent_task_summary_kind(args: &AgentTaskArgs) -> Option<AgentTaskSummaryKind> {
    match args.command {
        AgentTaskCommand::Cook(_) => Some(AgentTaskSummaryKind::Cook),
        AgentTaskCommand::Status(_) => Some(AgentTaskSummaryKind::Status),
        AgentTaskCommand::Review(_) => Some(AgentTaskSummaryKind::Review),
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
        AgentTaskSummaryKind::Review => render_review_summary(payload),
    }
}

fn render_cook_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let state = string_value(payload, &["state"])
        .or_else(|| string_value(payload, &["record", "state"]))
        .unwrap_or("unknown");
    let task_count = usize_value(payload, &["task_count"])
        .or_else(|| array_len(payload, &["record", "tasks"]))
        .unwrap_or(0);
    let aggregate_path = string_value(payload, &["aggregate_path"])
        .or_else(|| string_value(payload, &["record", "aggregate_path"]));
    let apply_candidates = usize_value(payload, &["aggregate", "totals", "apply_candidates"])
        .or_else(|| array_len(payload, &["aggregate", "outcomes"]));
    let artifact_count = array_len(payload, &["aggregate", "outcomes", "0", "artifacts"]);
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
        format!("Tasks: {task_count}"),
    ];
    if let Some(path) = aggregate_path {
        lines.push(format!("Aggregate: {path}"));
    }
    if let Some(count) = apply_candidates {
        lines.push(format!("Patch candidates: {count}"));
    }
    if let Some(count) = artifact_count {
        lines.push(format!("Artifacts: {count}"));
    }
    if let Some(artifact) = first_artifact {
        lines.push(format!("First artifact: {artifact}"));
    }
    lines.push(format!("Next: homeboy agent-task review {run_id}"));
    Some(finish(lines))
}

fn render_status_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let state = string_value(payload, &["state"]).unwrap_or("unknown");
    let task_count = array_len(payload, &["tasks"]).unwrap_or(0);
    let artifact_count = array_len(payload, &["artifact_refs"]).unwrap_or(0);
    let aggregate_path = string_value(payload, &["aggregate_path"]);

    let mut lines = vec![
        "Agent task status".to_string(),
        format!("Run: {run_id}"),
        format!("Status: {state}"),
        format!("Tasks: {task_count}"),
        format!("Artifacts: {artifact_count}"),
    ];
    if let Some(path) = aggregate_path {
        lines.push(format!("Aggregate: {path}"));
        lines.push(format!("Next: homeboy agent-task review {run_id}"));
    } else if state == "queued" {
        lines.push(format!("Next: homeboy agent-task run {run_id}"));
    } else {
        lines.push(format!("Next: homeboy agent-task logs {run_id}"));
    }
    Some(finish(lines))
}

fn render_review_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let state = string_value(payload, &["state"]).unwrap_or("unknown");
    let summary = value_at(payload, &["aggregate_review", "summary"]);
    let apply_candidates = summary
        .and_then(|_| {
            usize_value(
                payload,
                &["aggregate_review", "summary", "apply_candidates"],
            )
        })
        .unwrap_or_else(|| array_len(payload, &["promotion_candidates"]).unwrap_or(0));
    let failed = summary
        .and_then(|_| usize_value(payload, &["aggregate_review", "summary", "failed"]))
        .unwrap_or(0);
    let patch = string_value(payload, &["promotion_candidates", "0", "artifact_id"]);
    let patch_path = patch.and_then(|artifact_id| artifact_path(payload, artifact_id));
    let next = first_string(payload, &["next_actions"]);
    let command = command_line(payload, &["promotion_candidates", "0", "command"]);

    let outcome = if apply_candidates > 0 {
        "patch produced, not promoted"
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
        format!("Patch candidates: {apply_candidates}"),
    ];
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

fn value_at<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = payload;
    for segment in path {
        if let Ok(index) = segment.parse::<usize>() {
            current = current.as_array()?.get(index)?;
        } else {
            current = current.get(*segment)?;
        }
    }
    Some(current)
}

fn string_value<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(payload, path)?.as_str()
}

fn usize_value(payload: &Value, path: &[&str]) -> Option<usize> {
    value_at(payload, path)?.as_u64()?.try_into().ok()
}

fn array_len(payload: &Value, path: &[&str]) -> Option<usize> {
    Some(value_at(payload, path)?.as_array()?.len())
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
                    "artifacts": [{ "id": "patch", "path": "/tmp/patch.diff" }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.starts_with("Agent task cook\nRun: homeboy-4345\nStatus: succeeded"));
        assert!(summary.contains("Tasks: 1\n"));
        assert!(summary.contains("First artifact: /tmp/patch.diff\n"));
        assert!(summary.contains("Next: homeboy agent-task review homeboy-4345\n"));
        assert!(!summary.contains("{\n"));
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
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "patch.diff",
                    "kind": "patch",
                    "path": "/tmp/patch.diff"
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
        assert!(summary.contains("Patch candidates: 1\n"));
        assert!(summary.contains("Patch: /tmp/patch.diff\n"));
        assert!(summary
            .contains("Next: homeboy agent-task promote homeboy-4345 --artifact-id patch.diff\n"));
        assert!(!summary.contains("promotion_candidates"));
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
        assert!(summary.contains("Tasks: 1\n"));
        assert!(summary.contains("Artifacts: 0\n"));
        assert!(summary.contains("Next: homeboy agent-task run homeboy-4345\n"));
    }
}
