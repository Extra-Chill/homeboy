//! Runner-resident Lab offload path and managed-source refresh.

use super::*;

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

    // Refuse to dispatch caller-derived argv to the runner if any argument still
    // embeds the controller-local source path instead of the runner-resident
    // workspace. This mirrors the path-translation preflight used by the
    // patch-provider offload path before its remote dispatch (#5071).
    let source_path = lab_offload_source_path(request.normalized_args)?;
    preflight_remote_argv_path_translation(
        "Lab offload",
        runner_id,
        &command,
        &source_path,
        runner_workspace_root,
    )?;

    eprintln!(
        "Lab offload: running runner-resident `{}` on runner `{}` in `{}`.",
        redact_argv_display(&command),
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
    lab_metadata["job_scoped_overrides"] = job_scoped_overrides_metadata(&request.job_overrides);
    let secret_env_handoff = build_lab_secret_env_handoff_plan(&remapped_args, Default::default())?;
    lab_metadata["secret_env_handoff"] = secret_env_handoff.diagnostics.clone();
    let base_env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    lab_metadata["env_resolution"] = lab_env_resolution_report(vec![
        LabEnvResolutionLayer {
            source: "lab_metadata_and_passthroughs",
            env: base_env,
            secret_names: Vec::new(),
        },
        LabEnvResolutionLayer {
            source: "secret_env_plan_env_delta",
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
    env.extend(secret_env_handoff.env_delta);
    for (name, value) in &request.job_overrides.env {
        env.insert(name.clone(), value.clone());
    }
    let mut secret_env_names = secret_env_handoff.secret_env_plan.secret_env_names();
    secret_env_names.extend(request.job_overrides.secret_env_names.clone());
    secret_env_names.sort();
    secret_env_names.dedup();

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
            capture_patch: request.capture_patch,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: contract.required_extensions.clone(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: agent_task_run_id,
            detach_after_handoff: false,
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
    if !exec_output.stderr.is_empty() {
        messages.push(format!(
            "Lab offload: runner-resident command wrote {} stderr bytes.",
            exec_output.stderr.len()
        ));
    }

    let mut stderr = String::new();
    for message in messages {
        stderr.push_str(&message);
        stderr.push('\n');
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
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
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
