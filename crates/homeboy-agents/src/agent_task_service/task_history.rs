//! `agent-task history --task-url`: every Cook and attempt for one task, read
//! from the durable run store rather than tracker comments (#18451).

use super::{
    discovery_identity, discovery_outcome, read_discovery_record_page, AgentTaskDiscoveryFilter,
    AgentTaskDiscoveryPageScope, AgentTaskDiscoverySearchStatus,
};
use crate::agent_task_lifecycle::{AgentTaskRunRecord, CookTerminalOutcome};
use crate::agent_task_service::CookDelivery;
use homeboy_core::{Error, Result};
use serde_json::Value;

pub const AGENT_TASK_HISTORY_SCHEMA: &str = "homeboy/agent-task-history/v1";
const HISTORY_SUMMARY_MAX_CHARS: usize = 280;

/// One bounded page of a task's attempt history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTaskHistoryOptions {
    pub task_url: String,
    pub limit: usize,
    /// Opaque continuation returned by the previous page.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskHistoryPage {
    pub schema: &'static str,
    pub task_url: String,
    pub count: usize,
    pub limit: usize,
    /// `search_window_exhausted` with an empty page means "none in this
    /// window", not "no history"; follow `next_cursor` to continue.
    pub search_status: AgentTaskDiscoverySearchStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Newest first.
    pub entries: Vec<AgentTaskHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskHistoryEntry {
    pub cook_id: String,
    pub run_id: String,
    /// `cook` for a Cook's own record, `attempt` for a run it owns.
    pub kind: &'static str,
    /// The task URL this run was admitted for: the queried URL, or the PR
    /// a PR-task rework targeted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    pub admitted_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub state: homeboy_lifecycle_contract::RunExecutionState,
    /// The Cook's terminal outcome kind, else its lifecycle state.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub summary: String,
}

/// A run belongs to a task's history when it was admitted for that task URL
/// or published the PR at that URL, so `--task-url <PR>` returns the Cook
/// that opened the PR and every PR-task rework of it.
pub fn task_history(options: AgentTaskHistoryOptions) -> Result<AgentTaskHistoryPage> {
    let task_url = options.task_url.trim().to_string();
    if task_url.is_empty() {
        return Err(Error::validation_invalid_argument(
            "task_url",
            "must name the task URL whose history is read",
            None,
            None,
        ));
    }
    let scope = AgentTaskDiscoveryPageScope {
        filter: "history".to_string(),
        repo: None,
        workspace: None,
        task_url: Some(task_url.clone()),
        submitted_after: None,
        resolved_submitted_after: None,
        state: None,
        placement: None,
        parent_id: None,
        branch: None,
    };
    let page = read_discovery_record_page(
        AgentTaskDiscoveryFilter::All,
        options.limit,
        options.cursor.as_deref(),
        scope,
        |record, _| {
            discovery_identity(record).task_url.as_deref() == Some(task_url.as_str())
                || published_pr_url(record).as_deref() == Some(task_url.as_str())
        },
    )?;
    let entries = page
        .records
        .into_iter()
        .map(history_entry)
        .collect::<Vec<_>>();
    Ok(AgentTaskHistoryPage {
        schema: AGENT_TASK_HISTORY_SCHEMA,
        task_url,
        count: entries.len(),
        limit: page.limit,
        search_status: page.search_status,
        next_cursor: page.next_cursor,
        entries,
    })
}

fn published_pr_url(record: &AgentTaskRunRecord) -> Option<String> {
    match &record.terminal_outcome {
        Some(CookTerminalOutcome::Published { pr, .. }) => Some(pr.clone()),
        _ => None,
    }
}

/// Project one durable record. The store derives `terminal_outcome` from the
/// record's delivery and failure facts on every write, so outcome, failure
/// class and PR URL read that one summary.
fn history_entry(record: AgentTaskRunRecord) -> AgentTaskHistoryEntry {
    let projected = discovery_outcome(&record);
    let state = projected.state();
    let terminal = record
        .terminal_outcome
        .clone()
        .or_else(|| projected.terminal_outcome.clone());
    let outcome = terminal
        .as_ref()
        .and_then(|outcome| serde_json::to_value(outcome).ok())
        .and_then(|outcome| outcome["kind"].as_str().map(str::to_string))
        .or_else(|| serde_json::to_value(state).ok()?.as_str().map(str::to_string))
        .unwrap_or_default();
    let failure_class = match &terminal {
        Some(CookTerminalOutcome::Infrastructure { error_code }) => Some(error_code.clone()),
        Some(CookTerminalOutcome::GateRefused { .. })
        | Some(CookTerminalOutcome::Published {
            refused_gate: Some(_),
            ..
        }) => Some("gate_refused".to_string()),
        Some(CookTerminalOutcome::AgentDeclined) => Some("agent_declined".to_string()),
        _ => None,
    };
    let delivery = CookDelivery::from_run_metadata(&record.metadata);
    let failure_message = delivery
        .as_ref()
        .and_then(|delivery| Some(delivery.blocker.as_ref()?.message.clone()))
        .or_else(|| {
            ["cook_controller_failure", "pre_execution_failure"]
                .iter()
                .find_map(|key| record.metadata.pointer(&format!("/{key}/message")))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let pr_url = published_pr_url(&record).or_else(|| published_pr_url(&projected));
    let candidate_head_sha = record
        .candidate
        .as_ref()
        .map(|candidate| candidate.candidate_sha.clone())
        .or_else(|| {
            let delivery = delivery.as_ref()?;
            delivery
                .candidate
                .as_ref()
                .and_then(|candidate| Some(candidate.candidate.as_ref()?.candidate_sha.clone()))
                .or_else(|| delivery.publication.as_ref()?.pushed_sha.clone())
        });
    let summary = match (&terminal, &failure_message, &pr_url) {
        (Some(CookTerminalOutcome::GateRefused { gate }), message, _) => match message {
            Some(message) => format!("{gate} refused: {message}"),
            None => format!("{gate} refused"),
        },
        (_, Some(message), _) => message.clone(),
        (_, None, Some(pr)) => format!("{outcome}: {pr}"),
        _ => outcome.clone(),
    };
    let cook_id = record.metadata["cook_id"]
        .as_str()
        .unwrap_or(&record.run_id)
        .to_string();
    AgentTaskHistoryEntry {
        kind: if cook_id == record.run_id {
            "cook"
        } else {
            "attempt"
        },
        cook_id,
        task_url: discovery_identity(&record).task_url,
        admitted_at: record.submitted_at.clone(),
        finished_at: record
            .lifecycle
            .execution
            .finished_at
            .clone()
            .or_else(|| projected.lifecycle.execution.finished_at.clone()),
        state,
        outcome,
        failure_class,
        candidate_head_sha,
        pr_url,
        summary: summary.chars().take(HISTORY_SUMMARY_MAX_CHARS).collect(),
        run_id: record.run_id,
    }
}
