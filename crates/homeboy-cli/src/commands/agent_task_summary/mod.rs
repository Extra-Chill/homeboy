use serde_json::Value;

use super::agent_task::candidate::{
    changed_files_for_artifact, classify_candidates, CandidateState,
};
use super::agent_task::{AgentTaskArgs, AgentTaskCommand, AgentTaskControllerCommand};
use super::summary_json::{array_len, string_value, u64_value, usize_value, value_at};

mod controller;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTaskSummaryKind {
    Cook,
    Status,
    Logs,
    Review,
    Controller,
    Providers,
}

pub(crate) fn agent_task_summary_kind(args: &AgentTaskArgs) -> Option<AgentTaskSummaryKind> {
    match &args.command {
        AgentTaskCommand::Cook(_) => Some(AgentTaskSummaryKind::Cook),
        AgentTaskCommand::Status(_) => Some(AgentTaskSummaryKind::Status),
        AgentTaskCommand::Logs(_) => Some(AgentTaskSummaryKind::Logs),
        AgentTaskCommand::Review(_) => Some(AgentTaskSummaryKind::Review),
        AgentTaskCommand::Providers(_) => Some(AgentTaskSummaryKind::Providers),
        AgentTaskCommand::Controller(controller_args) => match &controller_args.command {
            AgentTaskControllerCommand::Status(_)
            | AgentTaskControllerCommand::Diagnose(_)
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
        AgentTaskSummaryKind::Controller => controller::render_controller_summary(payload),
        AgentTaskSummaryKind::Providers => render_providers_summary(payload),
    }
}

fn render_providers_summary(payload: &Value) -> Option<String> {
    let summary = payload.get("operator_summary")?;
    let state = summary.get("state")?.as_str()?;
    let provider_count = array_len(payload, &["providers"]).unwrap_or(0);
    let mut lines = vec![
        "Agent task providers".to_string(),
        format!("Status: {state}"),
        format!("Providers shown: {provider_count}"),
    ];
    if state == "selection_required" {
        let choices = summary
            .get("selection_choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|choice| choice.get("backend").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if !choices.is_empty() {
            lines.push(format!("Choose backend: {}", choices.join(", ")));
        }
    }
    if let Some(next_action) = summary.get("next_action").and_then(Value::as_str) {
        lines.push(format!("Next: {next_action}"));
    }
    Some(lines.join("\n"))
}

fn render_cook_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"])?;
    let raw_state = string_value(payload, &["state"])
        .or_else(|| string_value(payload, &["record", "state"]))
        .unwrap_or("unknown");
    let tasks_planned = usize_value(payload, &["task_count"])
        .or_else(|| array_len(payload, &["record", "tasks"]))
        .unwrap_or(0);
    let canonical = classify_candidates(payload);
    let tasks_attempted = canonical
        .provider_executions
        .or_else(|| aggregate_outcome_count(payload))
        .unwrap_or(0);
    let aggregate_path = string_value(payload, &["aggregate_path"])
        .or_else(|| string_value(payload, &["record", "aggregate_path"]));
    let metrics = code_production_metrics(payload);
    let state = effective_run_state(
        raw_state,
        tasks_attempted,
        metrics.candidate_state,
        metrics.candidate_scan_degraded,
    );
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
    if metrics.candidate_state.is_available() {
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
    let canonical = classify_candidates(payload);
    let tasks_attempted = canonical
        .provider_executions
        .unwrap_or_else(|| status_attempted_task_count(payload));
    let metrics = code_production_metrics(payload);
    let state = string_value(payload, &["cook", "state"]).unwrap_or_else(|| {
        effective_run_state(
            raw_state,
            tasks_attempted,
            metrics.candidate_state,
            metrics.candidate_scan_degraded,
        )
    });
    let completion = cook_completion_summary(payload);
    let cook = cook_outcome_summary(payload, state, metrics.candidate_state, completion.as_ref());
    let artifact_count = array_len(payload, &["artifact_refs"]).unwrap_or(0);
    let aggregate_path = string_value(payload, &["aggregate_path"]);

    let mut lines = vec!["Agent task status".to_string()];
    if let Some(cook) = cook.as_ref() {
        lines.extend(cook.lines());
        lines.push(format!("Run: {run_id}"));
        lines.push("Provider/task evidence:".to_string());
    } else {
        lines.push(format!("Status: {state}"));
        lines.push(format!("Run: {run_id}"));
    }
    lines.extend([
        format!("Tasks planned: {tasks_planned}"),
        format!("Tasks attempted: {tasks_attempted}"),
    ]);
    let mut production_lines = code_production_lines(&metrics);
    if let Some(candidate) = string_value(payload, &["execution_states", "candidate", "state"]) {
        production_lines[1] = format!("Candidate state: {candidate}");
    }
    lines.extend(production_lines);
    if let Some(diagnostic) = first_actionable_diagnostic(payload) {
        lines.push(format!("Diagnostic: {diagnostic}"));
    }
    lines.push(format!("Artifacts: {artifact_count}"));
    if let Some(cook) = cook {
        lines.push(format!("Next: {}", cook.next_action(run_id)));
    } else if metrics.candidate_state.is_available() {
        if let Some(path) = aggregate_path {
            lines.push(format!("Aggregate: {path}"));
        }
        lines.push(format!("Next: homeboy agent-task review {run_id}"));
    } else if state == "queued" && !is_transport_proxy(payload) {
        lines.push(format!("Next: homeboy agent-task run {run_id}"));
    } else if let Some(action) = transport_proxy_next_action(payload) {
        lines.push(format!("Next: {action}"));
    } else {
        lines.push(format!("Next: homeboy agent-task logs {run_id}"));
    }
    Some(finish(lines))
}

/// The compact Cook outcome uses the existing lifecycle and completion
/// projections. Provider success remains evidence below it, never the headline.
struct CookOutcomeSummary<'a> {
    state: &'a str,
    publication: Option<&'a str>,
    candidate_state: CandidateState,
    gate_state: Option<&'a str>,
    completion: Option<&'a CookCompletionSummary<'a>>,
}

impl CookOutcomeSummary<'_> {
    fn lines(&self) -> Vec<String> {
        let candidate = if self.candidate_state.is_available() {
            "yes"
        } else {
            "no"
        };
        let finalization = self
            .completion
            .map(|completion| completion.finalization_state())
            .unwrap_or("unknown");
        let mut lines = vec![
            format!("Cook outcome: {}", self.state),
            format!("Candidate: {candidate} ({})", self.candidate_state.as_str()),
            format!("Gates: {}", self.gate_state.unwrap_or("not_run")),
            format!("PR finalization: {finalization}"),
        ];
        if let Some(pr_url) = self.completion.and_then(|completion| completion.pr_url) {
            lines.push(format!("Pull request: {pr_url}"));
        }
        if let Some(publication) = self.publication {
            lines.push(format!("Publication: {publication}"));
        }
        lines
    }

    fn next_action(&self, run_id: &str) -> String {
        self.completion
            .and_then(|completion| completion.next_action)
            .map(str::to_string)
            .unwrap_or_else(|| {
                if self.publication == Some("completed") && self.candidate_state.is_available() {
                    format!("homeboy agent-task review {run_id}")
                } else {
                    format!("homeboy agent-task diagnose {run_id} --full")
                }
            })
    }
}

fn cook_outcome_summary<'a>(
    payload: &'a Value,
    state: &'a str,
    candidate_state: CandidateState,
    completion: Option<&'a CookCompletionSummary<'a>>,
) -> Option<CookOutcomeSummary<'a>> {
    let cook = value_at(payload, &["cook"]).filter(|cook| !cook.is_null());
    if cook.is_none() && completion.is_none() {
        return None;
    }
    Some(CookOutcomeSummary {
        // Older durable records may have completion evidence but predate the
        // Cook-state projection. A legal finalization continuation is the
        // canonical `candidate_recoverable` Cook lifecycle state, not provider
        // success.
        state: if cook.is_none()
            && completion
                .is_some_and(|completion| completion.state == "candidate_awaiting_finalization")
        {
            "candidate_recoverable"
        } else {
            state
        },
        publication: string_value(payload, &["cook", "publication"]),
        candidate_state,
        gate_state: string_value(payload, &["execution_states", "gate", "state"]),
        completion,
    })
}

/// The Cook-level publication facts, projected from the `cook_completion`
/// record `agent-task diagnose` already reads. The record is attached only to
/// Cook attempts, so non-Cook agent-task runs render exactly as before (#12571).
struct CookCompletionSummary<'a> {
    state: &'a str,
    finalization_state: Option<&'a str>,
    finalization_requested: bool,
    pr_finalized: bool,
    pr_url: Option<&'a str>,
    next_action: Option<&'a str>,
}

impl CookCompletionSummary<'_> {
    fn finalization_state(&self) -> &str {
        if self.pr_finalized {
            "finalized"
        } else if !self.finalization_requested {
            "not_requested"
        } else {
            self.finalization_state.unwrap_or("not_finalized")
        }
    }
}

fn cook_completion_summary(payload: &Value) -> Option<CookCompletionSummary<'_>> {
    let completion = value_at(payload, &["cook_completion"])?;
    Some(CookCompletionSummary {
        state: string_value(completion, &["state"]).unwrap_or("unknown"),
        finalization_state: string_value(payload, &["execution_states", "finalization", "state"])
            .or_else(|| string_value(payload, &["cook_finalization", "status"]))
            .or_else(|| string_value(payload, &["metadata", "cook_finalization", "status"])),
        finalization_requested: value_at(completion, &["finalization_requested"])
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pr_finalized: value_at(completion, &["pr_finalized"])
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pr_url: cook_pr_url(payload),
        next_action: string_value(completion, &["next_action", "command"]),
    })
}

/// The pull request this Cook published, read from the projected `pr_url` the
/// status payload carries and, failing that, from the same durable finalization
/// receipts that make a candidate `finalized`.
fn cook_pr_url(payload: &Value) -> Option<&str> {
    let url = string_value(payload, &["pr_url"])
        .or_else(|| receipt_pr_url(payload, &["cook_finalization"]))
        .or_else(|| receipt_pr_url(payload, &["metadata", "cook_finalization"]))
        .or_else(|| receipt_pr_url(payload, &["finalization"]))?
        .trim();
    (!url.is_empty()).then_some(url)
}

fn receipt_pr_url<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    let receipt = value_at(payload, path)?;
    string_value(receipt, &["pr_url"]).or_else(|| string_value(receipt, &["pull_request_url"]))
}

fn is_transport_proxy(payload: &Value) -> bool {
    payload.get("transport_recovery").is_some()
        || string_value(payload, &["metadata", "kind"])
            .is_some_and(|kind| kind.ends_with("_controller_proxy"))
}

fn transport_proxy_next_action(payload: &Value) -> Option<String> {
    if let Some(command) = string_value(payload, &["transport_recovery", "command"]) {
        return Some(command.to_string());
    }
    if !is_transport_proxy(payload) {
        return None;
    }
    let runner_id = string_value(payload, &["metadata", "runner_id"])?;
    let job_id = string_value(payload, &["metadata", "runner_job_id"])
        .or_else(|| string_value(payload, &["metadata", "runner_execution_record", "job_id"]));
    Some(match job_id {
        Some(job_id) => format!("homeboy runner job logs {runner_id} {job_id} --follow"),
        None => format!("homeboy runner connect {runner_id}"),
    })
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
    let promotable = metrics.candidate_state == CandidateState::PatchAvailable;
    let patch = promotable
        .then(|| string_value(payload, &["promotion_candidates", "0", "artifact_id"]))
        .flatten();
    let patch_path = patch.and_then(|artifact_id| artifact_path(payload, artifact_id));
    let next = first_string(payload, &["next_actions"]);
    let command = promotable
        .then(|| command_line(payload, &["promotion_candidates", "0", "command"]))
        .flatten();

    let outcome = if metrics.candidate_state == CandidateState::Finalized {
        "pull request finalized"
    } else if metrics.candidate_state == CandidateState::Promoted {
        "patch promoted"
    } else if promotable {
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CodeProductionMetrics {
    non_empty_patches: usize,
    empty_patches: usize,
    unknown_size_patches: usize,
    diff_bytes: u64,
    changed_files: usize,
    /// Number of non-empty patches whose changed-file count could not be
    /// determined (no metadata and the patch content was unavailable or
    /// unparseable). Used to render `unknown` instead of a misleading verified
    /// zero (#9742).
    changed_files_unknown_patches: usize,
    candidate_state: CandidateState,
    candidate_scan_degraded: bool,
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
    // Distinguish an authoritative zero from an unknown count: if any non-empty
    // patch's change set could not be parsed, mark the total unknown rather than
    // claiming zero changed files for a substantive patch.
    let changed_files = if metrics.changed_files_unknown_patches > 0 {
        if metrics.changed_files > 0 {
            format!("Changed files: {} (+unknown)", metrics.changed_files)
        } else {
            "Changed files: unknown".to_string()
        }
    } else {
        format!("Changed files: {}", metrics.changed_files)
    };
    vec![
        patch_candidates,
        format!("Candidate state: {}", metrics.candidate_state.as_str()),
        changed_files,
        format!("Diff bytes: {}", metrics.diff_bytes),
    ]
}

fn code_production_metrics(payload: &Value) -> CodeProductionMetrics {
    let mut metrics = CodeProductionMetrics::default();
    let canonical = classify_candidates(payload);
    if let Some(patch) = selected_candidate_patch(payload) {
        match patch.size_bytes {
            Some(size) if size > 0 => {
                metrics.non_empty_patches = 1;
                metrics.diff_bytes = size;
                match patch.changed_files {
                    Some(count) => metrics.changed_files = count,
                    None => metrics.changed_files_unknown_patches = 1,
                }
            }
            Some(_) => metrics.empty_patches = 1,
            None => metrics.unknown_size_patches = 1,
        }
        metrics.candidate_state = canonical.state();
        metrics.candidate_scan_degraded = canonical.is_degraded();
        return metrics;
    }
    metrics.non_empty_patches = canonical.available;
    metrics.empty_patches = canonical.empty;
    metrics.diff_bytes = canonical.diff_bytes;
    metrics.changed_files = canonical.changed_files;
    metrics.changed_files_unknown_patches = canonical.changed_files_unknown_patches;
    metrics.unknown_size_patches = canonical.unknown
        + canonical.missing
        + canonical.unreadable
        + canonical.conflicting
        + canonical.retained_only;
    metrics.candidate_state = canonical.state();
    metrics.candidate_scan_degraded = canonical.is_degraded();
    metrics
}

struct PatchArtifact {
    size_bytes: Option<u64>,
    /// `Some(n)` when the changed-file count is known (from metadata or by
    /// parsing the patch content); `None` when it could not be determined.
    changed_files: Option<usize>,
}

fn selected_candidate_patch(payload: &Value) -> Option<PatchArtifact> {
    let candidate = payload.get("selected_candidate")?;
    let artifact = candidate.get("artifact")?;
    let changed_files = candidate
        .get("changed_files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .or_else(|| resolve_changed_files(artifact));
    Some(PatchArtifact {
        size_bytes: candidate
            .get("size_bytes")
            .and_then(Value::as_u64)
            .or_else(|| u64_value(artifact, &["size_bytes"])),
        changed_files,
    })
}

/// Resolve the number of files a patch artifact changes.
///
/// Prefers authoritative metadata (`changed_files` list or
/// `changed_file_count`). When metadata is absent, falls back to parsing the
/// unified-diff content at the artifact `path`. Returns `None` when the count
/// cannot be determined so callers can render `unknown` instead of a misleading
/// zero for a substantive patch (#9742).
fn resolve_changed_files(artifact: &Value) -> Option<usize> {
    changed_files_for_artifact(artifact)
}

/// A run whose lifecycle state is `succeeded` but that produced zero promotion
/// candidates did not actually patch anything. Surface that honestly as
/// `no_patch_produced` instead of advertising success (#4610).
fn effective_run_state(
    raw_state: &str,
    tasks_attempted: usize,
    candidate_state: CandidateState,
    candidate_scan_degraded: bool,
) -> &str {
    if raw_state == "succeeded"
        && candidate_state == CandidateState::Unknown
        && candidate_scan_degraded
    {
        "unknown"
    } else if raw_state == "succeeded" && tasks_attempted > 0 && !candidate_state.is_available() {
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
    fn providers_summary_presents_selection_without_calling_it_blocked() {
        let payload = json!({
            "providers": [{ "backend": "alpha" }, { "backend": "zeta" }],
            "operator_summary": {
                "state": "selection_required",
                "selection_choices": [
                    { "backend": "alpha", "command": "homeboy agent-task providers --backend alpha --validate-readiness" },
                    { "backend": "zeta", "command": "homeboy agent-task providers --backend zeta --validate-readiness" }
                ],
                "next_action": "homeboy agent-task providers --backend alpha --validate-readiness"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Providers, &payload)
            .expect("providers summary");

        assert_eq!(
            summary,
            "Agent task providers\nStatus: selection_required\nProviders shown: 2\nChoose backend: alpha, zeta\nNext: homeboy agent-task providers --backend alpha --validate-readiness"
        );
    }

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
    fn cook_summary_keeps_a_canonical_candidate_after_an_empty_retry() {
        let payload = json!({
            "run_id": "agent-task-canonical",
            "state": "succeeded",
            "task_count": 2,
            "attempts": [
                {
                    "aggregate": {
                        "outcomes": [{
                            "artifacts": [{
                                "id": "canonical-patch",
                                "kind": "patch",
                                "size_bytes": 32318,
                                "url": "homeboy://agent-task/run/agent-task-canonical/artifacts#task=cook&artifact=canonical-patch",
                                "metadata": { "executor_artifact_finalized": true }
                            }]
                        }]
                    }
                },
                {
                    "aggregate": {
                        "outcomes": [{
                            "artifacts": [{ "id": "empty-retry", "kind": "patch", "size_bytes": 0 }]
                        }]
                    }
                }
            ]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: succeeded\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Candidate state: patch_available\n"));
        assert!(summary.contains("Next: homeboy agent-task review agent-task-canonical\n"));
        assert!(!summary.contains("no_patch_produced"));
    }

    #[test]
    fn finalized_pr_summary_is_not_reported_as_an_unpromoted_empty_patch() {
        let payload = json!({
            "run_id": "agent-task-finalized",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": { "outcomes": [{ "artifacts": [{ "id": "empty", "kind": "patch", "size_bytes": 0 }] }] },
            "finalization": { "status": "review_ready", "pr_url": "https://example.test/pull/1" }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: succeeded\n"));
        assert!(summary.contains("Candidate state: finalized\n"));
        assert!(summary.contains("Next: homeboy agent-task review agent-task-finalized\n"));
        assert!(!summary.contains("no_patch_produced"));
    }

    #[test]
    fn oversized_empty_artifacts_keep_the_terminal_summary_unknown() {
        let artifacts = (0..257)
            .map(
                |index| json!({ "id": format!("empty-{index}"), "kind": "patch", "size_bytes": 0 }),
            )
            .collect::<Vec<_>>();
        let payload = json!({
            "run_id": "agent-task-truncated",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": { "outcomes": [{ "artifacts": artifacts }] }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: unknown\n"));
        assert!(summary.contains("Candidate state: unknown\n"));
        assert!(!summary.contains("Status: no_patch_produced\n"));
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
    fn cook_summary_parses_changed_files_from_patch_when_metadata_absent() {
        // Regression for #9742: a substantive patch with no changed-file
        // metadata must report the real count parsed from patch content, not 0.
        crate::test_support::with_isolated_home(|_| {
            let root = homeboy::core::artifact_root().expect("artifact root");
            std::fs::create_dir_all(&root).expect("create artifact root");
            let patch_path = root.join("patch.diff");
            let patch = "diff --git a/crates/a/src/x.rs b/crates/a/src/x.rs\n\
             --- a/crates/a/src/x.rs\n+++ b/crates/a/src/x.rs\n@@ -1 +1 @@\n-a\n+b\n\
             diff --git a/crates/a/src/y.rs b/crates/a/src/y.rs\n\
             --- a/crates/a/src/y.rs\n+++ b/crates/a/src/y.rs\n@@ -1 +1 @@\n-c\n+d\n\
             diff --git a/crates/a/src/z.rs b/crates/a/src/z.rs\n\
             --- a/crates/a/src/z.rs\n+++ b/crates/a/src/z.rs\n@@ -1 +1 @@\n-e\n+f\n";
            std::fs::write(&patch_path, patch).expect("write patch");

            let payload = json!({
                "run_id": "agent-task-9742",
                "state": "succeeded",
                "task_count": 1,
                "aggregate": {
                    "outcomes": [{
                        "task_id": "agent-task-9742",
                        "artifacts": [{
                            "id": "patch",
                            "kind": "patch",
                            "path": patch_path.to_str().unwrap(),
                            "size_bytes": patch.len(),
                            "url": "homeboy://agent-task/run/agent-task-9742/artifacts#task=agent-task-9742&artifact=patch",
                            "metadata": { "executor_artifact_finalized": true }
                        }]
                    }]
                }
            });

            let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

            assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
            assert!(
                summary.contains("Changed files: 3\n"),
                "expected 3 changed files parsed from patch content, got: {summary}"
            );
        });
    }

    #[test]
    fn review_summary_uses_the_promoted_candidate_fingerprint() {
        // A promoted candidate is no longer an apply candidate, but its durable
        // promotion fingerprint remains the authoritative review summary source.
        let payload = json!({
            "run_id": "agent-task-11805",
            "state": "succeeded",
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "promoted",
                "diff_bytes": 0,
                "counts": { "patch_available": 1 },
                "scan": { "degraded": false }
            },
            "selected_candidate": {
                "status": "applied",
                "artifact": { "id": "candidate", "kind": "patch" },
                "size_bytes": 7635,
                "changed_files": ["a.rs", "b.rs", "c.rs"]
            },
            "aggregate_review": { "summary": { "apply_candidates": 0, "failed": 0 } },
            "next_actions": ["finalize the pull request"]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: patch promoted\n"));
        assert!(summary.contains("Changed files: 3\n"));
        assert!(summary.contains("Diff bytes: 7635\n"));
    }

    #[test]
    fn review_summary_counts_recoverable_candidate_artifacts() {
        let payload = json!({
            "run_id": "agent-task-11805-recoverable",
            "state": "partial_failure",
            "aggregate_review": {
                "summary": { "apply_candidates": 0, "failed": 0 },
                "review_candidates": [{ "task_id": "task", "artifact_ids": ["candidate"] }],
                "artifact_inventory": [{
                    "task_id": "task",
                    "artifact_id": "candidate",
                    "kind": "patch",
                    "size_bytes": 7635,
                    "metadata": { "changed_files": ["a.rs", "b.rs", "c.rs"] }
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Changed files: 3\n"));
        assert!(summary.contains("Diff bytes: 7635\n"));
    }

    #[test]
    fn selection_required_timeout_recovery_has_matching_status_and_review_inventory() {
        // Three providers timed out from the scheduler's perspective but each
        // deferred cleanup harvested a distinct durable patch. There are no
        // normalized task entries, which previously made status say zero tasks
        // while review discarded all three candidates.
        let candidates = [
            ("timeout-recovered-1", 32_318, 2),
            ("timeout-recovered-2", 32_318, 3),
            ("timeout-recovered-3", 32_412, 4),
        ]
        .into_iter()
        .map(|(id, size_bytes, changed_file_count)| {
            json!({
                "id": id,
                "kind": "patch",
                "size_bytes": size_bytes,
                "url": format!("homeboy://agent-task/run/selection-required/artifacts#{id}"),
                "metadata": {
                    "executor_artifact_finalized": true,
                    "changed_file_count": changed_file_count,
                    "recovered_from": "scheduler_timeout",
                },
            })
        })
        .collect::<Vec<_>>();
        let aggregate = json!({
            "outcomes": [{ "task_id": "cook", "artifacts": candidates }],
        });
        let status = json!({
            "run_id": "selection-required",
            "state": "selection_required",
            "tasks": [],
            "metadata": { "provider_executions_consumed": 3 },
            "aggregate": aggregate,
        });
        let review = json!({
            "run_id": "selection-required",
            "state": "partial_recoverable",
            "record": { "metadata": { "provider_executions_consumed": 3 } },
            "aggregate": status["aggregate"].clone(),
            "aggregate_review": { "summary": { "apply_candidates": 3, "failed": 0 } },
        });

        let status_summary =
            render_agent_task_summary(AgentTaskSummaryKind::Status, &status).expect("status");
        let review_summary =
            render_agent_task_summary(AgentTaskSummaryKind::Review, &review).expect("review");

        for summary in [&status_summary, &review_summary] {
            assert!(
                summary.contains("Patch candidates: 3 non-empty / 0 empty\n"),
                "{summary}"
            );
            assert!(
                summary.contains("Candidate state: patch_available\n"),
                "{summary}"
            );
            assert!(summary.contains("Changed files: 9\n"), "{summary}");
            assert!(summary.contains("Diff bytes: 97048\n"), "{summary}");
        }
        assert!(
            status_summary.contains("Tasks attempted: 3\n"),
            "{status_summary}"
        );
    }

    #[test]
    fn cook_summary_reports_unknown_changed_files_when_content_unavailable() {
        // Non-empty patch, no metadata, and an unreadable path: report unknown
        // rather than a misleading verified zero.
        let payload = json!({
            "run_id": "agent-task-9742-unknown",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": {
                "outcomes": [{
                    "task_id": "agent-task-9742-unknown",
                    "artifacts": [{
                        "id": "patch",
                        "kind": "patch",
                        "path": "/nonexistent/does-not-exist.diff",
                        "size_bytes": 512
                    }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(
            summary.contains("Changed files: unknown\n"),
            "expected unknown changed-file count, got: {summary}"
        );
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
    fn review_summary_keeps_the_default_compact_apply_candidate() {
        // #11982: default review must preserve the same substantive candidate
        // selected for promotion even though full artifact inventories are omitted.
        let payload = json!({
            "run_id": "agent-task-11982",
            "state": "succeeded",
            "aggregate_review": { "summary": { "apply_candidates": 1, "failed": 0 } },
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "patch_available",
                "diff_bytes": 17394,
                "counts": { "patch_available": 1 },
                "scan": { "degraded": false }
            },
            "selected_candidate": {
                "status": "available",
                "task_id": "task-a",
                "artifact": {
                    "artifact_id": "patch-a",
                    "kind": "patch",
                    "path": "/tmp/patch-a.diff",
                    "metadata": { "changed_files": ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] }
                },
                "size_bytes": 17394,
                "changed_files": ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"]
            },
            "promotion_candidates": [{
                "artifact_id": "patch-a",
                "command": null,
                "destination_required": true
            }],
            "next_actions": ["rerun review with `homeboy agent-task review agent-task-11982 --to-worktree <managed-worktree>` to generate executable promotion commands for apply candidates"]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: patch produced, not promoted\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Candidate state: patch_available\n"));
        assert!(summary.contains("Changed files: 6\n"));
        assert!(summary.contains("Diff bytes: 17394\n"));
        assert!(summary.contains("Next: rerun review with `homeboy agent-task review agent-task-11982 --to-worktree <managed-worktree>` to generate executable promotion commands for apply candidates\n"));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
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
                        "message": "Requested provider \"example-oauth\" is not registered. Registered provider plugins: []"
                    }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains(
            "Diagnostic: Requested provider \"example-oauth\" is not registered. Registered provider plugins: []\n"
        ));
    }

    #[test]
    fn status_summary_labels_the_subject_lifecycle_state() {
        for state in ["queued", "running", "failed", "succeeded"] {
            let payload = json!({
                "run_id": "homeboy-4345",
                "state": state,
                "tasks": [{ "task_id": "homeboy-4345" }],
                "artifact_refs": []
            });

            let summary =
                render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

            assert!(
                summary.starts_with(&format!(
                    "Agent task status\nStatus: {state}\nRun: homeboy-4345"
                )),
                "{summary}"
            );
            assert!(summary.contains("Tasks planned: 1\n"));
            assert!(summary.contains("Tasks attempted: 0\n"));
            assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
            assert!(summary.contains("Artifacts: 0\n"));
            if state == "queued" {
                assert!(summary.contains("Next: homeboy agent-task run homeboy-4345\n"));
            }
            // A run with no Cook completion record is not a Cook, so the
            // publication lines never appear for it (#12571).
            assert!(!summary.contains("Cook completion:"), "{summary}");
            assert!(!summary.contains("PR finalized:"), "{summary}");
        }
    }

    #[test]
    fn status_summary_leads_with_a_gated_candidate_whose_finalization_failed() {
        // A provider success is subordinate evidence when a promoted, gated
        // candidate fails publication. This matches the durable shape from
        // agent-task-25f..., where recovery finalization remains legal.
        let payload = json!({
            "run_id": "agent-task-25f-fixture",
            "state": "finalization_failed",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [{ "task_id": "cook", "kind": "patch", "uri": "artifact://cook/patch.diff", "size_bytes": 32318 }],
            "cook": { "state": "finalization_failed", "publication": "blocked" },
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "promoted",
                "counts": {}, "scan": {}
            },
            "execution_states": {
                "candidate": { "state": "promoted_finalization_failed" },
                "gate": { "state": "passed" },
                "finalization": { "state": "finalization_failed" },
                "provider": [{ "task_id": "cook", "state": "succeeded" }]
            },
            "cook_completion": {
                "schema": "homeboy/agent-task-cook-completion/v1",
                "candidate_produced": true,
                "finalization_requested": true,
                "pr_finalized": false,
                "state": "candidate_awaiting_finalization",
                "next_action": {
                    "action": "finalize_pr",
                    "command": "homeboy agent-task finalize-pr --recover agent-task-25f-fixture"
                }
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(
            summary.starts_with(
                "Agent task status\nCook outcome: finalization_failed\nCandidate: yes (promoted)\nGates: passed\nPR finalization: finalization_failed"
            ),
            "{summary}"
        );
        assert!(!summary.contains("Status: succeeded"), "{summary}");
        assert!(summary.contains("Candidate state: promoted_finalization_failed\n"));
        assert!(summary.contains("Publication: blocked\n"));
        assert!(summary.contains("Provider/task evidence:\n"));
        assert!(summary.contains("Tasks attempted: 1\n"));
        assert!(summary
            .contains("Next: homeboy agent-task finalize-pr --recover agent-task-25f-fixture\n"));
        assert!(!summary.contains("Pull request:"), "{summary}");
    }

    #[test]
    fn status_summary_surfaces_the_finalized_pull_request() {
        let payload = json!({
            "run_id": "agent-task-finalized",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [{ "task_id": "cook", "kind": "patch", "uri": "artifact://cook/patch.diff", "size_bytes": 32318 }],
            "cook": { "state": "review_ready", "publication": "completed" },
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "finalized",
                "counts": {}, "scan": {}
            },
            "execution_states": {
                "candidate": { "state": "finalized" },
                "gate": { "state": "passed" },
                "finalization": { "state": "review_ready" }
            },
            "cook_completion": {
                "schema": "homeboy/agent-task-cook-completion/v1",
                "candidate_produced": true,
                "finalization_requested": true,
                "pr_finalized": true,
                "state": "pr_finalized"
            },
            "pr_url": "https://example.test/pull/1"
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.starts_with(
            "Agent task status\nCook outcome: review_ready\nCandidate: yes (finalized)\nGates: passed\nPR finalization: finalized\nPull request: https://example.test/pull/1"
        ));
        assert!(summary.contains("Pull request: https://example.test/pull/1\n"));
        assert!(summary.contains("Next: homeboy agent-task review agent-task-finalized\n"));
    }

    #[test]
    fn status_summary_reads_the_pull_request_from_the_durable_finalization_receipt() {
        let payload = json!({
            "run_id": "agent-task-receipt",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [],
            "cook_completion": {
                "schema": "homeboy/agent-task-cook-completion/v1",
                "candidate_produced": true,
                "finalization_requested": true,
                "pr_finalized": true,
                "state": "pr_finalized"
            },
            "metadata": {
                "cook_finalization": { "status": "review_ready", "pr_url": "https://example.test/pull/2" }
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("PR finalization: finalized\n"));
        assert!(summary.contains("Pull request: https://example.test/pull/2\n"));
    }

    #[test]
    fn status_summary_keeps_a_no_finalize_cook_reported_as_succeeded() {
        // `--no-finalize` remains a successful Cook outcome without claiming a
        // pull request was finalized.
        let payload = json!({
            "run_id": "agent-task-no-finalize",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [{ "task_id": "cook", "kind": "patch", "uri": "artifact://cook/patch.diff", "size_bytes": 32318 }],
            "cook_completion": {
                "schema": "homeboy/agent-task-cook-completion/v1",
                "candidate_produced": true,
                "finalization_requested": false,
                "pr_finalized": false,
                "state": "candidate_produced"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Cook outcome: succeeded\n"), "{summary}");
        assert!(summary.contains("PR finalization: not_requested\n"));
    }

    #[test]
    fn status_summary_never_advertises_provider_run_for_transport_proxy() {
        let payload = json!({
            "run_id": "homeboy-transport",
            "state": "queued",
            "tasks": [{ "task_id": "homeboy-transport" }],
            "artifact_refs": [],
            "metadata": {
                "kind": "remote_controller_proxy",
                "runner_id": "runner-transport-42"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(!summary.contains("homeboy agent-task run homeboy-transport"));
        assert!(summary.contains("Next: homeboy runner connect runner-transport-42"));
    }

    #[test]
    fn status_summary_uses_authoritative_transport_recovery_guidance() {
        let payload = json!({
            "run_id": "homeboy-transport",
            "state": "queued",
            "tasks": [{ "task_id": "homeboy-transport" }],
            "artifact_refs": [],
            "transport_recovery": {
                "condition": "runner_busy_waiting_for_capacity",
                "command": "homeboy runner status runner-transport-42"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Next: homeboy runner status runner-transport-42"));
        assert!(!summary.contains("homeboy runner connect runner-transport-42"));
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
    fn status_summary_classifies_recovered_finalized_patch_refs_like_review() {
        let patch = json!({
            "id": "patch",
            "kind": "patch",
            "url": "homeboy://agent-task/run/recovered/artifacts#task=cook&artifact=patch",
            "size_bytes": 683500,
            "sha256": "fe060d978ff0d4ad0705a759308728ae29250c1b07587fc5ba8d0223262d9deb",
            "metadata": {
                "executor_artifact_finalized": true,
                "source_provenance": { "runner_id": "homeboy-lab" }
            }
        });
        let payload = json!({
            "run_id": "recovered",
            "state": "succeeded",
            "aggregate_path": "/tmp/recovered-aggregate.json",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [
                { "task_id": "cook", "kind": "patch", "uri": patch["url"], "size_bytes": 683500 },
                { "task_id": "cook", "kind": "transcript", "uri": "file:///tmp/transcript", "size_bytes": 12 },
                { "task_id": "cook", "kind": "json", "uri": "file:///tmp/result", "size_bytes": 4 },
                { "task_id": "cook", "kind": "runtime_log", "uri": "file:///tmp/runtime", "size_bytes": 20 }
            ],
            "aggregate": {
                "outcomes": [{
                    "artifacts": [patch],
                    "typed_artifacts": [{
                        "name": "patch",
                        "type": "file",
                        "artifact": { "id": "patch", "kind": "patch", "path": "/tmp/finalized-patch", "size_bytes": 683500 }
                    }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Status: succeeded\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Diff bytes: 683500\n"));
        assert!(summary.contains("Artifacts: 4\n"));
        assert!(summary.contains("Next: homeboy agent-task review recovered\n"));
    }

    #[test]
    fn lab_restart_summary_uses_the_32318_byte_canonical_mirror_not_a_stale_alias() {
        let payload = json!({
            "run_id": "lab-restarted",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook-intelligence", "state": "succeeded" }],
            "artifact_refs": [{ "task_id": "cook-intelligence", "kind": "patch", "uri": "runner-artifact://stale-alias" }],
            "aggregate": { "outcomes": [{ "task_id": "cook-intelligence", "artifacts": [{
                "id": "patch", "kind": "patch", "size_bytes": 32318,
                "url": "homeboy://agent-task/run/lab-restarted/artifacts#task=cook-intelligence&artifact=patch",
                "metadata": { "executor_artifact_finalized": true, "source_provenance": { "runner_id": "homeboy-lab" } }
            }] }] }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Status: succeeded\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Diff bytes: 32318\n"));
        assert!(summary.contains("Next: homeboy agent-task review lab-restarted\n"));
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
                "message": "Requested provider \"example-oauth\" is not registered. Registered provider plugins: []"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains(
            "Diagnostic: Requested provider \"example-oauth\" is not registered. Registered provider plugins: []\n"
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
                "message": "Requested provider \"example-oauth\" is not registered. Registered provider plugins: []"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Logs, &payload).unwrap();

        assert!(summary.starts_with("Agent task logs\nRun: agent-task-d1622a44\nEvents: 1\n"));
        assert!(summary.contains(
            "Diagnostic: Requested provider \"example-oauth\" is not registered. Registered provider plugins: []\n"
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
    fn controller_status_summary_prefers_failed_child_root_cause() {
        let payload = json!({
            "schema": "homeboy/agent-task-loop-controller-status/v1",
            "controller": {
                "loop_id": "loop-123",
                "phase": "triage",
                "state": "running",
                "entities": {},
                "task_lineage": [],
                "next_actions": [{
                    "action_id": "action-1",
                    "action": { "action": "spawn_task" },
                    "status": "failed",
                    "diagnostics": [{ "message": "child run failed" }]
                }]
            },
            "diagnostics": {
                "failed_child_actions": [{
                    "action_id": "action-1",
                    "child_run_id": "agent-task-child-1",
                    "child_run_status": "failed",
                    "top_diagnostic": "Agent runtime did not produce required typed artifacts: concept_packet, design_packet.",
                    "hydrated_root_cause": "Provider runtime import failed: module not found",
                    "owner_surface": "agent_runtime",
                    "next_command": "homeboy agent-task status agent-task-child-1 --full"
                }]
            }
        });

        let summary =
            render_agent_task_summary(AgentTaskSummaryKind::Controller, &payload).unwrap();

        assert!(summary.contains(
            "Last failure: action-1 (agent-task-child-1): Provider runtime import failed: module not found\n"
        ));
        assert!(summary.contains("Next: homeboy agent-task status agent-task-child-1 --full\n"));
    }

    #[test]
    fn controller_status_summary_surfaces_blocked_state_and_selected_executor() {
        let payload = json!({
            "schema": "homeboy/agent-task-loop-controller-status/v1",
            "controller": {
                "loop_id": "loop-123",
                "phase": "triage",
                "state": "running",
                "entities": {},
                "task_lineage": [],
                "next_actions": [{
                    "action_id": "action-1",
                    "action": { "action": "spawn_task" },
                    "status": "failed"
                }]
            },
            "diagnostics": {
                "controller_state": {
                    "state": "running_blocked_failed_action",
                    "label": "running but blocked on failed action",
                    "actionable": true,
                    "reason": "controller is marked running, but a failed or blocked action must be resolved before ordinary progress is safe"
                },
                "relevant_action": {
                    "action_id": "action-1",
                    "action": "spawn_task",
                    "status": "failed",
                    "selected_executor": {
                        "backend": "old-backend",
                        "selector": "old-selector",
                        "model": "old-model"
                    }
                },
                "next_commands": ["homeboy agent-task controller diagnose loop-123  # inspect failed action evidence"]
            }
        });

        let summary =
            render_agent_task_summary(AgentTaskSummaryKind::Controller, &payload).unwrap();

        assert!(summary.contains("State: running\n"));
        assert!(summary.contains("Controller state: running but blocked on failed action"));
        assert!(summary.contains(
            "Selected executor: backend=old-backend / selector=old-selector / model=old-model\n"
        ));
        assert!(summary.contains(
            "Next: homeboy agent-task controller diagnose loop-123  # inspect failed action evidence\n"
        ));
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
