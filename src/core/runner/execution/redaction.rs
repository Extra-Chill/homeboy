use std::collections::HashMap;

use serde_json::Value;

use crate::core::api_jobs::{Job, JobArtifactMetadata, JobEvent, JobStatus};
use crate::core::redaction::RedactionPolicy;
use crate::core::source_snapshot::SourceSnapshot;

use super::super::{
    Runner, RunnerArtifactRef, RunnerHandoff, RunnerJob, RunnerLifecycleOwner,
    RunnerMutationArtifacts, RunnerResult,
};

#[allow(unused_imports)]
use super::*;

pub(super) fn redact_runner_exec_streams(
    stdout: String,
    stderr: String,
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> (String, String) {
    let policy = RedactionPolicy::default();
    let secrets = runner_exec_secret_values(env, secret_env_names, &policy);
    (
        redact_runner_exec_text(&stdout, &policy, &secrets),
        redact_runner_exec_text(&stderr, &policy, &secrets),
    )
}

pub(super) fn runner_exec_secret_values(
    env: &HashMap<String, String>,
    secret_env_names: &[String],
    policy: &RedactionPolicy,
) -> Vec<String> {
    let mut values = env
        .iter()
        .filter_map(|(key, value)| {
            if value.is_empty() {
                return None;
            }
            if policy.is_sensitive_key(key) || secret_env_names.iter().any(|name| name == key) {
                Some(value.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

pub(super) fn redact_runner_exec_text(
    text: &str,
    policy: &RedactionPolicy,
    secret_values: &[String],
) -> String {
    let mut redacted = policy.redact_string(text);
    for value in secret_values {
        redacted = redacted.replace(value, policy.replacement());
    }
    redacted
}

pub(super) fn redact_runner_job_events(
    events: &[JobEvent],
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> Vec<JobEvent> {
    let policy = RedactionPolicy::default();
    let secrets = runner_exec_secret_values(env, secret_env_names, &policy);
    events
        .iter()
        .map(|event| {
            let mut redacted = event.clone();
            redacted.message = redacted
                .message
                .as_deref()
                .map(|message| redact_runner_exec_text(message, &policy, &secrets));
            redacted.data = redacted
                .data
                .as_ref()
                .map(|data| redact_runner_exec_json(data, &policy, &secrets));
            redacted
        })
        .collect()
}

pub(super) fn redact_runner_exec_json(
    value: &Value,
    policy: &RedactionPolicy,
    secret_values: &[String],
) -> Value {
    match policy.redact_json(value) {
        Value::String(text) => Value::String(redact_runner_exec_text(&text, policy, secret_values)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_runner_exec_json(item, policy, secret_values))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        redact_runner_exec_json(value, policy, secret_values),
                    )
                })
                .collect(),
        ),
        redacted => redacted,
    }
}

pub(super) fn runner_exec_diagnostics(
    runner: &Runner,
    source_snapshot: Option<&SourceSnapshot>,
    required_paths: &[String],
) -> Option<RunnerExecDiagnostics> {
    if required_paths.is_empty()
        && source_snapshot
            .and_then(|snapshot| snapshot.remote_path.as_ref())
            .is_none()
    {
        return None;
    }
    let mut hints = Vec::new();
    if let Some(remote_path) = source_snapshot.and_then(|snapshot| snapshot.remote_path.as_ref()) {
        hints.push(format!(
            "Reuse this runner workspace with `homeboy runner exec {} --cwd {} -- <command>`.",
            shell_arg(&runner.id),
            shell_arg(remote_path)
        ));
        hints.push(format!(
            "Discover recent runner workspaces with `homeboy runner workspace list {}`.",
            shell_arg(&runner.id)
        ));
    }
    if !required_paths.is_empty() {
        hints.push(
            "Use the generated _lab_workspaces/... snapshot path when the controller worktree path was synced into a lab snapshot."
                .to_string(),
        );
        hints.push(
            "Use --require-path to preflight paths that a command will reference before running it."
                .to_string(),
        );
    }

    Some(RunnerExecDiagnostics {
        runner_workspace_root: runner.workspace_root.clone(),
        source_snapshot_remote_path: source_snapshot
            .and_then(|snapshot| snapshot.remote_path.clone()),
        required_paths: required_paths.to_vec(),
        hints,
    })
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn runner_result(
    job: Option<&Job>,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    mirror_run_id: Option<&str>,
    mutation_artifacts: Option<RunnerMutationArtifacts>,
) -> RunnerResult {
    RunnerResult {
        exit_code,
        status: job.map(|job| job.status).unwrap_or_else(|| {
            if exit_code == 0 {
                JobStatus::Succeeded
            } else {
                JobStatus::Failed
            }
        }),
        stdout_bytes: Some(stdout.len()),
        stderr_bytes: Some(stderr.len()),
        mirror_run_id: mirror_run_id.map(str::to_string),
        mutation_artifacts,
        artifact_refs: job
            .map(|job| job.artifacts.iter().map(Into::into).collect())
            .unwrap_or_default(),
    }
}

pub(super) fn runner_handoff(
    runner: &Runner,
    transport: &str,
    job: Option<RunnerJob>,
    result: Option<RunnerResult>,
) -> RunnerHandoff {
    RunnerHandoff {
        runner_id: runner.id.clone(),
        transport: transport.to_string(),
        lifecycle_owner: match transport {
            "local" => RunnerLifecycleOwner::Local,
            "reverse_broker" => RunnerLifecycleOwner::Broker,
            _ => RunnerLifecycleOwner::Controller,
        },
        job,
        workspace_lease: None,
        workspace_leases: None,
        result,
    }
}

pub(super) fn mutation_artifacts_from_job(
    job: &Job,
    result: &Value,
) -> Option<RunnerMutationArtifacts> {
    let patch_artifact_id = result
        .get("patch")
        .or_else(|| result.pointer("/data/patch"))
        .and_then(|patch| {
            patch
                .get("patch_artifact_id")
                .or_else(|| patch.get("artifact_id"))
        })
        .and_then(Value::as_str);
    let patch_ref = patch_artifact_id
        .and_then(|id| job.artifacts.iter().find(|artifact| artifact.id == id))
        .or_else(|| {
            job.artifacts
                .iter()
                .find(|artifact| artifact_is_kind(artifact, "lab_fix_patch"))
        })
        .map(RunnerArtifactRef::from);
    let file_bundle_ref = job
        .artifacts
        .iter()
        .find(|artifact| artifact_is_kind(artifact, "mutation_file_bundle"))
        .map(RunnerArtifactRef::from);
    let operation_log_ref = job
        .artifacts
        .iter()
        .find(|artifact| artifact_is_kind(artifact, "mutation_operation_log"))
        .map(RunnerArtifactRef::from);
    let artifacts = RunnerMutationArtifacts {
        patch_ref,
        file_bundle_ref,
        operation_log_ref,
    };
    (!artifacts.is_empty()).then_some(artifacts)
}

pub(super) fn artifact_is_kind(artifact: &JobArtifactMetadata, kind: &str) -> bool {
    artifact
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("kind"))
        .and_then(Value::as_str)
        == Some(kind)
}
