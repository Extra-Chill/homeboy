//! Runner-resident Lab offload path and managed-source refresh.

use super::*;
use crate::core::runner_execution_envelope::{
    PathMaterializationEntry, PathMaterializationPlan, PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
    PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT, PATH_MATERIALIZATION_STATUS_VALIDATED,
};
use crate::core::secret_env_plan::SECRET_ENV_PLAN_ENV_DELTA_SOURCE;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_runner_resident_lab_offload(
    request: LabOffloadRequest<'_>,
    selection: LabRunnerSelection,
    contract: LabOffloadCommand,
    mut plan: HomeboyPlan,
    mut messages: Vec<String>,
    runner_workspace_root: &str,
    homeboy_path: &str,
    runner_status: &RunnerStatusReport,
    mut overhead: LabOffloadOverhead,
) -> Result<LabOffloadOutcome> {
    let runner_id = &selection.runner_id;
    let runner_homeboy = lab_runner_homeboy_metadata(runner_id, homeboy_path, runner_status);
    // Refreshing managed runner-resident source checkouts is workspace setup.
    let managed_sources_timer = overhead.phase(LabOffloadPhase::WorkspaceSync);
    let source_syncs = refresh_managed_runner_sources(runner_id, runner_workspace_root)?;
    managed_sources_timer.finish();
    if source_syncs.is_empty() {
        plan = with_step(
            plan,
            PlanStep::builder(
                "lab.managed_sources",
                "lab.managed_sources",
                PlanStepStatus::Skipped,
            )
            .skip_reason("no extension-declared managed runner sources")
            .build(),
        );
    } else {
        let syncs_json = serde_json::to_value(&source_syncs).map_err(|err| {
            Error::internal_json(
                format!("failed to serialize managed source syncs: {err}"),
                None,
            )
        })?;
        plan = with_step(
            plan,
            PlanStep::ready("lab.managed_sources", "lab.managed_sources")
                .inputs(PlanValues::new().json("sources", &syncs_json))
                .build(),
        );
        messages.push(format!(
            "Lab offload: refreshed {} managed runner source checkout(s) before dispatch.",
            source_syncs.len()
        ));
    }
    plan = with_step(
        plan,
        PlanStep::ready("lab.runner_homeboy", "lab.runner_homeboy")
            .inputs(PlanValues::new().json("runner_homeboy", &runner_homeboy))
            .build(),
    );

    let remote_output_file = request
        .output_file_requested
        .then(|| remote_lab_output_file(runner_workspace_root));
    // Structured output goes to a Homeboy-owned sibling artifact directory
    // outside the resident checkout (#6219); create it before dispatch.
    if let Some(output_file) = remote_output_file.as_deref() {
        ensure_remote_lab_artifact_dir(runner_id, output_file)?;
    }
    let remapped_args = rewrite_runner_resident_lab_offload_args(
        request.normalized_args,
        remote_output_file.as_deref(),
    );
    let run_isolation_token = agent_task_dispatch_run_isolation_token(request.normalized_args);
    let (remapped_args, agent_task_run_id) =
        ensure_agent_task_dispatch_run_id_with(&remapped_args, run_isolation_token.as_deref())
            .map_or((remapped_args, None), |(args, run_id)| (args, Some(run_id)));
    let mut command = vec![homeboy_path.to_string()];
    if remote_output_file.is_some() && !args_contain_output_file(request.normalized_args) {
        command.push("--output".to_string());
        command.push(remote_output_file.clone().expect("remote output path"));
    }
    command.extend(remapped_args.iter().skip(1).cloned());
    plan = with_step(
        plan,
        PlanStep::ready("lab.rewrite_args", "lab.rewrite_args")
            .inputs(PlanValues::new().json("argv", &redact_argv(&command)))
            .build(),
    );

    let source_path = lab_offload_source_path(request.normalized_args)?;
    let execution_context = LabExecutionContext::new(
        runner_workspace_root.to_string(),
        None,
        runner_resident_path_materialization_plan(runner_workspace_root, &source_syncs),
    );

    if !request.read_only_polling {
        eprintln!(
            "Lab offload: running runner-resident `{}` on runner `{}` in `{}`.",
            redact_argv_display(&command),
            runner_id,
            runner_workspace_root
        );
    }
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
        serde_json::to_value(&execution_context.path_materialization_plan)
            .unwrap_or(serde_json::json!(null));
    lab_metadata["job_scoped_overrides"] = job_scoped_overrides_metadata(&request.job_overrides);
    let secret_env_handoff = build_lab_secret_env_handoff_plan(
        &contract.secret_env_sources,
        &remapped_args,
        Default::default(),
    )?;
    let mut runner_workload = build_runner_workload(RunnerWorkloadBuildInput {
        plan: &plan,
        command: &contract,
        capture_patch: request.capture_patch,
        mutation_flag: request.mutation_flag,
        allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
        runner_id,
        runner_mode: status_tunnel_mode(runner_status).metadata_value(),
        assignment_source: selection.source.metadata_value(),
        status: "offloaded",
        remote_workspace: Some(&execution_context.remote_cwd),
        fallback_reason: None,
        workspace_mapping_ref: execution_context.workspace_mapping_ref(),
        proof_id: None,
    });
    runner_workload.agent_task =
        runner_workload_agent_task_from_command(&remapped_args, agent_task_run_id.as_deref());
    runner_workload.required_secrets.secret_env_plan = secret_env_handoff.secret_env_plan.clone();
    lab_metadata["secret_env_handoff"] = secret_env_handoff.diagnostics.clone();
    lab_metadata["runner_workload"] =
        serde_json::to_value(&runner_workload).unwrap_or(serde_json::json!(null));
    let base_env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    lab_metadata["env_resolution"] = lab_env_resolution_report(vec![
        LabEnvResolutionLayer {
            source: "lab_metadata_and_passthroughs",
            env: base_env,
            secret_names: Vec::new(),
        },
        LabEnvResolutionLayer {
            source: SECRET_ENV_PLAN_ENV_DELTA_SOURCE,
            env: secret_env_handoff.env_delta.clone(),
            secret_names: secret_env_handoff.secret_env_names.clone(),
        },
        LabEnvResolutionLayer {
            source: "job_override",
            env: request.job_overrides.env.clone(),
            secret_names: request.job_overrides.secret_env_names.clone(),
        },
    ]);
    let mut env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    env.extend(secret_env_handoff.env_delta.clone());
    for (name, value) in &request.job_overrides.env {
        env.insert(name.clone(), value.clone());
    }
    let mut secret_env_names = secret_env_handoff.secret_env_plan.secret_env_names();
    secret_env_names.extend(request.job_overrides.secret_env_names.clone());
    secret_env_names.sort();
    secret_env_names.dedup();
    preflight_lab_secret_env_handoff(runner_id, None, &env, &secret_env_handoff)?;
    let path_remaps =
        path_remaps_from_workspace_mapping(&[], Some(&source_path), Some(runner_workspace_root));
    preflight_lab_offload_remote_dispatch_paths(
        runner_id,
        &command,
        &env,
        &source_path,
        runner_workspace_root,
        &path_remaps,
    )?;

    let (mirror_evidence, print_handoff) =
        runner_resident_exec_noise_policy(request.read_only_polling);
    let exec_timer = overhead.phase(LabOffloadPhase::RemoteExec);
    let (exec_output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(runner_workspace_root.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env,
            secret_env_names,
            secret_env_plan: Some(secret_env_handoff.secret_env_plan.clone()),
            env_materialization: None,
            capture_patch: request.capture_patch,
            raw_exec: false,
            source_snapshot: None,
            path_materialization_plan: Some(execution_context.path_materialization_plan.clone()),
            capability_preflight: None,
            required_extensions: contract.required_extensions.clone(),
            require_paths: Vec::new(),
            runner_workload: Some(runner_workload),
            run_id: agent_task_run_id,
            detach_after_handoff: false,
            mirror_evidence,
            print_handoff,
        },
    )?;
    exec_timer.finish();
    plan = with_step(
        plan,
        PlanStep::builder(
            "lab.exec",
            "lab.exec",
            if exit_code == 0 {
                PlanStepStatus::Success
            } else {
                PlanStepStatus::Failed
            },
        )
        .inputs(PlanValues::new().json("exit_code", exit_code))
        .build(),
    );
    if !request.read_only_polling && !exec_output.stderr.is_empty() {
        messages.push(format!(
            "Lab offload: runner-resident command wrote {} stderr bytes.",
            exec_output.stderr.len()
        ));
    }

    let mut stderr = String::new();
    if !request.read_only_polling {
        for message in messages {
            stderr.push_str(&message);
            stderr.push('\n');
        }
    }
    stderr.push_str(&exec_output.stderr);
    if exit_code != 0 {
        append_runner_component_registry_repair_hint(
            &mut stderr,
            &contract,
            runner_id,
            runner_workspace_root,
            &exec_output.stdout,
            &exec_output.stderr,
        );
        append_runner_failure_context_summary(&mut stderr, &exec_output);
    }

    let output_file_content = match remote_output_file.as_deref() {
        Some(path) => Some(download_lab_output_file(runner_id, path)?),
        None => None,
    };

    Ok(LabOffloadOutcome::Offloaded {
        plan,
        stdout: exec_output.stdout,
        stderr,
        exit_code,
        output_file_content,
    })
}

fn runner_resident_exec_noise_policy(read_only_polling: bool) -> (bool, bool) {
    (!read_only_polling, !read_only_polling)
}

pub(crate) fn runner_resident_path_materialization_plan(
    runner_workspace_root: &str,
    source_syncs: &[ManagedRunnerSourceRefreshOutput],
) -> PathMaterializationPlan {
    let mut entries = vec![PathMaterializationEntry::primary_workspace_existing_remote(
        runner_workspace_root,
    )];
    entries.extend(source_syncs.iter().map(|source| {
        PathMaterializationEntry::new(
            format!("managed_source:{}", source.id),
            PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
            None,
            source.path.clone(),
            PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
            PATH_MATERIALIZATION_STATUS_VALIDATED,
        )
    }));
    PathMaterializationPlan::new(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_polling_skips_runner_exec_mirror_and_handoff_output() {
        assert_eq!(runner_resident_exec_noise_policy(true), (false, false));
    }

    #[test]
    fn runner_resident_execution_preserves_runner_exec_evidence() {
        assert_eq!(runner_resident_exec_noise_policy(false), (true, true));
    }

    #[test]
    fn runner_resident_path_materialization_plan_marks_existing_remote_sources() {
        let source_syncs = vec![ManagedRunnerSourceRefreshOutput {
            id: "runtime-agent-ci".to_string(),
            label: "Runtime Agent CI".to_string(),
            path: "/srv/homeboy/managed/runtime-agent-ci".to_string(),
            remote_url: Some("git@example.com:repo/runtime-agent-ci.git".to_string()),
            git_ref: Some("main".to_string()),
            stdout: String::new(),
            stderr: String::new(),
        }];

        let plan = runner_resident_path_materialization_plan(
            "/srv/homeboy/resident/homeboy",
            &source_syncs,
        );

        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].role, "primary_workspace");
        assert_eq!(plan.entries[0].remote_path, "/srv/homeboy/resident/homeboy");
        assert_eq!(plan.entries[0].materialization_mode, "existing_remote");
        assert_eq!(plan.entries[0].validation_status, "validated");
        assert_eq!(plan.entries[1].role, "managed_source:runtime-agent-ci");
        assert_eq!(
            plan.entries[1].remote_path,
            "/srv/homeboy/managed/runtime-agent-ci"
        );
        assert_eq!(plan.entries[1].materialization_mode, "existing_remote");
        assert_eq!(plan.entries[1].validation_status, "validated");
    }

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
        let path_remaps = path_remaps_from_workspace_mapping(
            &[],
            Some(&source),
            Some("/srv/homeboy/_lab_workspaces/homeboy"),
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

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ManagedRunnerSourceRefreshOutput {
    id: String,
    label: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_ref: Option<String>,
    stdout: String,
    stderr: String,
}

pub(crate) fn refresh_managed_runner_sources(
    runner_id: &str,
    cwd: &str,
) -> Result<Vec<ManagedRunnerSourceRefreshOutput>> {
    let plans = plan_managed_runner_source_syncs(&provider_runner_source_contracts());
    let mut refreshed = Vec::new();

    for source in plans {
        let (output, exit_code) = exec(
            runner_id,
            RunnerExecOptions {
                cwd: Some(cwd.to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                command: vec!["sh".to_string(), "-lc".to_string(), source.script.clone()],
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                env_materialization: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                path_materialization_plan: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                mirror_evidence: true,
                print_handoff: true,
            },
        )?;
        if exit_code != 0 {
            return Err(Error::validation_invalid_argument(
                "managed_runner_source",
                format!(
                    "Managed runner source `{}` could not be refreshed before Lab dispatch",
                    source.label
                ),
                Some(source.id),
                Some(vec![format!(
                    "Run `homeboy runner doctor {runner_id} --scope lab-offload --repair` for the first-class repair report before retrying."
                )]),
            ));
        }
        refreshed.push(ManagedRunnerSourceRefreshOutput {
            id: source.id,
            label: source.label,
            path: source.path,
            remote_url: source.remote_url,
            git_ref: source.git_ref,
            stdout: output.stdout,
            stderr: output.stderr,
        });
    }

    Ok(refreshed)
}
