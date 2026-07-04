use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::core::activity::{self, ActivityItem, ActivityReport, ActivityScope, ActivityState};
use homeboy::core::notify::{self, NotifyEvent, NotifyOutcome};
use homeboy::core::observation::ObservationStore;
use homeboy::core::{Error, Result};

use super::utils::response::{
    CommandActionableMetadata, CommandAgentTaskRef, CommandJobRef, CommandNextAction,
    CommandNextActionKind, CommandResultRefs, CommandRunRef,
};
use super::{CmdResult, GlobalArgs};

const TIMEOUT_EXIT_CODE: i32 = 124;

#[derive(Args, Clone)]
pub struct ActivityArgs {
    #[command(subcommand)]
    command: Option<ActivityCommand>,
}

#[derive(Subcommand, Clone)]
enum ActivityCommand {
    /// List active and recent Homeboy work
    List(ActivityListArgs),
    /// Resolve and show one activity item by run/task/job id
    Show { id: String },
    /// Poll any activity item until it reaches a terminal state
    Watch(ActivityWatchArgs),
}

#[derive(Args, Clone)]
pub struct ActivityListArgs {
    /// Maximum activity items to return.
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Include older completed records instead of active + recent.
    #[arg(long)]
    all: bool,
}

#[derive(Args, Clone)]
pub struct ActivityWatchArgs {
    /// Activity id, observation run id, agent-task run id, or runner job id.
    pub id: String,
    /// Maximum time to wait before giving up (e.g. `30m`, `2h`, `7d`).
    #[arg(long)]
    pub timeout: Option<String>,
    /// Delay between status polls (e.g. `2s`, `1m`).
    #[arg(long, default_value = "2s")]
    pub interval: String,
    /// Emit a local completion notification when the item reaches a terminal state.
    #[arg(long)]
    pub notify: bool,
    /// Override HOMEBOY_NOTIFY_COMMAND. Implies `--notify`.
    #[arg(long, requires = "notify")]
    pub notify_command: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ActivityOutput {
    Report(ActivityReportOutput),
    Watch(ActivityWatchOutput),
}

#[derive(Serialize)]
pub struct ActivityReportOutput {
    #[serde(flatten)]
    pub report: ActivityReport,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "CommandActionableMetadata::is_empty"
    )]
    pub actionable: CommandActionableMetadata,
}

#[derive(Serialize)]
pub struct ActivityWatchOutput {
    pub schema: &'static str,
    pub command: &'static str,
    pub id: String,
    pub state: ActivityState,
    pub terminal: bool,
    pub timed_out: bool,
    pub waited_secs: u64,
    pub poll_count: u64,
    pub item: ActivityItem,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "CommandActionableMetadata::is_empty"
    )]
    pub actionable: CommandActionableMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<NotifyOutcome>,
}

pub fn run(args: ActivityArgs, _global: &GlobalArgs) -> CmdResult<ActivityOutput> {
    match args
        .command
        .unwrap_or(ActivityCommand::List(ActivityListArgs {
            limit: 20,
            all: false,
        })) {
        ActivityCommand::List(args) => list(args),
        ActivityCommand::Show { id } => show(&id),
        ActivityCommand::Watch(args) => watch(args),
    }
}

pub fn render_activity_summary(payload: &serde_json::Value) -> Option<String> {
    let report = payload.get("payload").or(Some(payload))?;
    let counts = report.get("counts")?;
    let items = report.get("items")?.as_array()?;
    let mut lines = Vec::new();
    lines.push(format!(
        "activity: total={} active={} running={} queued={} failed={} stale={}",
        counts.get("total")?.as_u64()?,
        counts.get("active")?.as_u64()?,
        counts.get("running")?.as_u64()?,
        counts.get("queued")?.as_u64()?,
        counts.get("failed")?.as_u64()?,
        counts.get("stale")?.as_u64()?,
    ));
    if items.is_empty() {
        lines.push("No active or recent Homeboy activity.".to_string());
        return Some(format!("{}\n", lines.join("\n")));
    }
    lines.push(format!(
        "{:<28} {:<14} {:<18} {}",
        "id", "state", "kind", "updated"
    ));
    for item in items {
        let id = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-");
        let state = item
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let kind = item
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-");
        let updated = item
            .get("updated_at")
            .or_else(|| item.get("created_at"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-");
        lines.push(format!(
            "{:<28} {:<14} {:<18} {}",
            truncate(id, 28),
            state,
            truncate(kind, 18),
            updated
        ));
        if let Some(actions) = item
            .get("next_actions")
            .and_then(serde_json::Value::as_array)
        {
            for action in actions.iter().take(2) {
                let label = action
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("next");
                let command = action
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                if !command.is_empty() {
                    lines.push(format!("  next: {label}: {command}"));
                }
            }
        }
    }
    Some(format!("{}\n", lines.join("\n")))
}

fn list(args: ActivityListArgs) -> CmdResult<ActivityOutput> {
    reconcile_runs_best_effort();
    let scope = if args.all {
        ActivityScope::All
    } else {
        ActivityScope::ActiveRecent
    };
    let report = activity::activity_report(scope, args.limit)?;
    let actionable = actionable_for_activity_report(&report);
    Ok((
        ActivityOutput::Report(ActivityReportOutput { report, actionable }),
        0,
    ))
}

fn show(id: &str) -> CmdResult<ActivityOutput> {
    reconcile_runs_best_effort();
    let report = activity::show_activity(id)?;
    let actionable = actionable_for_activity_report(&report);
    Ok((
        ActivityOutput::Report(ActivityReportOutput { report, actionable }),
        0,
    ))
}

fn watch(args: ActivityWatchArgs) -> CmdResult<ActivityOutput> {
    let interval = parse_duration(&args.interval)?;
    let timeout = args.timeout.as_deref().map(parse_duration).transpose()?;
    let started = Instant::now();
    let mut poll_count = 0;

    loop {
        reconcile_runs_best_effort();
        let item = activity::resolve_activity(&args.id)?;
        poll_count += 1;
        eprintln!(
            "activity {}: {:?} (poll {})",
            args.id, item.state, poll_count
        );
        if !item.state.is_active() {
            return Ok(watch_output(
                args,
                item,
                false,
                started.elapsed(),
                poll_count,
            ));
        }
        if let Some(timeout) = timeout {
            if started.elapsed() >= timeout {
                return Ok(watch_output(
                    args,
                    item,
                    true,
                    started.elapsed(),
                    poll_count,
                ));
            }
        }
        std::thread::sleep(interval);
    }
}

fn reconcile_runs_best_effort() {
    if let Ok(store) = ObservationStore::open_initialized() {
        let _ = crate::commands::runs::reconcile::reconcile_owned_stale_running_runs(&store, 1000);
    }
}

fn watch_output(
    args: ActivityWatchArgs,
    item: ActivityItem,
    timed_out: bool,
    waited: Duration,
    poll_count: u64,
) -> (ActivityOutput, i32) {
    let notify = maybe_notify(&args, &item, timed_out);
    let exit_code = if timed_out {
        TIMEOUT_EXIT_CODE
    } else if item.state.is_failure() {
        1
    } else {
        0
    };
    let next_actions = item
        .next_actions
        .iter()
        .map(|action| action.command.clone())
        .collect();
    let actionable = actionable_for_activity_item(&item);
    (
        ActivityOutput::Watch(ActivityWatchOutput {
            schema: activity::ACTIVITY_REPORT_SCHEMA,
            command: "activity.watch",
            id: args.id,
            state: item.state.clone(),
            terminal: !timed_out,
            timed_out,
            waited_secs: waited.as_secs(),
            poll_count,
            item,
            next_actions,
            actionable,
            notify,
        }),
        exit_code,
    )
}

fn actionable_for_activity_item(item: &ActivityItem) -> CommandActionableMetadata {
    let mut metadata = CommandActionableMetadata {
        refs: CommandResultRefs {
            runs: item
                .refs
                .run_id
                .as_deref()
                .map(activity_run_ref)
                .into_iter()
                .collect(),
            jobs: item
                .refs
                .runner_job_id
                .as_deref()
                .map(activity_job_ref)
                .into_iter()
                .collect(),
            agent_tasks: item
                .refs
                .agent_task_run_id
                .as_deref()
                .map(activity_agent_task_ref)
                .into_iter()
                .collect(),
        },
        next_actions: item
            .next_actions
            .iter()
            .map(|action| {
                CommandNextAction::new(action.label.clone(), action.command.clone())
                    .with_kind(action_kind_from_label(&action.label))
            })
            .collect(),
        artifacts: item
            .artifacts
            .iter()
            .map(|artifact| super::utils::response::CommandArtifactRef {
                id: artifact.id.clone(),
                kind: artifact.kind.clone(),
                uri: artifact.uri.clone(),
                semantic_key: None,
            })
            .collect(),
        evidence: item
            .evidence
            .iter()
            .map(|evidence| super::utils::response::CommandEvidenceRef {
                id: evidence.id.clone(),
                kind: evidence.kind.clone(),
                uri: evidence.uri.clone(),
                semantic_key: None,
            })
            .collect(),
        ..Default::default()
    };
    metadata.run = metadata.refs.runs.first().cloned();
    metadata
}

fn actionable_for_activity_report(report: &ActivityReport) -> CommandActionableMetadata {
    let mut metadata = CommandActionableMetadata::default();
    for item in report.items.iter().take(20) {
        let item_metadata = actionable_for_activity_item(item);
        if metadata.run.is_none() {
            metadata.run = item_metadata.run.clone();
        }
        metadata.refs.runs.extend(item_metadata.refs.runs);
        metadata.refs.jobs.extend(item_metadata.refs.jobs);
        metadata
            .refs
            .agent_tasks
            .extend(item_metadata.refs.agent_tasks);
        metadata.next_actions.extend(item_metadata.next_actions);
        metadata.artifacts.extend(item_metadata.artifacts);
        metadata.evidence.extend(item_metadata.evidence);
    }
    metadata
}

fn activity_run_ref(run_id: &str) -> CommandRunRef {
    CommandRunRef {
        id: run_id.to_string(),
        kind: "activity".to_string(),
        source: "homeboy-activity".to_string(),
        location: None,
        started_at: None,
        updated_at: None,
        finished_at: None,
        status_command: format!("homeboy runs show {run_id}"),
        watch_command: format!("homeboy runs watch {run_id}"),
    }
}

fn activity_job_ref(job_id: &str) -> CommandJobRef {
    CommandJobRef {
        id: job_id.to_string(),
        kind: "runner_job".to_string(),
        source: "homeboy-activity".to_string(),
        status_command: format!("homeboy activity show {job_id}"),
        watch_command: Some(format!("homeboy activity watch {job_id}")),
    }
}

fn activity_agent_task_ref(run_id: &str) -> CommandAgentTaskRef {
    CommandAgentTaskRef {
        id: run_id.to_string(),
        source: "homeboy-activity".to_string(),
        status_command: format!("homeboy agent-task status {run_id} --full"),
        logs_command: format!("homeboy agent-task logs {run_id}"),
        review_command: Some(format!("homeboy agent-task review {run_id}")),
    }
}

fn action_kind_from_label(label: &str) -> CommandNextActionKind {
    match label {
        "watch" => CommandNextActionKind::Watch,
        "artifacts" => CommandNextActionKind::Artifacts,
        "repair" | "reconcile" => CommandNextActionKind::Repair,
        _ => CommandNextActionKind::Show,
    }
}

fn maybe_notify(
    args: &ActivityWatchArgs,
    item: &ActivityItem,
    timed_out: bool,
) -> Option<NotifyOutcome> {
    if !args.notify && args.notify_command.is_none() {
        return None;
    }
    let status = if timed_out {
        "timed_out".to_string()
    } else {
        format!("{:?}", item.state).to_lowercase()
    };
    Some(notify::dispatch(
        &NotifyEvent {
            title: format!("homeboy activity {status}"),
            body: format!("{} {}", item.kind, item.id),
            status,
            run_id: item.id.clone(),
        },
        args.notify_command.as_deref(),
    ))
}

fn parse_duration(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (amount, unit) = trimmed.split_at(split);
    if amount.is_empty() || unit.is_empty() {
        return Err(Error::validation_invalid_argument(
            "duration",
            "expected duration like 2s, 30m, or 1h",
            Some(raw.to_string()),
            None,
        ));
    }
    let amount = amount.parse::<u64>().map_err(|_| {
        Error::validation_invalid_argument(
            "duration",
            "duration amount must be a positive integer",
            Some(raw.to_string()),
            None,
        )
    })?;
    let seconds = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => amount,
        "m" | "min" | "mins" | "minute" | "minutes" => amount * 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => amount * 60 * 60,
        "d" | "day" | "days" => amount * 60 * 60 * 24,
        _ => {
            return Err(Error::validation_invalid_argument(
                "duration",
                "duration unit must be s, m, h, or d",
                Some(raw.to_string()),
                None,
            ))
        }
    };
    Ok(Duration::from_secs(seconds))
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.to_string()
    } else {
        let mut truncated = value
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>();
        truncated.push('~');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_state_summary_is_explicit() {
        let rendered = render_activity_summary(&json!({
            "counts": {"total": 0, "active": 0, "running": 0, "queued": 0, "failed": 0, "stale": 0},
            "items": []
        }))
        .expect("summary");
        assert!(rendered.contains("No active or recent Homeboy activity."));
    }

    #[test]
    fn duration_parser_accepts_seconds() {
        assert_eq!(
            parse_duration("2s").expect("duration"),
            Duration::from_secs(2)
        );
    }
}
