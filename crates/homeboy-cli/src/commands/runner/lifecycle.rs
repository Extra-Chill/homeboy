use clap::ValueEnum;
use serde::Serialize;
use serde_json::json;

use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy::core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycle,
    ResourceLifecycleInspection, ResourceLifecycleRecord, ResourceLifecycleResourceStatus,
};
use homeboy::core::run_lifecycle_status::{RunLifecycleStatus, RUN_LIFECYCLE_STATUS_SCHEMA};
use homeboy::core::run_outcome_envelope::RunOutcomeEnvelope;

use super::CmdResult;

pub const RUNNER_WORKSPACE_LIFECYCLE_SCHEMA: &str = "homeboy/runner-workspace-lifecycle/v1";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RunnerLifecycleStatusArg {
    Unknown,
    Queued,
    Running,
    Succeeded,
    PartialFailure,
    Failed,
    Cancelled,
    TimedOut,
    Stale,
}

#[derive(Debug, Serialize)]
pub struct RunnerLifecycleOutput {
    pub variant: &'static str,
    pub schema: &'static str,
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub workspace: RunnerLifecycleWorkspace,
    pub lifecycle: RunnerLifecycleStatusOutput,
    pub resource_lifecycle: ResourceLifecycleInspection,
    pub finalization: RunnerLifecycleFinalization,
    pub outcome: RunOutcomeEnvelope,
}

#[derive(Debug, Serialize)]
pub struct RunnerLifecycleWorkspace {
    pub path: String,
    pub kind: &'static str,
    pub owner: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RunnerLifecycleStatusOutput {
    pub schema: &'static str,
    pub status: RunLifecycleStatus,
    pub terminal: bool,
    pub success: bool,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct RunnerLifecycleFinalization {
    pub owner: &'static str,
    pub state: &'static str,
    pub reason: String,
    pub runner_should_finalize: bool,
    pub controller_should_finalize: bool,
    pub next_commands: Vec<RunnerLifecycleNextCommand>,
}

#[derive(Debug, Serialize)]
pub struct RunnerLifecycleNextCommand {
    pub label: &'static str,
    pub command: Vec<String>,
}

pub(super) fn lifecycle(
    runner_id: String,
    workspace: String,
    job_id: Option<String>,
    run_id: Option<String>,
    status: Option<RunnerLifecycleStatusArg>,
    exit_code: Option<i32>,
) -> CmdResult<RunnerLifecycleOutput> {
    let status = status
        .map(RunLifecycleStatus::from)
        .unwrap_or_else(|| status_from_exit_code(exit_code));
    let lifecycle = RunnerLifecycleStatusOutput {
        schema: RUN_LIFECYCLE_STATUS_SCHEMA,
        status,
        terminal: status.is_terminal(),
        success: status.is_success(),
        retryable: status.is_retryable(),
        exit_code,
    };
    let resource_record = ResourceLifecycleRecord {
        owner: "homeboy.runner".to_string(),
        run_id: run_id
            .clone()
            .or_else(|| job_id.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        runner_id: Some(runner_id.clone()),
        path: workspace.clone(),
        root_bound: None,
        kind: "runner_workspace".to_string(),
        ttl: None,
        cleanup_policy: cleanup_policy_for_status(status),
        evidence_retention: ResourceEvidenceRetention::Manifest,
        cleanup_intent: ResourceCleanupIntent::DryRun,
        cleanup_command: Some(format!(
            "homeboy runs resources --run-id {} --cleanup-plan",
            run_id.as_deref().or(job_id.as_deref()).unwrap_or("unknown")
        )),
        status: resource_status_for_status(status),
    };
    let resource_lifecycle = ResourceLifecycle::inspect(&resource_record);
    let finalization =
        finalization_for_status(&runner_id, job_id.as_deref(), run_id.as_deref(), status);
    let mut outcome = RunOutcomeEnvelope::new(status_label(status))
        .with_run_id(run_id.clone())
        .with_runner_id(Some(runner_id.clone()));
    outcome.exit_code = exit_code;
    outcome = outcome.with_result(json!({
        "schema": RUNNER_WORKSPACE_LIFECYCLE_SCHEMA,
        "runner_id": runner_id,
        "job_id": job_id,
        "workspace": workspace,
        "lifecycle": lifecycle,
        "resource_lifecycle": resource_lifecycle,
        "finalization": finalization,
    }));

    Ok((
        RunnerLifecycleOutput {
            variant: "lifecycle",
            schema: RUNNER_WORKSPACE_LIFECYCLE_SCHEMA,
            runner_id: outcome.runner_id.clone().unwrap_or_default(),
            job_id: outcome
                .result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
            run_id: outcome.run_id.clone(),
            workspace: RunnerLifecycleWorkspace {
                path: outcome
                    .result
                    .get("workspace")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string(),
                kind: "runner_workspace",
                owner: "homeboy.runner",
            },
            lifecycle,
            resource_lifecycle,
            finalization,
            outcome,
        },
        0,
    ))
}

impl From<RunnerLifecycleStatusArg> for RunLifecycleStatus {
    fn from(value: RunnerLifecycleStatusArg) -> Self {
        match value {
            RunnerLifecycleStatusArg::Unknown => Self::Unknown,
            RunnerLifecycleStatusArg::Queued => Self::Queued,
            RunnerLifecycleStatusArg::Running => Self::Running,
            RunnerLifecycleStatusArg::Succeeded => Self::Succeeded,
            RunnerLifecycleStatusArg::PartialFailure => Self::PartialFailure,
            RunnerLifecycleStatusArg::Failed => Self::Failed,
            RunnerLifecycleStatusArg::Cancelled => Self::Cancelled,
            RunnerLifecycleStatusArg::TimedOut => Self::TimedOut,
            RunnerLifecycleStatusArg::Stale => Self::Stale,
        }
    }
}

fn status_from_exit_code(exit_code: Option<i32>) -> RunLifecycleStatus {
    match exit_code {
        Some(0) => RunLifecycleStatus::Succeeded,
        Some(_) => RunLifecycleStatus::Failed,
        None => RunLifecycleStatus::Unknown,
    }
}

fn cleanup_policy_for_status(status: RunLifecycleStatus) -> ResourceCleanupPolicy {
    if status.is_success() {
        ResourceCleanupPolicy::DeleteOnSuccess
    } else if status.is_terminal() {
        ResourceCleanupPolicy::Manual
    } else {
        ResourceCleanupPolicy::Preserve
    }
}

fn resource_status_for_status(status: RunLifecycleStatus) -> ResourceLifecycleResourceStatus {
    if status.is_success() {
        ResourceLifecycleResourceStatus::CleanupPending
    } else if status.is_terminal() {
        ResourceLifecycleResourceStatus::Retained
    } else {
        ResourceLifecycleResourceStatus::Active
    }
}

fn finalization_for_status(
    runner_id: &str,
    job_id: Option<&str>,
    run_id: Option<&str>,
    status: RunLifecycleStatus,
) -> RunnerLifecycleFinalization {
    let (state, reason, controller_should_finalize) = if status.is_success() {
        (
            "ready",
            "runner workspace completed successfully; product-specific finalization belongs to the controller",
            true,
        )
    } else if status.is_terminal() {
        (
            "blocked",
            "runner workspace reached a terminal non-success status; inspect evidence before finalization",
            false,
        )
    } else {
        (
            "pending",
            "runner workspace is not terminal; finalization is not ready",
            false,
        )
    };

    let mut next_commands = Vec::new();
    if let Some(job_id) = job_id {
        next_commands.push(RunnerLifecycleNextCommand {
            label: "runner_job_logs",
            command: vec![
                "homeboy".to_string(),
                "runner".to_string(),
                "job".to_string(),
                "logs".to_string(),
                runner_id.to_string(),
                job_id.to_string(),
                "--compact".to_string(),
            ],
        });
    }
    if let Some(run_id) = run_id {
        next_commands.push(RunnerLifecycleNextCommand {
            label: "run_artifacts",
            command: vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "artifacts".to_string(),
                run_id.to_string(),
            ],
        });
    }

    RunnerLifecycleFinalization {
        owner: "controller",
        state,
        reason: reason.to_string(),
        runner_should_finalize: false,
        controller_should_finalize,
        next_commands,
    }
}

fn status_label(status: RunLifecycleStatus) -> &'static str {
    match status {
        RunLifecycleStatus::Unknown => "unknown",
        RunLifecycleStatus::Queued => "queued",
        RunLifecycleStatus::Running => "running",
        RunLifecycleStatus::Succeeded => "succeeded",
        RunLifecycleStatus::PartialFailure => "partial_failure",
        RunLifecycleStatus::Failed => "failed",
        RunLifecycleStatus::Cancelled => "cancelled",
        RunLifecycleStatus::TimedOut => "timed_out",
        RunLifecycleStatus::Stale => "stale",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_lifecycle_reports_controller_finalization_ready() {
        let (output, exit_code) = lifecycle(
            "lab-a".to_string(),
            "/runner/work/homeboy".to_string(),
            Some("job-1".to_string()),
            Some("run-1".to_string()),
            None,
            Some(0),
        )
        .expect("lifecycle output");
        let value = serde_json::to_value(&output).expect("json");

        assert_eq!(exit_code, 0);
        assert_eq!(value["variant"], "lifecycle");
        assert_eq!(value["schema"], RUNNER_WORKSPACE_LIFECYCLE_SCHEMA);
        assert_eq!(value["lifecycle"]["status"], "succeeded");
        assert_eq!(value["finalization"]["owner"], "controller");
        assert_eq!(value["finalization"]["state"], "ready");
        assert_eq!(value["finalization"]["runner_should_finalize"], false);
        assert_eq!(value["finalization"]["controller_should_finalize"], true);
        assert_eq!(
            value["outcome"]["schema"],
            "homeboy/run-outcome-envelope/v1"
        );
        assert_eq!(value["outcome"]["exit_code"], 0);
        assert_eq!(
            value["outcome"]["result"]["schema"],
            RUNNER_WORKSPACE_LIFECYCLE_SCHEMA
        );
    }

    #[test]
    fn failed_lifecycle_blocks_finalization_and_retains_workspace() {
        let (output, exit_code) = lifecycle(
            "lab-a".to_string(),
            "/runner/work/homeboy".to_string(),
            None,
            Some("run-1".to_string()),
            Some(RunnerLifecycleStatusArg::Failed),
            Some(2),
        )
        .expect("lifecycle output");
        let value = serde_json::to_value(&output).expect("json");

        assert_eq!(exit_code, 0);
        assert_eq!(value["lifecycle"]["terminal"], true);
        assert_eq!(value["lifecycle"]["retryable"], true);
        assert_eq!(value["finalization"]["state"], "blocked");
        assert_eq!(value["resource_lifecycle"]["cleanup_policy"], "manual");
        assert_eq!(value["resource_lifecycle"]["status"], "retained");
    }
}
