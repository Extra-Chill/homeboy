use std::collections::HashMap;

use homeboy_core::api_jobs::RunnerJobLifecycleMetadata;
use homeboy_core::error::{Error, Result};
use homeboy_core::lab_contract::LabRunnerWorkload;
use homeboy_core::runner_execution_envelope::{
    PathMaterializationPlan, RunnerExecutionDispatch, RunnerExecutionEnvelope,
    RunnerExecutionMutationPolicy, RunnerExecutionResultRefs,
};
use homeboy_core::source_snapshot::SourceSnapshot;
use homeboy_lab_contract::lab::execution_envelope::runner_execution_envelope_from_workload;

use super::{is_internal_control_env, runner_exec_secret_env_plan};

pub(super) struct RunnerApiExecutionInput {
    pub runner_id: String,
    pub project_id: Option<String>,
    pub command: Vec<String>,
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub capture_patch: bool,
    pub source_snapshot: SourceSnapshot,
    pub path_materialization_plan: Option<PathMaterializationPlan>,
    pub require_paths: Vec<String>,
    pub extension_env_providers: Vec<String>,
    pub workload: Option<LabRunnerWorkload>,
    pub lifecycle: RunnerJobLifecycleMetadata,
    pub metadata: serde_json::Value,
}

pub(super) fn runner_api_execution_envelope(
    input: RunnerApiExecutionInput,
) -> Result<RunnerExecutionEnvelope> {
    let base_secret_env_plan = input
        .workload
        .as_ref()
        .map(|workload| workload.required_secrets.secret_env_plan.clone())
        .filter(|plan| *plan != Default::default());
    let secret_env_plan = runner_exec_secret_env_plan(
        &input.command,
        None,
        &input.secret_env_names,
        &input.env,
        base_secret_env_plan,
    );
    let inline_secret_names = secret_env_plan
        .secret_env_names()
        .into_iter()
        .filter(|name| input.env.get(name).is_some_and(|value| !value.is_empty()))
        .collect::<Vec<_>>();
    if !inline_secret_names.is_empty() {
        return Err(Error::validation_invalid_argument(
            "env",
            "durable reverse-runner jobs cannot accept inline secret env values",
            Some("durable_reverse_runner_inline_secret_env".to_string()),
            Some(vec![format!(
                "Inline secret variables: {}",
                inline_secret_names.join(", ")
            )]),
        ));
    }

    let envelope_id = input
        .workload
        .as_ref()
        .map(|workload| workload.workload_id.clone())
        .or_else(|| input.lifecycle.durable_run_id.clone())
        .or_else(|| {
            ["durable_run_id", "run_id", "record_run_id"]
                .iter()
                .find_map(|key| input.metadata.get(*key))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|run_id| !run_id.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("remote-runner:{}:runner.exec", input.runner_id));
    let mut envelope = input
        .workload
        .clone()
        .map(runner_execution_envelope_from_workload)
        .unwrap_or_else(|| {
            RunnerExecutionEnvelope::planned(&envelope_id, "remote_runner_job_request")
        });

    envelope.envelope_id = envelope_id.clone();
    envelope.source.kind = "remote_runner_job_request".to_string();
    envelope.source.ref_id = Some(envelope_id);
    envelope.secret_env = Some(secret_env_plan);
    envelope.dispatch = Some(RunnerExecutionDispatch {
        runner_id: input.runner_id,
        project_id: input.project_id,
        operation: "runner.exec".to_string(),
        command: input.command,
        cwd: Some(input.cwd),
        env: input
            .env
            .into_iter()
            .filter(|(name, _)| !is_internal_control_env(name))
            .collect(),
        source_snapshot: Some(input.source_snapshot),
        require_paths: input.require_paths,
        extension_env_providers: input.extension_env_providers,
    });
    envelope.metadata = input.metadata;
    if let Some(path_materialization_plan) = input.path_materialization_plan {
        if !envelope.metadata.is_object() {
            envelope.metadata = serde_json::json!({});
        }
        envelope.metadata["path_materialization_plan"] =
            serde_json::to_value(path_materialization_plan).unwrap_or(serde_json::Value::Null);
    }
    envelope.lifecycle = Some(input.lifecycle);
    envelope.mutation_policy = RunnerExecutionMutationPolicy {
        capture_patch: input.capture_patch,
        ..envelope.mutation_policy.clone()
    };
    if envelope.result_refs.run_id.is_none() {
        envelope.result_refs.run_id = envelope
            .lifecycle
            .as_ref()
            .and_then(|lifecycle| lifecycle.durable_run_id.clone())
            .or_else(|| {
                ["durable_run_id", "run_id", "record_run_id"]
                    .iter()
                    .find_map(|key| envelope.metadata.get(*key))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|run_id| !run_id.is_empty())
                    .map(str::to_string)
            });
    }
    if envelope.result_refs.artifacts.is_empty() {
        if let Some(workload) = input.workload.as_ref() {
            envelope.result_refs = RunnerExecutionResultRefs {
                artifacts: workload.result_refs.artifacts.clone(),
                ..envelope.result_refs
            };
        }
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::api_jobs::RemoteRunnerJobRequest;

    #[test]
    fn canonical_execution_input_preserves_the_legacy_envelope_projection() {
        let source_snapshot = homeboy_core::source_snapshot::existing_remote(
            "homeboy-lab",
            "/runner/workspace",
            Some("/runner"),
        );
        let lifecycle = RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: Some("run-17".to_string()),
            ..Default::default()
        };
        let command = vec!["homeboy".to_string(), "status".to_string()];
        let env = HashMap::from([
            ("HOMEBOY_COMMAND".to_string(), "/runner/homeboy".to_string()),
            (
                "HOMEBOY_RUNNER_PLACEMENT_RESOLVED".to_string(),
                "1".to_string(),
            ),
        ]);
        let metadata = serde_json::json!({
            "submission_key": "agent-task:v1:homeboy-lab:run-17",
            "command_assets": { "schema": "homeboy/reverse-runner-command-assets/v1", "assets": [] },
        });
        let expected = RemoteRunnerJobRequest {
            runner_id: "homeboy-lab".to_string(),
            project_id: Some("project".to_string()),
            operation: "runner.exec".to_string(),
            command: command.clone(),
            cwd: Some("/runner/workspace".to_string()),
            env: env.clone(),
            secret_env_names: vec!["RUNNER_TOKEN".to_string()],
            secret_env_plan: runner_exec_secret_env_plan(
                &command,
                None,
                &["RUNNER_TOKEN".to_string()],
                &env,
                None,
            ),
            env_materialization: None,
            capture_patch: true,
            source_snapshot: Some(source_snapshot.clone()),
            path_materialization_plan: None,
            require_paths: vec![".git".to_string()],
            extension_env_providers: vec!["provider".to_string()],
            lab_runner_workload: None,
            lifecycle: Some(lifecycle.clone()),
            workspace_claim_binding: None,
            workspace_owner_lease: None,
            metadata: Some(metadata.clone()),
        }
        .execution_envelope();

        let actual = runner_api_execution_envelope(RunnerApiExecutionInput {
            runner_id: "homeboy-lab".to_string(),
            project_id: Some("project".to_string()),
            command,
            cwd: "/runner/workspace".to_string(),
            env,
            secret_env_names: vec!["RUNNER_TOKEN".to_string()],
            capture_patch: true,
            source_snapshot,
            path_materialization_plan: None,
            require_paths: vec![".git".to_string()],
            extension_env_providers: vec!["provider".to_string()],
            workload: None,
            lifecycle,
            metadata,
        })
        .expect("canonical envelope");

        assert_eq!(actual, expected);
    }
}
