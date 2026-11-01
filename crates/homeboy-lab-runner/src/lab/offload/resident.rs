//! Runner-resident Lab offload path.

use super::*;
use homeboy_core::runner_execution_envelope::PathMaterializationPlan;
use homeboy_core::secret_env_plan::SECRET_ENV_PLAN_ENV_DELTA_SOURCE;
use homeboy_lab_contract::path_materialization::primary_workspace_existing_remote;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_runner_resident_lab_offload(
    request: LabOffloadRequest<'_>,
    selection: LabRunnerSelection,
    contract: LabOffloadCommand,
    mut plan: HomeboyPlan,
    messages: Vec<String>,
    runner_workspace_root: &str,
    homeboy_path: &str,
    runner_status: &RunnerStatusReport,
    observation: OffloadObservation,
) -> Result<LabOffloadOutcome> {
    let runner_id = &selection.runner_id;
    let runner_homeboy = lab_runner_homeboy_metadata(runner_id, homeboy_path, runner_status);
    plan = with_step(
        plan,
        PlanStep::ready("lab.runner_homeboy", "lab.runner_homeboy")
            .inputs(PlanValues::new().json("runner_homeboy", &runner_homeboy))
            .build(),
    );

    let remote_output_file = request
        .output_file_requested
        .then(|| remote_runner_resident_lab_output_file(runner_workspace_root));
    if let Some(output_file) = remote_output_file.as_deref() {
        ensure_remote_lab_artifact_dir(runner_id, output_file)?;
    }
    let remapped_args = rewrite_runner_resident_lab_offload_args(
        request.normalized_args,
        remote_output_file.as_deref(),
    );
    let remapped_args = inject_agent_task_resolved_provider_policy_in_args(&remapped_args)?;
    let run_isolation_token = agent_task_dispatch_run_isolation_token(request.normalized_args);
    let (remapped_args, agent_task_run_id) = ensure_agent_task_lifecycle_identity_with(
        &remapped_args,
        run_isolation_token.as_deref(),
        None,
    )
    .map_or((remapped_args, None), |(args, run_id)| (args, Some(run_id)));
    let mut command =
        lab_offload_command_prefix(std::path::Path::new(runner_workspace_root), homeboy_path).argv;
    if remote_output_file.is_some() && !args_contain_output_file(request.normalized_args) {
        command.push("--output".to_string());
        command.push(remote_output_file.clone().expect("remote output path"));
    }
    command.extend(remapped_args.iter().skip(1).cloned());
    plan = with_step(
        plan,
        PlanStep::ready("lab.rewrite_args", "lab.rewrite_args")
            .inputs(PlanValues::new().json("argv", redact_argv(&command)))
            .build(),
    );

    let source_path = lab_offload_source_path(request.normalized_args)?;
    let path_materialization_plan =
        runner_resident_path_materialization_plan(runner_workspace_root);

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        redact_argv_shell_display(&command),
        runner_id,
        runner_workspace_root
    );
    let mut lab_metadata = lab_offload_metadata(
        &plan,
        selection.source.metadata_value(),
        Some(runner_id),
        Some(status_tunnel_mode(runner_status).metadata_value()),
        "offloaded",
        Some(runner_workspace_root),
        None,
    );
    lab_metadata["runner_homeboy"] = runner_homeboy;
    lab_metadata["workspace"] = serde_json::json!({
        "schema": "homeboy/lab-runner-resident-workspace/v1",
        "mode": "runner_resident",
        "runner_cwd": runner_workspace_root,
        "command_paths": "runner_side",
    });
    lab_metadata["workspace_materialization_plan"] =
        serde_json::to_value(&path_materialization_plan).unwrap_or(serde_json::json!(null));
    lab_metadata["job_scoped_overrides"] = job_scoped_overrides_metadata(&request.job_overrides);
    let secret_env_handoff = build_lab_secret_env_handoff_plan(
        contract.secret_env_sources,
        &remapped_args,
        Default::default(),
    )?;
    let mut lab_runner_workload = build_lab_runner_workload_for_dispatched_command(
        LabRunnerWorkloadBuildInput {
            plan: &plan,
            command: &contract,
            capture_patch: request.capture_patch,
            mutation_flag: request.mutation_flag,
            allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
            runner_id,
            runner_mode: status_tunnel_mode(runner_status).metadata_value(),
            assignment_source: selection.source.metadata_value(),
            status: "offloaded",
            remote_workspace: Some(runner_workspace_root),
            fallback_reason: None,
            workspace_mapping_ref: path_materialization_plan.mapping_ref(),
            proof_id: None,
        },
        &command,
    );
    lab_runner_workload.agent_task = lab_runner_workload_agent_task_from_command(
        &command,
        agent_task_run_id.as_deref(),
        request.durable_agent_task_plan,
    );
    lab_runner_workload.required_secrets.secret_env_plan =
        secret_env_handoff.secret_env_plan.clone();
    lab_metadata["secret_env_handoff"] = secret_env_handoff.diagnostics.clone();
    let path_remaps = path_remaps_from_materialization_plan(
        &path_materialization_plan,
        Some((&source_path, runner_workspace_root)),
    );
    lab_metadata["runner_workload"] =
        serde_json::to_value(&lab_runner_workload).unwrap_or(serde_json::json!(null));

    let (mirror_evidence, print_handoff) =
        runner_resident_exec_noise_policy(request.read_only_polling);
    exec_lab_context(LabDispatchExecutionContext {
        request: &request,
        selection: &selection,
        contract: &contract,
        runner: None,
        runner_id,
        runner_status,
        source_path,
        remote_cwd: runner_workspace_root.to_string(),
        command: command.clone(),
        remote_command: command,
        accepted_extension_settings: Vec::new(),
        secret_preflight_args: remapped_args,
        agent_task_run_id,
        lab_runner_workload: Some(lab_runner_workload),
        lab_metadata,
        rig_registry_root: None,
        env_resolution_layers: vec![LabEnvResolutionLayer {
            source: SECRET_ENV_PLAN_ENV_DELTA_SOURCE,
            env: secret_env_handoff.env_delta.clone(),
            secret_names: secret_env_handoff.secret_env_names.clone(),
        }],
        secret_env_handoff,
        source_snapshot: None,
        path_materialization_plan: Some(path_materialization_plan),
        capability_preflight: None,
        path_remaps,
        workspace_mapping_metadata: serde_json::json!({
            "schema": "homeboy/lab-runner-resident-workspace/v1",
            "mode": "runner_resident",
            "runner_cwd": runner_workspace_root,
        }),
        materialized_workspace: None,
        remote_output_file,
        host_telemetry: None,
        admission: None,
        plan,
        messages,
        observation,
        mirror_evidence,
        print_handoff,
        detach_after_handoff: false,
    })
}

fn runner_resident_exec_noise_policy(read_only_polling: bool) -> (bool, bool) {
    (!read_only_polling, !read_only_polling)
}

pub(crate) fn runner_resident_path_materialization_plan(
    runner_workspace_root: &str,
) -> PathMaterializationPlan {
    PathMaterializationPlan::new(vec![primary_workspace_existing_remote(
        runner_workspace_root,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_resident_preflight_rejects_controller_local_path_env() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("homeboy");
        let fixture_root = controller.path().join("fixtures");
        std::fs::create_dir_all(&source).expect("source");
        std::fs::create_dir_all(&fixture_root).expect("fixture root");
        let command = vec!["homeboy".to_string(), "test".to_string()];
        let env = std::collections::HashMap::from([(
            "FIXTURE_ROOT".to_string(),
            fixture_root.display().to_string(),
        )]);
        let plan =
            runner_resident_path_materialization_plan("/srv/homeboy/_lab_workspaces/homeboy");
        let path_remaps = path_remaps_from_materialization_plan(
            &plan,
            Some((&source, "/srv/homeboy/_lab_workspaces/homeboy")),
        );

        let err = preflight_lab_offload_remote_dispatch_paths(
            "lab-resident",
            &command,
            &env,
            &source,
            "/srv/homeboy/_lab_workspaces/homeboy",
            &path_remaps,
        )
        .expect_err("resident env with controller-local path must fail pre-dispatch");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("path-bearing remote surface"));
        assert!(err.message.contains("lab-resident"));
        assert!(err
            .details
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id.contains("env `FIXTURE_ROOT`")
                && id.contains(&fixture_root.display().to_string())));
    }
}
