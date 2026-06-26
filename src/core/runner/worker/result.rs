use base64::Engine;
use serde_json::json;

use crate::command_contract::AgentTaskDispatchIdentity;
use crate::core::api_jobs::{
    Job, JobArtifactMetadata, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::core::runner::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_output;

use super::super::capabilities::RunnerCapabilityPreflight;
use super::types::{ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput};

pub(super) fn remote_runner_result_from_exec_output(
    exec_output: super::super::execution::RunnerExecOutput,
    exit_code: i32,
    runner_workload: Option<crate::command_contract::RunnerWorkload>,
) -> RemoteRunnerJobResult {
    let patch = exec_output.patch.clone();
    let mutation_artifacts = exec_output.mutation_artifacts.clone();
    let mut data = json!({
        "mode": exec_output.mode,
        "remote_cwd": exec_output.remote_cwd,
    });
    if let Some(patch) = patch.clone() {
        data["patch"] = patch;
    }
    if let Some(mutation_artifacts) = mutation_artifacts.clone() {
        data["mutation_artifacts"] =
            serde_json::to_value(&mutation_artifacts).unwrap_or(serde_json::Value::Null);
    }
    if let Some(mirror_run_id) = exec_output.mirror_run_id.clone() {
        data["mirror_run_id"] = json!(mirror_run_id);
    }
    if let Some(lifecycle_event) = agent_task_run_plan_lifecycle_event_from_output(
        AgentTaskDispatchIdentity {
            runner_id: exec_output.runner_id.clone(),
            runner_job_id: exec_output.job_id.clone().unwrap_or_default(),
            persisted_run_id: exec_output.mirror_run_id.clone(),
            run_id: exec_output.mirror_run_id.clone(),
            handoff_id: exec_output
                .job_id
                .as_ref()
                .map(|job_id| format!("runner:{}:job:{job_id}", exec_output.runner_id)),
        },
        &exec_output.stdout,
    )
    .ok()
    .flatten()
    {
        data["agent_task_lifecycle_event"] =
            serde_json::to_value(lifecycle_event).unwrap_or(serde_json::Value::Null);
    }
    if let Some(runner_workload) = runner_workload {
        data["runner_workload"] =
            serde_json::to_value(super::super::workload::runner_workload_with_result_refs(
                runner_workload,
                exec_output.job_id.as_deref(),
                exec_output.mirror_run_id.as_deref(),
                &exec_output.artifacts,
            ))
            .unwrap_or(serde_json::Value::Null);
    }
    let artifacts = mirror_file_artifact_content(exec_output.artifacts, &exec_output.remote_cwd);
    RemoteRunnerJobResult {
        exit_code,
        stdout: Some(exec_output.stdout),
        stderr: Some(exec_output.stderr),
        patch,
        mutation_artifacts,
        data: Some(data),
        observation_run_ids: exec_output.mirror_run_id.into_iter().collect(),
        artifacts,
        artifact_refs: exec_output
            .runner_result
            .map(|result| {
                result
                    .artifact_refs
                    .into_iter()
                    .map(|artifact| JobArtifactMetadata {
                        id: artifact.artifact_id,
                        name: artifact.name,
                        path: artifact.path,
                        url: artifact.url,
                        mime: artifact.mime,
                        size_bytes: artifact.size_bytes,
                        sha256: artifact.sha256,
                        content_base64: None,
                        metadata: None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        metrics: exec_output.metrics,
        capture: exec_output.capture,
    }
}

fn mirror_file_artifact_content(
    artifacts: Vec<JobArtifactMetadata>,
    remote_cwd: &str,
) -> Vec<JobArtifactMetadata> {
    artifacts
        .into_iter()
        .map(|mut artifact| {
            if artifact.content_base64.is_none() {
                if let Some(path) = artifact.path.as_deref().map(|path| {
                    let path = std::path::PathBuf::from(path);
                    if path.is_absolute() {
                        path
                    } else {
                        std::path::Path::new(remote_cwd).join(path)
                    }
                }) {
                    if path.is_file() {
                        if let Ok(content) = std::fs::read(&path) {
                            artifact.size_bytes = artifact
                                .size_bytes
                                .or_else(|| u64::try_from(content.len()).ok());
                            artifact.content_base64 =
                                Some(base64::engine::general_purpose::STANDARD.encode(content));
                        }
                    }
                }
            }
            artifact
        })
        .collect()
}

pub(super) fn cancelled_output(
    options: ReverseRunnerWorkerOptions,
    iterations: u64,
    jobs_claimed: u64,
    broker_failures: u32,
    stopped: bool,
    job: Job,
) -> (ReverseRunnerWorkerOutput, i32) {
    let exit_code = 0;
    (
        claimed_output(
            options,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped,
            job,
            exit_code,
        ),
        exit_code,
    )
}

/// Build the remote capability-parity preflight for a claimed reverse-runner
/// job. The claimed command's executable (its first argv element) must be
/// available on this runner before execution starts, mirroring the direct
/// `runner exec` path's preflight contract (#5093).
pub(super) fn reverse_worker_capability_preflight(
    request: &RemoteRunnerJobRequest,
) -> Option<RunnerCapabilityPreflight> {
    let required_commands: Vec<String> = request
        .command
        .first()
        .filter(|program| !program.trim().is_empty())
        .cloned()
        .into_iter()
        .collect();
    if required_commands.is_empty() {
        return None;
    }
    Some(RunnerCapabilityPreflight {
        command: "runner.work".to_string(),
        required_commands,
        ..Default::default()
    })
}

pub(super) fn claimed_output(
    options: ReverseRunnerWorkerOptions,
    iterations: u64,
    jobs_claimed: u64,
    broker_failures: u32,
    stopped: bool,
    job: Job,
    exit_code: i32,
) -> ReverseRunnerWorkerOutput {
    let loop_mode = options.loop_mode;
    ReverseRunnerWorkerOutput {
        variant: "work",
        command: "runner.work",
        runner_id: options.runner_id,
        broker_url: options.broker_url,
        claimed: true,
        loop_mode,
        iterations: if loop_mode { iterations } else { 0 },
        jobs_claimed: if loop_mode { jobs_claimed + 1 } else { 0 },
        broker_failures: if loop_mode { broker_failures } else { 0 },
        stopped,
        last_claim: if loop_mode {
            Some(job.id.to_string())
        } else {
            None
        },
        last_result: if loop_mode { Some(exit_code) } else { None },
        last_error: if loop_mode && exit_code != 0 {
            Some(format!("job exited with code {exit_code}"))
        } else {
            None
        },
        job: Some(job),
        exit_code: Some(exit_code),
    }
}

pub(super) fn log_worker_event(
    options: &ReverseRunnerWorkerOptions,
    event: &str,
    data: serde_json::Value,
) {
    eprintln!(
        "{}",
        json!({
            "command": "runner.work",
            "event": event,
            "runner_id": options.runner_id,
            "broker_url": options.broker_url,
            "project_id": options.project_id,
            "data": data,
        })
    );
}
