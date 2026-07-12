//! Core `run_lab_offload_inner` remote-exec path, @file materialization,
//! output-file download, and stream-truncation guard.

use super::*;
use crate::core::build_identity;
use crate::core::secret_env_plan::SECRET_ENV_PLAN_ENV_DELTA_SOURCE;

/// Homeboy-owned Lab artifact directory for a given runner checkout root.
///
/// Lab structured output is a Homeboy-owned artifact, not part of the synced
/// source tree. Writing it inside `checkout_root` made the runner checkout
/// dirty and the next Lab run failed the dirty-workspace preflight (#6219).
/// Derive a sibling directory (a `-homeboy-artifacts` suffix on the checkout
/// path) so the artifact lives OUTSIDE the git checkout and never dirties it.
pub(crate) fn remote_lab_artifact_dir(checkout_root: &str) -> String {
    RunnerWorkspaceOutputPaths::artifact_dir_for_workspace(checkout_root)
}

/// Remote path for the Lab structured-output JSON file.
///
/// `checkout_root` is the runner-side synced checkout (or the resident
/// workspace root). The structured output is written to a Homeboy-owned
/// sibling artifact directory rather than into the checkout itself so repeated
/// Lab runs against the same checkout never fail the dirty-workspace preflight.
pub(crate) fn remote_lab_output_file(checkout_root: &str) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{}/homeboy-lab-structured-output-{}.json",
        remote_lab_artifact_dir(checkout_root),
        nonce
    )
}

/// Remote structured-output path for runner-resident commands.
///
/// Runner-resident commands execute from the runner workspace root itself, not a
/// synced checkout under that root. Keep their output inside `workspace_root` so
/// daemon-mode file transfer can create/read it, while still keeping the file out
/// of any materialized git checkout.
pub(crate) fn remote_runner_resident_lab_output_file(workspace_root: &str) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{}/.homeboy-artifacts/homeboy-lab-structured-output-{}.json",
        workspace_root.trim_end_matches('/'),
        nonce
    )
}

/// Ensure the Homeboy-owned Lab artifact directory exists on the runner before
/// a command writes its structured output there. The directory is a sibling of
/// the checkout, so it is not created by workspace sync and must be made
/// explicitly. Failure here is non-fatal preparation: surface a clear error so
/// the missing `--output` target is diagnosable instead of a late download
/// failure.
pub(crate) fn ensure_remote_lab_artifact_dir(runner_id: &str, output_file: &str) -> Result<()> {
    let parent = output_file
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or(".");
    lab_runner_file_transfer(runner_id)?
        .ensure_directory(parent)
        .map_err(|err| lab_artifact_directory_error(runner_id, parent, err))?;
    Ok(())
}

pub(crate) fn args_contain_output_file(args: &[String]) -> bool {
    let mut passthrough = false;
    args.iter().any(|arg| {
        if passthrough {
            return false;
        }
        if arg == "--" {
            passthrough = true;
            return false;
        }
        arg == "--output" || arg.starts_with("--output=")
    })
}

pub(crate) fn materialize_lab_at_files_on_runner(
    runner_id: &str,
    specs: &[LabAtFileSpec],
) -> Result<()> {
    if specs.is_empty() {
        return Ok(());
    }

    let transfer = lab_runner_file_transfer(runner_id)?;
    for spec in specs {
        let parent = spec
            .remote_path
            .rsplit_once('/')
            .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
            .unwrap_or(".");
        transfer.ensure_directory(parent).map_err(|err| {
            lab_at_file_materialization_error(
                runner_id,
                spec,
                format!("failed to create remote parent directory: {}", err.message),
            )
        })?;
        transfer
            .upload_file(&spec.local_path.display().to_string(), &spec.remote_path)
            .map_err(|err| {
                lab_at_file_materialization_error(
                    runner_id,
                    spec,
                    format!("failed to upload file: {}", err.message),
                )
            })?;
    }

    Ok(())
}

pub(crate) fn lab_runner_file_transfer(runner_id: &str) -> Result<RunnerFileTransfer> {
    let runner = load(runner_id)?;
    let runner_status = status(runner_id).ok();
    RunnerFileTransfer::for_runner(&runner, runner_status.as_ref())
}

pub(crate) fn lab_artifact_directory_error(runner_id: &str, parent: &str, err: Error) -> Error {
    let mut error = Error::new(
        err.code,
        format!(
            "Lab offload could not create Homeboy-owned artifact directory `{parent}` on runner `{runner_id}`: {}",
            err.message
        ),
        err.details,
    );
    error.retryable = err.retryable;
    error.hints = err.hints;
    error.with_hint(
        "Lab structured output is written outside the synced checkout so it does not dirty the runner workspace; the runner workspace parent must be writable.".to_string(),
    )
}

pub(crate) fn lab_at_file_materialization_error(
    runner_id: &str,
    spec: &LabAtFileSpec,
    reason: String,
) -> Error {
    Error::validation_invalid_argument(
        "at_file",
        format!(
            "Lab offload cannot materialize @file argument `{}` on runner `{}`: {}",
            spec.original_spec, runner_id, reason
        ),
        Some(spec.local_path.display().to_string()),
        Some(vec![
            format!("Controller-side file: {}", spec.local_path.display()),
            format!("Runner-side file: {}", spec.remote_path),
            "Ensure the selected runner is reachable and its workspace root is writable, then retry Lab offload.".to_string(),
        ]),
    )
}

pub(crate) fn preflight_lab_offload_remote_dispatch_paths(
    runner_id: &str,
    command: &[String],
    env: &std::collections::HashMap<String, String>,
    source_path: &Path,
    remote_cwd: &str,
    path_remaps: &[LabPathRemap],
) -> Result<()> {
    preflight_remote_argv_path_translation(
        "Lab offload",
        runner_id,
        command,
        source_path,
        remote_cwd,
    )?;
    preflight_remote_path_bearing_surfaces(
        "Lab offload",
        runner_id,
        command,
        env,
        source_path,
        remote_cwd,
        path_remaps,
    )
}

pub(crate) struct LabProviderPreflightContext {
    pub(crate) command_prefix_argv: Vec<String>,
    pub(crate) runner_homeboy: serde_json::Value,
}

pub(crate) struct LabDispatchExecutionContext<'a> {
    pub(crate) request: &'a LabOffloadRequest<'a>,
    pub(crate) selection: &'a LabRunnerSelection,
    pub(crate) contract: &'a LabOffloadCommand,
    pub(crate) runner: Option<&'a Runner>,
    pub(crate) runner_id: &'a str,
    pub(crate) runner_status: &'a RunnerStatusReport,
    pub(crate) source_path: std::path::PathBuf,
    pub(crate) remote_cwd: String,
    pub(crate) command: Vec<String>,
    pub(crate) remote_command: Vec<String>,
    pub(crate) remapped_args: Vec<String>,
    pub(crate) accepted_extension_settings: Vec<String>,
    pub(crate) secret_preflight_args: Vec<String>,
    pub(crate) agent_task_run_id: Option<String>,
    pub(crate) runner_workload: Option<RunnerWorkload>,
    pub(crate) lab_metadata: serde_json::Value,
    pub(crate) env_resolution_layers: Vec<LabEnvResolutionLayer>,
    pub(crate) secret_env_handoff: super::super::secrets::LabSecretEnvHandoffPlan,
    pub(crate) source_snapshot: Option<SourceSnapshot>,
    pub(crate) path_materialization_plan: Option<PathMaterializationPlan>,
    pub(crate) capability_preflight: Option<RunnerCapabilityPreflight>,
    pub(crate) provider_preflight: Option<LabProviderPreflightContext>,
    pub(crate) path_remaps: Vec<LabPathRemap>,
    pub(crate) workspace_mapping_metadata: serde_json::Value,
    pub(crate) materialized_workspace: Option<MaterializedWorkspace>,
    pub(crate) dependency_cache_saves: Vec<RunnerDependencyCacheSaveRequest>,
    pub(crate) remote_output_file: Option<String>,
    pub(crate) host_telemetry: Option<LabHostTelemetryCapture>,
    pub(crate) plan: HomeboyPlan,
    pub(crate) messages: Vec<String>,
    pub(crate) overhead: LabOffloadOverhead,
    pub(crate) mirror_evidence: bool,
    pub(crate) print_handoff: bool,
    pub(crate) detach_after_handoff: bool,
}

fn lab_runner_exec_options(
    context: &LabDispatchExecutionContext<'_>,
    env: std::collections::HashMap<String, String>,
    secret_env_names: Vec<String>,
) -> RunnerExecOptions {
    RunnerExecOptions {
        cwd: Some(context.remote_cwd.clone()),
        project_id: None,
        allow_diagnostic_ssh: false,
        command: context.command.clone(),
        env,
        secret_env_names,
        secret_env_plan: Some(context.secret_env_handoff.secret_env_plan.clone()),
        env_materialization: None,
        capture_patch: context.request.capture_patch,
        raw_exec: false,
        source_snapshot: context.source_snapshot.clone(),
        path_materialization_plan: context.path_materialization_plan.clone(),
        capability_preflight: context.capability_preflight.clone(),
        required_extensions: context.contract.required_extensions.clone(),
        accepted_extension_settings: context.accepted_extension_settings.clone(),
        require_paths: Vec::new(),
        runner_workload: context.runner_workload.clone(),
        run_id: context.agent_task_run_id.clone(),
        detach_after_handoff: context.detach_after_handoff,
        mirror_evidence: context.mirror_evidence,
        print_handoff: context.print_handoff,
    }
}

pub(crate) fn exec_lab_context(
    mut context: LabDispatchExecutionContext<'_>,
) -> Result<LabOffloadOutcome> {
    let request = context.request;
    let selection = context.selection;
    let contract = context.contract;
    let runner_id = context.runner_id;
    let remote_cwd = context.remote_cwd.clone();
    let source_path = context.source_path.clone();
    let agent_task_workload = context
        .runner_workload
        .as_ref()
        .and_then(|workload| workload.agent_task.clone());
    let notification_route = context
        .runner_workload
        .as_ref()
        .and_then(|workload| workload.notification_route.clone());

    let base_env = build_lab_offload_env_with_passthroughs(&context.lab_metadata);
    context.lab_metadata["env_resolution"] = lab_env_resolution_report(
        std::iter::once(LabEnvResolutionLayer {
            source: "lab_metadata_and_passthroughs",
            env: base_env,
            secret_names: Vec::new(),
        })
        .chain(context.env_resolution_layers.clone())
        .chain(std::iter::once(LabEnvResolutionLayer {
            source: "job_override",
            env: request.job_overrides.env.clone(),
            secret_names: request.job_overrides.secret_env_names.clone(),
        }))
        .collect(),
    );
    let mut env = build_lab_offload_env_with_passthroughs(&context.lab_metadata);
    env.extend(context.secret_env_handoff.env_delta.clone());
    for (name, value) in &request.job_overrides.env {
        env.insert(name.clone(), value.clone());
    }
    let mut secret_env_names = context
        .secret_env_handoff
        .secret_env_plan
        .secret_env_names();
    secret_env_names.extend(request.job_overrides.secret_env_names.clone());
    secret_env_names.sort();
    secret_env_names.dedup();

    let pre_dispatch_started = std::time::Instant::now();
    preflight_lab_secret_env_handoff(runner_id, context.runner, &env, &context.secret_env_handoff)?;
    if let Some(runner) = context.runner {
        preflight_agent_task_runner_secret_env_plan(
            runner_id,
            runner,
            &context.secret_preflight_args,
            &env,
            &context.secret_env_handoff.secret_env_plan,
        )?;
    }
    if let (Some(provider), Some(runner), Some(source_snapshot)) = (
        context.provider_preflight.as_ref(),
        context.runner,
        context.source_snapshot.as_ref(),
    ) {
        preflight_agent_task_provider_on_runner(
            runner_id,
            &provider.command_prefix_argv,
            &context.remote_cwd,
            &context.source_path,
            &context.remapped_args,
            env.clone(),
            source_snapshot.clone(),
            contract.required_extensions.clone(),
            context.capability_preflight.clone(),
            &provider.runner_homeboy,
            context.runner_status,
        )?;
        let _ = runner;
    }
    preflight_lab_offload_remote_dispatch_paths(
        runner_id,
        &context.command,
        &env,
        &source_path,
        &remote_cwd,
        &context.path_remaps,
    )?;
    context
        .overhead
        .record(LabOffloadPhase::Preflight, pre_dispatch_started.elapsed());

    let remote_exec_started = std::time::Instant::now();
    let exec_result = exec(
        runner_id,
        lab_runner_exec_options(&context, env, secret_env_names),
    );
    context
        .overhead
        .record(LabOffloadPhase::RemoteExec, remote_exec_started.elapsed());
    let (exec_output, exit_code) = match exec_result {
        Ok(output) => output,
        Err(err) => {
            if let Some(workspace) = context.materialized_workspace.as_mut() {
                workspace.preserve();
            }
            if let Some(health) = runner_daemon_health_failure(&err) {
                let reason = health.reason.clone();
                context.plan = with_step(
                    context.plan,
                    PlanStep::builder("lab.exec", "lab.exec", PlanStepStatus::Failed)
                        .skip_reason(reason.clone())
                        .build(),
                );
                if let Some(job_id) = health.job_id.as_deref() {
                    if let Some(run_id) = context.agent_task_run_id.as_deref() {
                        return in_flight_daemon_disconnect_outcome(
                            context.plan,
                            runner_id,
                            job_id,
                            run_id,
                            &remote_cwd,
                            &context.remote_command,
                            &reason,
                            &err,
                        );
                    }
                    return Err(in_flight_daemon_disconnect_error(
                        runner_id, job_id, None, &reason, &err,
                    ));
                }
                return match selection.source {
                    LabRunnerSelectionSource::Default => {
                        if request.local_policy.deny_local_execution() {
                            Err(local_execution_denied_error(&reason, Some(runner_id)))
                        } else if !request.local_policy.allow_local_fallback() {
                            Err(selected_runner_fallback_error(
                                selection,
                                "Lab offload selected a runner but its daemon did not respond",
                                &reason,
                                vec![format!(
                                    "Reconnect runner `{runner_id}` before retrying Lab offload."
                                )],
                            ))
                        } else {
                            Ok(LabOffloadOutcome::RunLocal {
                                metadata: Some(lab_offload_metadata_with_workspace_mapping(
                                    &context.plan,
                                    selection.source.metadata_value(),
                                    Some(runner_id),
                                    Some(status_tunnel_mode(context.runner_status).metadata_value()),
                                    "fallback",
                                    Some(&remote_cwd),
                                    Some(&reason),
                                    Some(&context.workspace_mapping_metadata),
                                )),
                                plan: context.plan,
                                messages: vec![format!("Lab offload: {reason}; running locally.")],
                            })
                        }
                    }
                    LabRunnerSelectionSource::Explicit => Err(Error::validation_invalid_argument(
                        "runner",
                        format!(
                            "Lab offload runner `{runner_id}` is connected but its daemon did not respond: {}",
                            err.message
                        ),
                        Some(runner_id.to_string()),
                        Some(vec![
                            format!("Reconnect runner `{runner_id}` before retrying Lab offload."),
                            "Use --force-hot to run the command locally instead of offloading."
                                .to_string(),
                        ]),
                    )),
                };
            }
            return Err(err);
        }
    };

    context.plan = with_step(
        context.plan,
        PlanStep::builder("lab.exec", "lab.exec", PlanStepStatus::Success).build(),
    );
    let dependency_cache_save_outputs =
        save_dependency_caches(runner_id, &context.dependency_cache_saves)?;
    if !dependency_cache_save_outputs.is_empty() {
        context.plan = with_step(
            context.plan,
            PlanStep::ready("lab.save_dependency_caches", "lab.save_dependency_caches")
                .inputs(
                    PlanValues::new()
                        .json("count", dependency_cache_save_outputs.len())
                        .json("caches", &dependency_cache_save_outputs),
                )
                .build(),
        );
    }
    if exec_output.mirror_run_id.is_some() {
        context.plan = with_step(
            context.plan,
            PlanStep::builder(
                "lab.mirror_evidence",
                "lab.mirror_evidence",
                PlanStepStatus::Success,
            )
            .build(),
        );
    }
    if context.detach_after_handoff {
        if let Some(workspace) = context.materialized_workspace.as_mut() {
            workspace.preserve();
        }
        if let (Some(run_id), Some(job_id)) = (
            context.agent_task_run_id.as_deref(),
            exec_output.job_id.as_deref(),
        ) {
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id,
                    runner_job_id: job_id,
                    remote_workspace: &remote_cwd,
                    remote_command: &context.remote_command,
                },
            )?;
        }
        let mut stderr = String::new();
        for message in context.messages {
            stderr.push_str(&message);
            stderr.push('\n');
        }
        stderr.push_str(&exec_output.stderr);
        return Ok(LabOffloadOutcome::InFlight {
            plan: context.plan,
            stdout: exec_output.stdout.clone(),
            stderr,
            exit_code,
            output_file_content: Some(exec_output.stdout),
        });
    }

    let output_parse_started = std::time::Instant::now();
    let mut applied_mutation_files = Vec::new();
    if request.capture_patch && exit_code == 0 {
        let apply_output = apply_lab_offload_patch(&exec_output)?;
        let Some(apply_output) = apply_output else {
            return Err(missing_mutation_patch_error(
                request.normalized_args,
                request.mutation_flag,
                &exec_output,
            ));
        };
        applied_mutation_files = apply_output.result.modified_files.clone();
        context.plan = with_lab_apply_patch_step(context.plan, Some(apply_output));
    }
    context
        .overhead
        .record(LabOffloadPhase::OutputParse, output_parse_started.elapsed());

    let artifact_import_started = std::time::Instant::now();
    let mut output_file_content = match context.remote_output_file.as_deref() {
        Some(path) => Some(download_lab_output_file(runner_id, path)?),
        None => None,
    };
    context.overhead.record(
        LabOffloadPhase::ArtifactImport,
        artifact_import_started.elapsed(),
    );
    if let Some(host_telemetry) = context.host_telemetry {
        let host_telemetry = host_telemetry.finish();
        eprintln!(
            "Lab offload host telemetry: {}",
            host_telemetry.to_metadata()
        );
    }
    ensure_lab_offload_streams_not_truncated(
        &exec_output,
        lab_offload_structured_result_available(&exec_output, output_file_content.as_deref()),
    )?;
    let mut stdout = exec_output.stdout.clone();
    if !applied_mutation_files.is_empty() {
        stdout = reconcile_lab_mutation_output(&stdout, &applied_mutation_files);
        if let Some(content) = output_file_content.as_mut() {
            *content = reconcile_lab_mutation_output(content, &applied_mutation_files);
        }
    }

    mirror_agent_task_run_plan_lifecycle(
        request.normalized_args,
        agent_task_workload.as_ref(),
        notification_route.as_ref(),
        &stdout,
        output_file_content.as_deref(),
        exec_output.job_events.as_deref(),
    )?;

    let mut stderr = String::new();
    if !request.read_only_polling {
        for message in context.messages {
            stderr.push_str(&message);
            stderr.push('\n');
        }
    }
    stderr.push_str(&exec_output.stderr);
    if exit_code != 0 {
        stderr.push_str(&format!(
            "Lab offload FAILED REMOTELY: command exited {exit_code} on runner `{runner_id}` (remote workspace `{remote_cwd}`), NOT on this machine. If the error references a path or file, check that it exists on runner `{runner_id}`, not just locally.\n"
        ));
        append_runner_component_registry_repair_hint(
            &mut stderr,
            contract,
            runner_id,
            &remote_cwd,
            &exec_output.stdout,
            &exec_output.stderr,
        );
        append_runner_failure_context_summary(&mut stderr, &exec_output);
        if let Some(run_id) = context.agent_task_run_id.as_deref() {
            if let Some(handoff) = parse_offloaded_agent_task_handoff_from_outputs(
                &exec_output.stdout,
                &exec_output.stderr,
            )? {
                if let Some(record) = agent_task_lifecycle::record_remote_dispatch_failure(
                    agent_task_lifecycle::AgentTaskRemoteDispatchFailure {
                        identity: agent_task_lifecycle::RunDispatchIdentity { run_id, runner_id },
                        local_command: redact_argv(request.normalized_args),
                        remote_command: redact_argv(&context.remote_command),
                        remote_workspace: &remote_cwd,
                        stdout: &exec_output.stdout,
                        stderr: &exec_output.stderr,
                        exit_code,
                    },
                    &handoff.envelope,
                )? {
                    stderr.push_str(&format!(
                        "Persisted remote agent-task dispatch failure evidence for run `{}`. Inspect with `homeboy agent-task status {}` and `homeboy agent-task logs {}`.\n",
                        record.run_id, record.run_id, record.run_id
                    ));
                    return Ok(LabOffloadOutcome::Offloaded {
                        plan: context.plan,
                        stdout: exec_output.stdout,
                        stderr,
                        exit_code,
                        output_file_content,
                    });
                }
            }
            let failure_message = lab_pre_dispatch_failure_message(&exec_output.stderr)
                .or_else(|| lab_pre_dispatch_failure_message(&exec_output.stdout))
                .unwrap_or_else(|| format!("offloaded agent-task command exited with {exit_code}"));
            stderr.push_str(&format!("Lab pre-dispatch failure: {failure_message}\n"));
            let record = agent_task_lifecycle::record_pre_dispatch_failure(
                agent_task_lifecycle::AgentTaskPreDispatchFailure {
                    identity: agent_task_lifecycle::RunDispatchIdentity { run_id, runner_id },
                    local_command: redact_argv(request.normalized_args),
                    remote_command: redact_argv(&context.remote_command),
                    remote_workspace: &remote_cwd,
                    failure_message: &failure_message,
                    stdout: &exec_output.stdout,
                    stderr: &exec_output.stderr,
                    exit_code,
                },
            )?;
            stderr.push_str(&format!(
                "Persisted agent-task pre-dispatch failure evidence for run `{}`. Inspect with `homeboy agent-task status {}` and `homeboy agent-task logs {}`.\n",
                record.run_id, record.run_id, record.run_id
            ));
        }
    }

    if let Some(workspace) = context.materialized_workspace.as_mut() {
        workspace.set_success(exit_code == 0);
    }
    Ok(LabOffloadOutcome::Offloaded {
        plan: context.plan,
        stdout,
        stderr,
        exit_code,
        output_file_content,
    })
}

pub(crate) fn run_lab_offload_inner(
    request: LabOffloadRequest<'_>,
    selection: LabRunnerSelection,
    contract: LabOffloadCommand,
    mut plan: HomeboyPlan,
    mut messages: Vec<String>,
    mut overhead: LabOffloadOverhead,
) -> Result<LabOffloadOutcome> {
    let runner_id = &selection.runner_id;
    let runner = load(runner_id)?;
    let runner_status = status(runner_id)?;
    if runner.kind != super::super::super::RunnerKind::Ssh {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote direct SSH or reverse-connected runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner first."
                    .to_string(),
            ]),
        ));
    }

    if !runner_status.connected {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "Lab offload requires a connected {} runner daemon",
                status_tunnel_mode(&runner_status).label()
            ),
            Some(runner_id.to_string()),
            Some(vec![format!(
                "Connect runner `{runner_id}` before using --runner."
            )]),
        ));
    }

    let runner_workspace_root = request
        .job_overrides
        .workspace_root
        .clone()
        .or_else(|| runner.workspace_root.clone())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace_root",
                "Lab offload requires runner.workspace_root so the local checkout can be mapped remotely",
                Some(runner.id.clone()),
                Some(vec![
                    "This Wave 3 adapter assumes workspace sync/provenance has placed the same checkout basename under runner.workspace_root.".to_string(),
                ]),
            )
        })?;

    if contract.workspace_mode_policy == LabOffloadWorkspaceModePolicy::RunnerResident {
        return run_runner_resident_lab_offload(
            request,
            selection,
            contract,
            plan,
            messages,
            &runner_workspace_root,
            remote_runner_homeboy_path(&runner, "Lab offload preflight")?,
            &runner_status,
            overhead,
        );
    }

    let source_path =
        rig_materialization::lab_offload_rig_component_checkout_root(request.normalized_args)?
            .unwrap_or(lab_offload_source_path(request.normalized_args)?);
    // Begin best-effort host-level telemetry capture around the offloaded run
    // boundary (#3258). The opening snapshot of the controller host + watched
    // source/artifact dir is taken now; the closing snapshot and before/after
    // delta are produced after the run completes. Capture never fails the run.
    let host_telemetry = LabHostTelemetryCapture::start(&source_path);
    if let Some(terminal) = terminal_lab_run_evidence(request.normalized_args, &source_path) {
        plan = with_step(
            plan,
            PlanStep::builder(
                "lab.run_idempotency_guard",
                "lab.run_idempotency_guard",
                PlanStepStatus::Success,
            )
            .inputs(
                PlanValues::new()
                    .string("run_id", &terminal.run_id)
                    .string("run_dir", terminal.run_dir.to_string_lossy().to_string())
                    .string(
                        "summary_path",
                        terminal.summary_path.to_string_lossy().to_string(),
                    )
                    .string(
                        "manifest_path",
                        terminal.manifest_path.to_string_lossy().to_string(),
                    )
                    .json("passed_count", terminal.passed_count),
            )
            .build(),
        );
        let stdout = serde_json::json!({
            "schema": "homeboy/lab-run-idempotency-guard/v1",
            "command": "lab.run_idempotency_guard",
            "run_id": terminal.run_id,
            "status": terminal.status,
            "result_counts": {
                "passed": terminal.passed_count,
            },
            "published": {
                "manifest_path": terminal.manifest_path.to_string_lossy().to_string(),
            },
            "retry_policy": {
                "after_published_pass": "stop",
                "controller_action": "skipped_remote_exec",
                "active_attempt": false,
                "cancellation_command": null,
            },
        });
        let stdout = serde_json::to_string_pretty(&stdout).unwrap_or_else(|_| "{}".to_string());
        let stderr = format!(
            "Lab offload: run `{}` already published a passing result at {}; skipping duplicate remote attempt.\n",
            terminal.run_id,
            terminal.manifest_path.display()
        );
        return Ok(LabOffloadOutcome::Offloaded {
            plan,
            stdout,
            stderr,
            exit_code: 0,
            output_file_content: None,
        });
    }
    if let Some(warning) = misplaced_runner_exec_wait_timeout_warning(request.normalized_args) {
        messages.push(warning);
    }
    let source_checkout = lab_source_checkout_metadata(&source_path);
    let homeboy_path = remote_runner_homeboy_path(&runner, "Lab offload preflight")?;
    let require_exact_runner_version = require_exact_runner_version(&runner.settings);
    let runner_homeboy = lab_runner_homeboy_metadata(runner_id, homeboy_path, &runner_status);
    plan = with_step(
        plan,
        PlanStep::builder(
            "lab.runner_homeboy",
            "lab.runner_homeboy",
            if lab_runner_homeboy_has_blocking_drift(&runner_status, require_exact_runner_version) {
                PlanStepStatus::Failed
            } else {
                PlanStepStatus::Ready
            },
        )
        .inputs(
            PlanValues::new()
                .json("runner_homeboy", &runner_homeboy)
                .json("source_checkout", &source_checkout),
        )
        .build(),
    );
    eprintln!(
        "Lab offload: runner `{}` Homeboy binary `{}`; active daemon {}; refresh with `{}`.",
        runner_id,
        homeboy_path,
        runner_homeboy_daemon_display(&runner_homeboy),
        runner_homeboy["refresh_commands"]
            .as_array()
            .map(|commands| commands
                .iter()
                .filter_map(|command| command.as_str())
                .collect::<Vec<_>>()
                .join(" && "))
            .unwrap_or_else(|| format!(
                "homeboy runner disconnect {} && homeboy runner connect {}",
                shell::quote_arg(runner_id),
                shell::quote_arg(runner_id)
            ))
    );
    if lab_runner_homeboy_has_blocking_drift(&runner_status, require_exact_runner_version) {
        return Err(stale_runner_homeboy_error(
            runner_id,
            homeboy_path,
            &runner_status,
        ));
    }
    if let Some(warning) =
        lab_runner_homeboy_compatible_drift_warning(&runner_status, require_exact_runner_version)
    {
        eprintln!("{warning}");
        messages.push(warning);
    }
    let command_prefix = lab_offload_command_prefix(&source_path, homeboy_path);
    eprintln!(
        "Lab offload preflight: source checkout `{}` at {}; active Homeboy command `{}` from runner `{}`.",
        source_path.display(),
        source_checkout_ref_display(&source_checkout),
        redact_argv_display(&command_prefix.argv),
        runner_id,
    );
    let capability_contract =
        lab_runner_capability_contract(&contract, &source_path, &command_prefix.required_tools);
    let capability_plan = capability_contract
        .clone()
        .map(prepare_lab_runner_capability);
    if let Some(capability_plan) = &capability_plan {
        // Capability/daemon preflight is runner setup overhead (#3001); time it
        // and record the elapsed duration on every exit path so a fallback-to-
        // local still reports the attempted preflight cost.
        let capability_preflight_started = std::time::Instant::now();
        let decision = match evaluate_lab_runner_capabilities_for_runner(
            &runner,
            capability_plan,
            selection.source.gate_mode(),
        ) {
            Ok(decision) => decision,
            Err(err) if matches!(selection.source, LabRunnerSelectionSource::Default) => {
                overhead.record(
                    LabOffloadPhase::Preflight,
                    capability_preflight_started.elapsed(),
                );
                let reason = format!("runner capability preflight failed: {}", err.message);
                plan = with_step(
                    plan,
                    PlanStep::builder(
                        "lab.capability_preflight",
                        "lab.capability_preflight",
                        PlanStepStatus::Failed,
                    )
                    .skip_reason(reason.clone())
                    .build(),
                );
                if request.local_policy.deny_local_execution() {
                    return Err(local_execution_denied_error(&reason, Some(runner_id)));
                }
                overhead.set_fallback_reason(&reason);
                return Ok(automatic_capability_fallback(
                    plan,
                    runner_id,
                    &runner_status,
                    reason,
                    &overhead,
                ));
            }
            Err(err) => return Err(err),
        };
        overhead.record(
            LabOffloadPhase::Preflight,
            capability_preflight_started.elapsed(),
        );

        let gate_result = decision.clone();
        match decision {
            LabRunnerGateDecision::Eligible => {
                plan = with_step(
                    plan,
                    PlanStep::ready("lab.capability_preflight", "lab.capability_preflight")
                        .inputs(PlanValues::new().string("command", capability_plan.command))
                        .gate_result(gate_result)
                        .build(),
                );
            }
            LabRunnerGateDecision::Missing {
                reason,
                remediation,
                ..
            } => match selection.source {
                LabRunnerSelectionSource::Default => {
                    plan = with_step(
                        plan,
                        PlanStep::builder(
                            "lab.capability_preflight",
                            "lab.capability_preflight",
                            PlanStepStatus::Missing,
                        )
                        .skip_reason(reason.clone())
                        .gate_result(gate_result)
                        .build(),
                    );
                    overhead.set_fallback_reason(&reason);
                    return automatic_capability_fallback_or_error(
                        plan,
                        &selection,
                        &runner_status,
                        reason,
                        remediation,
                        request.local_policy.allow_local_fallback(),
                        request.local_policy.deny_local_execution(),
                        &overhead,
                    );
                }
                LabRunnerSelectionSource::Explicit => {
                    return Err(Error::validation_invalid_argument(
                        "runner_capabilities",
                        reason,
                        Some(runner_id.to_string()),
                        Some(remediation),
                    ));
                }
            },
        }
    }

    let capability_preflight: Option<RunnerCapabilityPreflight> = capability_plan.map(Into::into);
    let workspace_sync_timer = overhead.phase(LabOffloadPhase::WorkspaceSync);
    let workspace_stage = prepare_lab_offload_workspace_stage(
        &request,
        &contract,
        plan,
        runner_id,
        &source_path,
        homeboy_path,
        &command_prefix.argv,
        Some(&runner_workspace_root),
    )?;
    workspace_sync_timer.finish();
    let LabOffloadWorkspaceStage {
        plan: next_plan,
        sync_mode,
        changed_since_preflight,
        synced,
        remote_cwd,
        workspace_mapping,
        path_materialization_plan,
        source_snapshot,
        remapped_args,
        agent_task_run_id,
        runner_required_extensions,
        accepted_extension_settings,
        command,
        remote_command,
        remote_output_file,
        synced_rigs,
        rig_component_path_overrides,
        dependency_cache_saves,
        runtime_overlay_env,
        runtime_overlay_metadata,
    } = workspace_stage;
    plan = next_plan;
    let execution_context = LabExecutionContext::new(
        remote_cwd.clone(),
        Some(source_snapshot.clone()),
        path_materialization_plan,
    );

    let cleanup_policy = if agent_task_run_id.is_some() {
        WorkspaceCleanupPolicy::PreserveAlways
    } else {
        WorkspaceCleanupPolicy::PreserveOnFailure
    };
    let mut workspace_resource_lifecycle = synced.resource_lifecycle.clone();
    workspace_resource_lifecycle.cleanup_policy = if agent_task_run_id.is_some() {
        crate::core::resource_lifecycle_index::ResourceCleanupPolicy::Preserve
    } else {
        crate::core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess
    };
    let materialized_workspace = MaterializedWorkspace::new(
        runner_id.to_string(),
        remote_cwd.clone(),
        remote_output_file
            .as_deref()
            .map(|_| remote_lab_artifact_dir(&remote_cwd)),
        cleanup_policy,
    );

    if let Some(output_file) = remote_output_file.as_deref() {
        ensure_remote_lab_artifact_dir(runner_id, output_file)?;
    }

    let dependency_hydration = hydrate_for_lab_workspace_exec(
        request.skip_deps_hydration,
        runner_id,
        &synced.local_path,
        &remote_cwd,
        plan,
    )?;
    plan = dependency_hydration.plan;

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        redact_argv_display(&command),
        runner_id,
        remote_cwd
    );
    eprintln!(
        "Lab offload provenance: controller_exe=`{}` controller_build=`{}` source_args=`{}` remapped_args=`{}` required_extensions={} final_argv=`{}`.",
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|error| format!("<unavailable: {error}>")),
        build_identity::current().display,
        redact_argv_display(request.normalized_args),
        redact_argv_display(&remapped_args),
        serde_json::to_string(&runner_required_extensions).unwrap_or_else(|_| "[]".to_string()),
        redact_argv_display(&remote_command),
    );
    if let Some(run_id) = &agent_task_run_id {
        emit_durable_run_id_before_execution(
            run_id,
            runner_id,
            request.local_output_file,
            &mut messages,
        );
    }
    let workspace_mapping_metadata = lab_workspace_mapping_metadata(&workspace_mapping);
    let mut lab_metadata = lab_offload_metadata_with_workspace_mapping(
        &plan,
        selection.source.metadata_value(),
        Some(runner_id),
        Some(status_tunnel_mode(&runner_status).metadata_value()),
        "offloaded",
        Some(&remote_cwd),
        None,
        Some(&workspace_mapping_metadata),
    );
    lab_metadata["source_snapshot"] =
        serde_json::to_value(&source_snapshot).unwrap_or(serde_json::json!(null));
    lab_metadata["dependency_hydration"] =
        dependency_hydration_metadata(&dependency_hydration.record);
    lab_metadata["workspace_materialization_plan"] =
        serde_json::to_value(&execution_context.path_materialization_plan)
            .unwrap_or(serde_json::json!(null));
    lab_metadata["workspace_resource_lifecycle"] =
        serde_json::to_value(&workspace_resource_lifecycle).unwrap_or(serde_json::json!(null));
    lab_metadata["materialization_proof"] = lab_materialization_proof_metadata(
        &source_snapshot,
        &synced.snapshot_identity,
        &remote_cwd,
        &runner_homeboy,
        &source_checkout,
        &workspace_mapping_metadata,
        &synced_rigs,
    );
    lab_metadata["runtime_dependency_manifest"] = lab_runtime_dependency_manifest_metadata(
        &command_prefix.argv,
        &contract.required_extensions,
        &runner_homeboy,
        &source_checkout,
        &workspace_mapping_metadata,
        &remapped_args,
    );
    let mut env_delta = std::collections::HashMap::new();
    let rig_component_path_env =
        forward_rig_component_path_env(&mut env_delta, &workspace_mapping)?;
    let declared_dependency_paths_env =
        forward_declared_dependency_paths_env(&mut env_delta, &workspace_mapping);
    apply_rig_component_path_overrides(&mut env_delta, &rig_component_path_overrides);
    // Surface synced runtime-overlay remote paths into the command env so a hot
    // command (e.g. a CLI-runner env entry) points at the real remote runtime
    // directory rather than a controller-local path (#3831).
    let env_delta_before_runtime_overlay = env_delta.clone();
    for (name, value) in &runtime_overlay_env {
        env_delta.insert(name.clone(), value.clone());
    }
    let env_delta_before_secret_handoff = env_delta.clone();
    lab_metadata["runtime_overlays"] = runtime_overlay_metadata;
    let secret_env_handoff = build_lab_secret_env_handoff_plan(
        &contract.secret_env_sources,
        &changed_since_preflight.args,
        env_delta,
    )?;
    lab_metadata["secret_env_handoff"] = secret_env_handoff.diagnostics.clone();
    let mut runner_workload = build_runner_workload_for_dispatched_command(
        RunnerWorkloadBuildInput {
            plan: &plan,
            command: &contract,
            capture_patch: request.capture_patch,
            mutation_flag: request.mutation_flag,
            allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
            runner_id,
            runner_mode: status_tunnel_mode(&runner_status).metadata_value(),
            assignment_source: selection.source.metadata_value(),
            status: "offloaded",
            remote_workspace: Some(&remote_cwd),
            fallback_reason: None,
            workspace_mapping_ref: execution_context.workspace_mapping_ref(),
            proof_id: lab_metadata
                .get("proof")
                .and_then(|proof| proof.get("id"))
                .and_then(|id| id.as_str()),
        },
        &command,
    );
    runner_workload.agent_task =
        runner_workload_agent_task_from_command(&command, agent_task_run_id.as_deref());
    runner_workload.required_extensions = runner_required_extensions.clone();
    runner_workload.required_secrets.secret_env_plan = secret_env_handoff.secret_env_plan.clone();
    lab_metadata["runner_workload"] =
        serde_json::to_value(&runner_workload).unwrap_or(serde_json::json!(null));
    lab_metadata["rig_component_path_env"] = rig_component_path_env;
    lab_metadata["declared_dependency_paths_env"] = declared_dependency_paths_env;
    lab_metadata["rig_component_path_overrides"] =
        rig_component_path_overrides_metadata(&rig_component_path_overrides);
    lab_metadata["settings_env"] =
        settings_env_diagnostics(&remapped_args, &secret_env_handoff.env_delta);
    lab_metadata["runner_homeboy"] = runner_homeboy.clone();
    lab_metadata["source_checkout"] = source_checkout.clone();
    lab_metadata["job_scoped_overrides"] = job_scoped_overrides_metadata(&request.job_overrides);
    lab_metadata["rig_sync"] = serde_json::json!({
        "step": "lab.sync_rigs",
        "synced_count": synced_rigs.len(),
        "source_snapshot_remote_path": remote_cwd,
        "rigs": synced_rigs,
        "selected_before_remote_settings_resolution": true,
    });
    lab_metadata["workspace_cleanliness"] = serde_json::json!({
        "schema": "homeboy/lab-workspace-cleanliness/v1",
        "mode": sync_mode.label(),
        "remote_workspace": remote_cwd,
        "status": synced.workspace_cleanliness,
        "allow_dirty_lab_workspace": request.allow_dirty_lab_workspace,
    });
    attach_lab_offload_overhead(&mut lab_metadata, &overhead);
    lab_metadata["lab_host_telemetry"] = host_telemetry.before_metadata();
    let secret_env_delta = secret_env_handoff
        .env_delta
        .iter()
        .filter(|(name, value)| env_delta_before_secret_handoff.get(*name) != Some(*value))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let path_remaps = path_remaps_from_workspace_mapping(
        &workspace_mapping,
        Some(&source_path),
        Some(&remote_cwd),
    );
    exec_lab_context(LabDispatchExecutionContext {
        request: &request,
        selection: &selection,
        contract: &contract,
        runner: Some(&runner),
        runner_id,
        runner_status: &runner_status,
        source_path,
        remote_cwd,
        command,
        remote_command,
        remapped_args,
        accepted_extension_settings,
        secret_preflight_args: changed_since_preflight.args,
        agent_task_run_id,
        runner_workload: Some(runner_workload),
        lab_metadata,
        env_resolution_layers: vec![
            LabEnvResolutionLayer {
                source: "env_delta",
                env: env_delta_before_runtime_overlay,
                secret_names: Vec::new(),
            },
            LabEnvResolutionLayer {
                source: "runtime_overlay",
                env: runtime_overlay_env.iter().cloned().collect(),
                secret_names: Vec::new(),
            },
            LabEnvResolutionLayer {
                source: SECRET_ENV_PLAN_ENV_DELTA_SOURCE,
                env: secret_env_delta,
                secret_names: secret_env_handoff.secret_env_names.clone(),
            },
        ],
        secret_env_handoff,
        source_snapshot: Some(source_snapshot),
        path_materialization_plan: Some(execution_context.path_materialization_plan.clone()),
        capability_preflight,
        provider_preflight: Some(LabProviderPreflightContext {
            command_prefix_argv: command_prefix.argv,
            runner_homeboy,
        }),
        path_remaps,
        workspace_mapping_metadata,
        materialized_workspace: Some(materialized_workspace),
        dependency_cache_saves,
        remote_output_file,
        host_telemetry: Some(host_telemetry),
        plan,
        messages,
        overhead,
        mirror_evidence: true,
        print_handoff: true,
        detach_after_handoff: request.detach_after_handoff,
    })
}

fn reconcile_lab_mutation_output(output: &str, changed_files: &[String]) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(output) else {
        return output.to_string();
    };

    if !rewrite_refactor_sources_result(&mut value, changed_files) {
        return output.to_string();
    }

    serde_json::to_string_pretty(&value).unwrap_or_else(|_| output.to_string())
}

fn rewrite_refactor_sources_result(
    value: &mut serde_json::Value,
    changed_files: &[String],
) -> bool {
    let mut rewritten = false;

    if value
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| command == "refactor.sources")
    {
        rewrite_refactor_sources_object(value, changed_files);
        rewritten = true;
    }

    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                rewritten |= rewrite_refactor_sources_result(child, changed_files);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                rewritten |= rewrite_refactor_sources_result(child, changed_files);
            }
        }
        _ => {}
    }

    rewritten
}

fn rewrite_refactor_sources_object(value: &mut serde_json::Value, changed_files: &[String]) {
    let files_value = serde_json::to_value(changed_files).unwrap_or_else(|_| serde_json::json!([]));
    let file_count = serde_json::json!(changed_files.len());

    value["applied"] = serde_json::json!(true);
    value["dry_run"] = serde_json::json!(false);
    value["files_modified"] = file_count.clone();
    value["changed_files"] = files_value.clone();

    if let Some(totals) = value.get_mut("source_totals") {
        totals["stages_with_edits"] = serde_json::json!(1);
        totals["total_files_selected"] = file_count.clone();
        if totals
            .get("total_edits")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
            == 0
        {
            totals["total_edits"] = file_count.clone();
        }
    }

    if let Some(stages) = value
        .get_mut("stages")
        .and_then(serde_json::Value::as_array_mut)
    {
        for stage in stages {
            if stage
                .get("stage")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|stage| stage == "lint")
            {
                stage["applied"] = serde_json::json!(true);
                stage["files_modified"] = file_count.clone();
                stage["changed_files"] = files_value.clone();
            }
        }
    }

    if let Some(warnings) = value
        .get_mut("warnings")
        .and_then(serde_json::Value::as_array_mut)
    {
        warnings.retain(|warning| {
            warning.as_str() != Some("No automated fixes accumulated across audit/lint/test")
        });
    }
}

pub(crate) fn ensure_lab_offload_streams_not_truncated(
    exec_output: &super::super::super::RunnerExecOutput,
    structured_output_available: bool,
) -> Result<()> {
    let Some(capture) = exec_output.capture.as_ref() else {
        return Ok(());
    };
    if !capture.stdout.truncated && !capture.stderr.truncated {
        return Ok(());
    }
    if structured_output_available || has_recoverable_fuzz_result_artifact(exec_output) {
        return Ok(());
    }

    let mut error = Error::internal_unexpected(
        "Lab offload command output exceeded the retained stream limit; refusing to treat truncated stdout/stderr as the complete command result",
    )
    .with_hint("The remote command completed, but Homeboy retained only the tail of at least one output stream.".to_string())
    .with_hint("Inspect the persisted runner job instead of using the partial stdout payload.".to_string());
    error.details["runner_id"] = serde_json::json!(exec_output.runner_id);
    error.details["remote_cwd"] = serde_json::json!(exec_output.remote_cwd);
    error.details["job_id"] = serde_json::json!(exec_output.job_id);
    error.details["reason"] = serde_json::json!("output_too_large");
    error.details["capture"] =
        serde_json::to_value(capture).unwrap_or_else(|_| serde_json::json!({}));
    Err(error)
}

fn save_dependency_caches(
    runner_id: &str,
    requests: &[RunnerDependencyCacheSaveRequest],
) -> Result<Vec<RunnerDependencyCacheSaveOutput>> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let runner = load(runner_id)?;
    requests
        .iter()
        .map(|request| dependency_cache_save(&runner, request))
        .collect()
}

fn has_recoverable_fuzz_result_artifact(
    exec_output: &super::super::super::RunnerExecOutput,
) -> bool {
    exec_output
        .artifacts
        .iter()
        .any(job_artifact_is_fuzz_result)
        || exec_output.runner_result.as_ref().is_some_and(|result| {
            result
                .artifact_refs
                .iter()
                .any(runner_artifact_is_fuzz_result)
        })
}

fn job_artifact_is_fuzz_result(artifact: &crate::core::api_jobs::JobArtifactMetadata) -> bool {
    artifact
        .name
        .as_deref()
        .is_some_and(artifact_name_or_kind_is_fuzz_result)
        || artifact
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("kind"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(artifact_name_or_kind_is_fuzz_result)
}

fn runner_artifact_is_fuzz_result(artifact: &crate::core::runner::RunnerArtifactRef) -> bool {
    artifact
        .name
        .as_deref()
        .is_some_and(artifact_name_or_kind_is_fuzz_result)
}

fn artifact_name_or_kind_is_fuzz_result(value: &str) -> bool {
    matches!(
        value,
        "fuzz_results" | "fuzz_result_envelope" | "result_envelope"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_refactor_sources_output_after_lab_patch_apply() {
        let output = serde_json::json!({
            "success": true,
            "data": {
                "command": "refactor.sources",
                "dry_run": false,
                "applied": false,
                "files_modified": 0,
                "changed_files": [],
                "source_totals": {
                    "stages_with_edits": 0,
                    "total_edits": 0,
                    "total_files_selected": 0
                },
                "stages": [
                    {
                        "stage": "lint",
                        "applied": false,
                        "files_modified": 0,
                        "changed_files": []
                    }
                ],
                "warnings": [
                    "Deterministic merge order: lint",
                    "No automated fixes accumulated across audit/lint/test"
                ]
            }
        })
        .to_string();

        let rewritten = reconcile_lab_mutation_output(
            &output,
            &["inc/Demo.php".to_string(), "tests/demo.php".to_string()],
        );
        let value: serde_json::Value = serde_json::from_str(&rewritten).expect("json output");
        let data = &value["data"];

        assert_eq!(data["applied"], serde_json::json!(true));
        assert_eq!(data["files_modified"], serde_json::json!(2));
        assert_eq!(
            data["changed_files"],
            serde_json::json!(["inc/Demo.php", "tests/demo.php"])
        );
        assert_eq!(data["source_totals"]["stages_with_edits"], 1);
        assert_eq!(data["stages"][0]["applied"], serde_json::json!(true));
        assert_eq!(data["stages"][0]["files_modified"], serde_json::json!(2));
        assert!(!data["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning.as_str()
                == Some("No automated fixes accumulated across audit/lint/test")));
    }

    #[test]
    fn leaves_non_refactor_json_output_unchanged() {
        let output = "{\"success\":true,\"data\":{\"command\":\"lint\"}}";

        assert_eq!(
            reconcile_lab_mutation_output(output, &["src/lib.rs".to_string()]),
            output
        );
    }
}

pub(crate) fn lab_offload_structured_result_available(
    exec_output: &super::super::super::RunnerExecOutput,
    output_file_content: Option<&str>,
) -> bool {
    output_file_content.is_some() || exec_output.runner_result.is_some()
}

pub(crate) fn download_lab_output_file(runner_id: &str, remote_path: &str) -> Result<String> {
    let transfer = lab_runner_file_transfer(runner_id)?;
    read_downloaded_output_via_temp_file(|local_path| {
        transfer
            .download_file(remote_path, local_path)
            .map_err(|err| lab_output_download_error(runner_id, remote_path, err))
    })
}

/// Download a remote file into a unique, Drop-cleaned local temp file via
/// `download`, then read and return its contents as a string.
///
/// The temp file is a [`tempfile::NamedTempFile`]: it has a unique name (no
/// cross-run collision) and is removed when this function returns through ANY
/// path — including the `?`-error path on `download` and the `?`-error path on
/// `read_to_string`. The previous implementation built a DETERMINISTIC temp
/// path under `std::env::temp_dir()` and relied on a best-effort `remove_file`
/// placed AFTER the fallible read, so a read error (`?`) leaked the file and a
/// concurrent run at the same name could collide (#6678).
fn read_downloaded_output_via_temp_file<F>(download: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<()>,
{
    let temp = tempfile::Builder::new()
        .prefix("homeboy-lab-output-")
        .suffix(".json")
        .tempfile()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("create Lab output temp file".to_string()),
            )
        })?;
    let temp_text = temp.path().display().to_string();
    download(&temp_text)?;
    std::fs::read_to_string(temp.path()).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "read downloaded Lab output {}",
                temp.path().display()
            )),
        )
    })
    // `temp` (NamedTempFile) is dropped here, removing the unique file on every
    // return path above, including the `download(...)?` and `read_to_string`
    // error paths.
}

#[cfg(test)]
mod read_downloaded_output_tests {
    use super::*;

    #[test]
    fn returns_downloaded_contents_and_removes_temp_on_success() {
        let mut temp_path = String::new();
        let content = read_downloaded_output_via_temp_file(|local_path| {
            temp_path = local_path.to_string();
            std::fs::write(local_path, "structured-output\n")
                .map_err(|err| Error::internal_io(err.to_string(), None))
        })
        .expect("download + read succeeds");

        assert_eq!(content, "structured-output\n");
        assert!(!temp_path.is_empty());
        assert!(
            !Path::new(&temp_path).exists(),
            "temp file leaked after success: {temp_path}"
        );
    }

    #[test]
    fn removes_temp_when_download_errors() {
        let mut temp_path = String::new();
        let result = read_downloaded_output_via_temp_file(|local_path| {
            temp_path = local_path.to_string();
            Err(Error::internal_unexpected("download failed"))
        });

        assert!(result.is_err(), "download error should propagate");
        assert!(!temp_path.is_empty());
        assert!(
            !Path::new(&temp_path).exists(),
            "temp file leaked after download error: {temp_path}"
        );
    }

    #[test]
    fn removes_temp_when_read_errors() {
        // Reproduce the leak the fix targets: the download succeeds, but the
        // subsequent read fails (invalid UTF-8), so `read_to_string` `?`-returns
        // before any manual cleanup could run. The unique NamedTempFile must
        // still be removed on this error path.
        let mut temp_path = String::new();
        let result = read_downloaded_output_via_temp_file(|local_path| {
            temp_path = local_path.to_string();
            std::fs::write(local_path, [0xff, 0xfe, 0x00])
                .map_err(|err| Error::internal_io(err.to_string(), None))
        });

        assert!(result.is_err(), "invalid UTF-8 must fail the read");
        assert!(!temp_path.is_empty());
        assert!(
            !Path::new(&temp_path).exists(),
            "temp file leaked after read error: {temp_path}"
        );
    }
}

pub(crate) fn lab_output_download_error(runner_id: &str, remote_path: &str, err: Error) -> Error {
    let mut error = Error::new(
        err.code,
        format!(
            "Lab offload could not retrieve remote structured output `{remote_path}` from runner `{runner_id}`: {}",
            err.message
        ),
        err.details,
    );
    error.retryable = err.retryable;
    error.hints = err.hints;
    error.with_hint("The remote command was invoked with --output, but the runner-side file was not readable after execution.".to_string())
}
