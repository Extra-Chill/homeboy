//! Core `run_lab_offload_inner` remote-exec path, @file materialization,
//! output-file download, and stream-truncation guard.

use super::*;
use homeboy_core::build_identity;
use homeboy_core::secret_env_plan::SECRET_ENV_PLAN_ENV_DELTA_SOURCE;

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

/// Job-scoped rig registry rooted in the run-owned artifact sibling.
pub(crate) fn remote_lab_rig_registry_root(checkout_root: &str) -> String {
    format!("{}/rig-registry", remote_lab_artifact_dir(checkout_root))
}

pub(crate) fn lab_rig_registry_env(
    registry_root: Option<&str>,
) -> std::collections::HashMap<String, String> {
    registry_root.map_or_else(std::collections::HashMap::new, |root| {
        std::collections::HashMap::from([(
            homeboy_core::paths::RIG_REGISTRY_ROOT_ENV.to_string(),
            root.to_string(),
        )])
    })
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

/// Serializable result of the runner-private runtime preparation boundary.
/// It is shared by synchronous offload and durable staging so the two paths
/// publish identical rig, extension, and agent-runtime state.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LabRuntimeMaterializationOutput {
    pub(crate) remote_cwd: String,
    pub(crate) plan: HomeboyPlan,
    pub(crate) synced_rigs: Vec<rig_materialization::LabOffloadRigSync>,
    pub(crate) rig_registry_root: Option<String>,
    pub(crate) extension_overlays: serde_json::Value,
    pub(crate) extension_runtime_root: String,
    pub(crate) extension_runtime_home: Option<String>,
    pub(crate) runtime_generation:
        Option<crate::runtime_materializer::ResolvedAgentRuntimeGeneration>,
    pub(crate) runtime_env: Vec<(String, String)>,
    pub(crate) runtime_evidence: Option<crate::runtime_materializer::AgentRuntimeExecutionEvidence>,
    pub(crate) remapped_args: Vec<String>,
    pub(crate) command: Vec<String>,
    pub(crate) remote_command: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_lab_runtime<F>(
    runner: &Runner,
    homeboy_path: &str,
    remote_cwd: &str,
    changed_args: &[String],
    synced_local_path: &str,
    source_snapshot: &SourceSnapshot,
    workspace_snapshot_identity: &str,
    workspace_remaps: &[(String, String)],
    runner_required_extensions: &[String],
    agent_task_run_id: Option<&str>,
    mut plan: HomeboyPlan,
    mut remapped_args: Vec<String>,
    mut command: Vec<String>,
    mut remote_command: Vec<String>,
    mut check_cancelled: F,
) -> Result<LabRuntimeMaterializationOutput>
where
    F: FnMut() -> Result<()>,
{
    check_cancelled()?;
    ensure_remote_lab_artifact_dir(runner.id.as_str(), &remote_lab_artifact_dir(remote_cwd))?;
    check_cancelled()?;

    let candidate_rig_registry_root = remote_lab_rig_registry_root(remote_cwd);
    let synced_rigs = rig_materialization::sync_lab_offload_rigs(
        runner.id.as_str(),
        homeboy_path,
        remote_cwd,
        &candidate_rig_registry_root,
        changed_args,
        rig_materialization::LabOffloadPrimaryRigSource {
            local_path: synced_local_path,
            remote_path: remote_cwd,
            source_snapshot,
            workspace_snapshot_identity,
        },
    )?;
    check_cancelled()?;
    if !synced_rigs.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready("lab.sync_rigs", "lab.sync_rigs")
                .inputs(
                    PlanValues::new()
                        .json("count", synced_rigs.len())
                        .string("source_snapshot_remote_path", remote_cwd)
                        .string("registry_root", &candidate_rig_registry_root)
                        .json("rigs", &synced_rigs),
                )
                .build(),
        );
    }
    let rig_registry_root = (!synced_rigs.is_empty()).then_some(candidate_rig_registry_root);

    let extension_runtime_root =
        format!("{}/extension-runtime", remote_lab_artifact_dir(remote_cwd));
    let extension_overlays = crate::materialize_lab_job_extension_overlays(
        runner,
        &extension_runtime_root,
        runner_required_extensions,
    )?;
    check_cancelled()?;
    let extension_runtime_home =
        (!extension_overlays.is_empty()).then(|| format!("{extension_runtime_root}/home"));

    let mut runtime_operations = RunnerRuntimeMaterializerOperations::new(runner.clone());
    let resolved_runtime = resolve_lab_agent_runtime(
        &mut runtime_operations,
        runner,
        &remapped_args,
        workspace_remaps,
        agent_task_run_id.unwrap_or("lab-offload"),
    )?;
    check_cancelled()?;
    let runtime_generation = resolved_runtime
        .as_ref()
        .map(|resolved| resolved.generation.clone());
    let runtime_env = resolved_runtime
        .as_ref()
        .map(|resolved| resolved.env.clone())
        .unwrap_or_default();
    if let Some(resolved) = resolved_runtime {
        let prior_args = remapped_args.clone();
        remapped_args = resolved.args;
        rewrite_dispatched_runtime_args(
            &prior_args,
            &remapped_args,
            &mut command,
            &mut remote_command,
        );
        plan = with_step(
            plan,
            PlanStep::ready(
                "lab.materialize_agent_runtime",
                "lab.materialize_agent_runtime",
            )
            .inputs(PlanValues::new().json("generation", &runtime_generation))
            .build(),
        );
    }
    let runtime_evidence = runtime_generation
        .as_ref()
        .map(|generation| {
            runtime_execution_evidence(generation, &remapped_args, runner.id.as_str())
        })
        .transpose()?;

    Ok(LabRuntimeMaterializationOutput {
        remote_cwd: remote_cwd.to_string(),
        plan,
        synced_rigs,
        rig_registry_root,
        extension_overlays: serde_json::to_value(extension_overlays).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize Lab extension overlays".to_string()),
            )
        })?,
        extension_runtime_root,
        extension_runtime_home,
        runtime_generation,
        runtime_env,
        runtime_evidence,
        remapped_args,
        command,
        remote_command,
    })
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
    pub(crate) lab_runner_workload: Option<LabRunnerWorkload>,
    pub(crate) lab_metadata: serde_json::Value,
    /// Admitted during standard workspace staging and authoritative at dispatch.
    pub(crate) rig_registry_root: Option<String>,
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
    pub(crate) admission: Option<DaemonAdmissionReservation>,
    pub(crate) plan: HomeboyPlan,
    pub(crate) messages: Vec<String>,
    pub(crate) overhead: LabOffloadOverhead,
    pub(crate) mirror_evidence: bool,
    pub(crate) print_handoff: bool,
    pub(crate) detach_after_handoff: bool,
}

fn lab_runner_exec_options(
    context: &LabDispatchExecutionContext<'_>,
    mut env: std::collections::HashMap<String, String>,
    secret_env_names: Vec<String>,
) -> RunnerExecOptions {
    env.insert(
        super::super::super::RUNNER_PLACEMENT_RESOLVED_ENV.to_string(),
        "1".to_string(),
    );
    env.insert(
        homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV.to_string(),
        context.runner_id.to_string(),
    );
    if let Some(bundle) = context.lab_metadata.get("execution_bundle") {
        env.extend(crate::execution_bundle::bundle_env(bundle));
        if let Some(path) = bundle
            .pointer("/binary/path")
            .and_then(serde_json::Value::as_str)
        {
            env.insert("HOMEBOY_COMMAND".to_string(), path.to_string());
        }
        if let Some(home) = bundle
            .get("extension_runtime_home")
            .and_then(serde_json::Value::as_str)
        {
            env.insert("HOME".to_string(), home.to_string());
        }
    }
    env.extend(lab_rig_registry_env(context.rig_registry_root.as_deref()));
    RunnerExecOptions {
        cwd: Some(context.remote_cwd.clone()),
        project_id: None,
        allow_diagnostic_ssh: false,
        diagnostic_ssh_timeout: None,
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
        lab_runner_workload: context.lab_runner_workload.clone(),
        run_id: context.agent_task_run_id.clone(),
        run_id_owns_generic_exec: false,
        detach_after_handoff: context.detach_after_handoff,
        mirror_evidence: context.mirror_evidence,
        print_handoff: context.print_handoff,
        read_only_artifact_access: false,
    }
}

pub(crate) fn exec_lab_context(
    mut context: LabDispatchExecutionContext<'_>,
) -> Result<LabOffloadOutcome> {
    // Keep the daemon-visible admission active until this function returns.
    let _admission = context.admission.take();
    let request = context.request;
    let selection = context.selection;
    let contract = context.contract;
    let runner_id = context.runner_id;
    let remote_cwd = context.remote_cwd.clone();
    let source_path = context.source_path.clone();
    let agent_task_workload = context
        .lab_runner_workload
        .as_ref()
        .and_then(|workload| workload.agent_task.clone());
    let notification_route = context
        .lab_runner_workload
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
        .chain(
            context
                .rig_registry_root
                .as_deref()
                .map(|root| LabEnvResolutionLayer {
                    source: "admitted_rig_registry",
                    env: lab_rig_registry_env(Some(root)),
                    secret_names: Vec::new(),
                }),
        )
        .collect(),
    );
    let mut env = build_lab_offload_env_with_passthroughs(&context.lab_metadata);
    env.extend(context.secret_env_handoff.env_delta.clone());
    for (name, value) in &request.job_overrides.env {
        env.insert(name.clone(), value.clone());
    }
    // Reassert immutable authority after untrusted per-job overrides.
    if let Some(bundle) = context.lab_metadata.get("execution_bundle") {
        env.extend(crate::execution_bundle::bundle_env(bundle));
        if let Some(path) = bundle
            .pointer("/binary/path")
            .and_then(serde_json::Value::as_str)
        {
            env.insert("HOMEBOY_COMMAND".to_string(), path.to_string());
        }
        if let Some(home) = bundle
            .get("extension_runtime_home")
            .and_then(serde_json::Value::as_str)
        {
            env.insert("HOME".to_string(), home.to_string());
        }
    }
    let mut secret_env_names = context
        .secret_env_handoff
        .secret_env_plan
        .secret_env_names();
    secret_env_names.extend(request.job_overrides.secret_env_names.clone());
    secret_env_names.sort();
    secret_env_names.dedup();

    let pre_dispatch_started = std::time::Instant::now();
    let source_checkout = context
        .source_snapshot
        .as_ref()
        .and_then(|snapshot| serde_json::to_value(snapshot).ok());
    if let Some(run_id) = context.agent_task_run_id.as_deref() {
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            runner_id,
            "executor_preflight",
            Some(&remote_cwd),
            source_checkout.as_ref(),
            None,
            request.durable_agent_task_plan,
        )?;
        if context.detach_after_handoff {
            agent_task_lifecycle::record_lab_offload_submission_intent(
                run_id,
                runner_id,
                &remote_cwd,
                &context.remote_command,
                &secret_env_names,
            )?;
        }
    }
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
        if let Some(run_id) = context.agent_task_run_id.as_deref() {
            agent_task_lifecycle::record_lab_offload_phase(
                run_id,
                runner_id,
                "provider_preflight",
                Some(&remote_cwd),
                source_checkout.as_ref(),
                None,
                request.durable_agent_task_plan,
            )?;
        }
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

    // Persist the controller parent before the daemon accepts a child. This is
    // intentionally before `exec`: a controller timeout must leave status and
    // logs resolvable without reading the runner's private lifecycle store.
    if let Some(run_id) = context.agent_task_run_id.as_deref() {
        agent_task_lifecycle::record_lab_offload_planned(
            agent_task_lifecycle::LabOffloadProxyPlan {
                run_id,
                runner_id,
                remote_workspace: &remote_cwd,
                remote_command: &context.remote_command,
                durable_plan: request.durable_agent_task_plan,
            },
        )?;
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            runner_id,
            "provider_dispatch",
            Some(&remote_cwd),
            source_checkout.as_ref(),
            None,
            request.durable_agent_task_plan,
        )?;
    }

    let remote_exec_started = std::time::Instant::now();
    let exec_result = exec_with_status_snapshot(
        runner_id,
        lab_runner_exec_options(&context, env, secret_env_names),
        Some(context.runner_status.clone()),
    );
    context
        .overhead
        .record(LabOffloadPhase::RemoteExec, remote_exec_started.elapsed());
    let (exec_output, exit_code) = match exec_result {
        Ok(output) => output,
        Err(err) => {
            if let Some(run_id) = context.agent_task_run_id.as_deref() {
                if let Some(job_id) = accepted_runner_job_id(runner_id, run_id) {
                    if let Some(workspace) = context.materialized_workspace.as_mut() {
                        workspace.preserve();
                    }
                    return in_flight_daemon_disconnect_outcome(
                        context.plan,
                        runner_id,
                        &job_id,
                        run_id,
                        &remote_cwd,
                        &context.remote_command,
                        "daemon accepted the durable job before the exec response was lost",
                        &err,
                    );
                }
            }
            let health = runner_daemon_health_failure(&err);
            let in_flight_job_id = health
                .as_ref()
                .and_then(|health| health.job_id.as_deref())
                .or_else(|| controller_wait_expired_job_id(&err))
                .map(str::to_string);
            if in_flight_job_id.is_none() {
                if let Some(run_id) = context.agent_task_run_id.as_deref() {
                    // The controller parent already exists, so a failed handoff
                    // must terminalize it rather than leave a queued ghost.
                    let plan = agent_task_lifecycle::load_plan(run_id)?;
                    agent_task_lifecycle::record_pre_execution_failure(
                        run_id,
                        &plan,
                        "lab_handoff",
                        &err,
                    )?;
                }
            }
            if let Some(health) = health {
                let reason = health.reason.clone();
                context.plan = with_step(
                    context.plan,
                    PlanStep::builder("lab.exec", "lab.exec", PlanStepStatus::Failed)
                        .skip_reason(reason.clone())
                        .build(),
                );
                if let Some(job_id) = in_flight_job_id.as_deref() {
                    if let Some(run_id) = context.agent_task_run_id.as_deref() {
                        if let Some(workspace) = context.materialized_workspace.as_mut() {
                            workspace.preserve();
                        }
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
                        if request.placement == homeboy_cli_contract::Placement::Lab {
                            Err(local_execution_denied_error(&reason, Some(runner_id)))
                        } else if !request.allow_local_fallback {
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
                            "Use --placement local to run the command locally instead of offloading."
                                .to_string(),
                        ]),
                    )),
                };
            }
            if let Some(job_id) = in_flight_job_id.as_deref() {
                if let Some(run_id) = context.agent_task_run_id.as_deref() {
                    if let Some(workspace) = context.materialized_workspace.as_mut() {
                        workspace.preserve();
                    }
                    return in_flight_daemon_disconnect_outcome(
                        context.plan,
                        runner_id,
                        job_id,
                        run_id,
                        &remote_cwd,
                        &context.remote_command,
                        "controller wait expired; awaiting authoritative daemon result",
                        &err,
                    );
                }
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
    let promotion_intent = promotion_handoff_intent(request.normalized_args, &exec_output.stdout)?;
    if request.capture_patch && (exit_code == 0 || promotion_intent.is_some()) {
        let apply_output = match promotion_intent.as_ref() {
            Some(intent) => apply_lab_promotion_patch(&exec_output, intent)?,
            None => apply_lab_offload_patch(&exec_output)?,
        };
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
        // Reap on a clean success only. A provider can exit 0 while its
        // controller-side candidate projection or handoff still owes follow-up;
        // reaping then would destroy a preserved candidate's workspace (#9377).
        // Treat any recorded candidate-preserving follow-up as not-yet-success
        // so post-mortem/recovery bytes survive on the lab.
        let owes_candidate_follow_up = context
            .agent_task_run_id
            .as_deref()
            .map(|run_id| {
                agent_task_lifecycle::run_owes_candidate_follow_up(run_id).unwrap_or(false)
            })
            .unwrap_or(false);
        workspace.set_success(exit_code == 0 && !owes_candidate_follow_up);
    }
    Ok(LabOffloadOutcome::Offloaded {
        plan: context.plan,
        stdout,
        stderr,
        exit_code,
        output_file_content,
    })
}

/// `/exec` is not an idempotent operation. When its response is lost, query the
/// daemon's durable active-job projection by the preassigned run ID rather than
/// submitting the workload again.
fn accepted_runner_job_id(runner_id: &str, run_id: &str) -> Option<String> {
    accepted_runner_job_id_with(runner_id, run_id, || crate::status(runner_id))
}

pub(crate) fn accepted_runner_job_id_with<F>(
    runner_id: &str,
    run_id: &str,
    status: F,
) -> Option<String>
where
    F: FnOnce() -> Result<crate::RunnerStatusReport>,
{
    let status = status().ok()?;
    if status.runner_id != runner_id {
        return None;
    }
    let mut matching = status
        .active_runner_jobs
        .into_iter()
        .filter(|job| job.durable_run_id.as_deref() == Some(run_id))
        .map(|job| job.job_id)
        .collect::<Vec<_>>();
    matching.sort();
    (matching.len() == 1).then(|| matching.remove(0))
}

fn promotion_handoff_intent(args: &[String], stdout: &str) -> Result<Option<PromotionPatchIntent>> {
    if !args
        .windows(2)
        .any(|args| args == ["agent-task", "promote"])
    {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("portable promotion result".to_string()),
            Some(stdout.to_string()),
        )
    })?;
    let report = value.get("data").unwrap_or(&value);
    let status = report.get("status").and_then(|value| value.as_str());
    if !matches!(status, Some("applied" | "gate_failed")) {
        return Ok(None);
    }
    let changed_files = report
        .get("changed_files")
        .and_then(|files| files.as_array())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion_handoff.changed_files",
                "portable promotion result did not declare changed files for safe controller handback",
                None,
                None,
            )
        })?
        .iter()
        .map(|file| {
            file.as_str().map(str::to_string).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion_handoff.changed_files",
                    "portable promotion result contains a non-string changed file",
                    None,
                    None,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(PromotionPatchIntent { changed_files }))
}

pub(crate) fn run_lab_offload_inner(
    request: LabOffloadRequest<'_>,
    selection: LabRunnerSelection,
    contract: LabOffloadCommand,
    mut plan: HomeboyPlan,
    mut messages: Vec<String>,
    mut overhead: LabOffloadOverhead,
    _runner_status: RunnerStatusReport,
) -> Result<LabOffloadOutcome> {
    let runner_id = &selection.runner_id;
    let runner = load(runner_id)?;
    let mut runner_status = status_for_admission(runner_id)?;
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

    require_available_lab_runner(
        runner_id,
        &runner_status,
        runner.settings.concurrency_limit,
        contract.hot_label,
    )?;

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

    let source_path = request
        .source_path
        .map(std::path::Path::to_path_buf)
        .or(rig_materialization::lab_offload_rig_component_checkout_root(request.normalized_args)?)
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
    let run_isolation_token = agent_task_dispatch_run_isolation_token(request.normalized_args);
    // Cook's public run id identifies the retry series. Persist pre-acceptance
    // Lab progress under its first attempt id so daemon acceptance advances the
    // same canonical lifecycle record instead of leaving a queued proxy behind.
    let pre_acceptance_lifecycle = ensure_agent_task_lifecycle_identity_with(
        request.normalized_args,
        run_isolation_token.as_deref(),
        None,
    );
    let pre_acceptance_run_id = pre_acceptance_lifecycle
        .as_ref()
        .map(|(_, run_id)| run_id.clone());
    let provider_rotation =
        serde_json::to_value(homeboy_core::defaults::load_config().agent_task.rotation)
            .unwrap_or(serde_json::Value::Null);
    if let Some(run_id) = pre_acceptance_run_id.as_deref() {
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            runner_id,
            "validation",
            None,
            Some(&source_checkout),
            Some(&provider_rotation),
            request.durable_agent_task_plan,
        )?;
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            runner_id,
            "materializing",
            None,
            Some(&source_checkout),
            Some(&provider_rotation),
            request.durable_agent_task_plan,
        )?;
    }
    let homeboy_path = remote_runner_homeboy_path(&runner, "Lab offload preflight")?;
    let require_exact_runner_version = require_exact_runner_version(&runner.settings);
    let configured_build_identity =
        configured_runner_homeboy_build_identity(&runner, homeboy_path)?;
    if runner_status
        .session
        .as_ref()
        .is_some_and(|session| session.mode == RunnerTunnelMode::DirectSsh)
        && runner_status.stale_daemon.is_none()
    {
        let session = runner_status
            .session
            .as_ref()
            .expect("checked direct SSH session");
        let active_identity = session.homeboy_build_identity.clone();
        if configured_build_identity.as_deref() != active_identity.as_deref() {
            runner_status.stale_daemon = Some(
                RunnerStaleDaemonWarning::new(
                    runner_id,
                    session.homeboy_version.clone(),
                    session.homeboy_version.clone(),
                    active_identity,
                    configured_build_identity.clone(),
                )
                .with_identity_unverifiable(
                    runner_id,
                    homeboy_path,
                    configured_build_identity.is_none(),
                ),
            );
        }
    }
    let runner_homeboy = lab_runner_homeboy_metadata(runner_id, homeboy_path, &runner_status);
    plan = with_step(
        plan,
        PlanStep::builder(
            "lab.runner_homeboy",
            "lab.runner_homeboy",
            if lab_runner_homeboy_has_blocking_drift_against_configured_identity(
                &runner_status,
                configured_build_identity.as_deref(),
                require_exact_runner_version,
            ) {
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
    if lab_runner_homeboy_has_blocking_drift_against_configured_identity(
        &runner_status,
        configured_build_identity.as_deref(),
        require_exact_runner_version,
    ) {
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
        redact_argv_shell_display(&command_prefix.argv),
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
                if request.placement == homeboy_cli_contract::Placement::Lab {
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
                        .gate_result(crate::capabilities::gate_result_from_lab_decision(
                            gate_result,
                        ))
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
                        .gate_result(crate::capabilities::gate_result_from_lab_decision(
                            gate_result,
                        ))
                        .build(),
                    );
                    overhead.set_fallback_reason(&reason);
                    return automatic_capability_fallback_or_error(
                        plan,
                        &selection,
                        &runner_status,
                        reason,
                        remediation,
                        request.allow_local_fallback,
                        request.placement,
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
    // Detached agent-task work is controller-owned from this point.
    // It must never fall through to caller-side workspace staging after admission.
    if request.detach_after_handoff
        && request.durable_agent_task_plan.is_some()
        && capability_preflight.is_some()
    {
        let run_id = pre_acceptance_run_id.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
            "run_id",
            "detached Lab offload requires a durable agent-task attempt before controller staging",
            None,
            None,
        )
        })?;
        if request.local_output_file.is_some() {
            emit_durable_run_id_before_execution(
                run_id,
                runner_id,
                request.local_output_file,
                &mut messages,
            );
        }
        let controller_job_id = match crate::lab_staging_controller::submit_detached_staging(
            run_id,
            runner_id,
            selection.mode.clone(),
            &request,
        ) {
            Ok(controller_job_id) => controller_job_id,
            Err(error) => {
                // A retryable transport failure can follow durable acceptance.
                // Keep the parent nonterminal so replay can reconcile the same
                // controller idempotency key instead of contradicting live work.
                if error.retryable != Some(true) {
                    if let Some(durable_plan) = request.durable_agent_task_plan.as_ref() {
                        let _ = agent_task_lifecycle::record_pre_execution_failure(
                            run_id,
                            durable_plan,
                            "lab_staging_submission",
                            &error,
                        );
                    }
                }
                return Err(
                    error.with_hint(format!("Retry: homeboy agent-task retry {run_id} --run"))
                );
            }
        };
        let controller_job_commands = controller_job_retrieval_commands(&controller_job_id);
        let stdout = serde_json::to_string_pretty(&serde_json::json!({
            "success": true,
            "data": {
                "status": "materializing",
                "durable_run_id": run_id,
                "controller_job_id": controller_job_id,
                "retrieval_commands": {
                    "status": format!("homeboy agent-task status {run_id}"),
                    "controller_job": controller_job_commands.show,
                    "controller_job_watch": controller_job_commands.watch,
                },
                "next_actions": [
                    format!("Show controller job: {}", controller_job_commands.show),
                    format!("Watch controller job: {}", controller_job_commands.watch),
                ],
            }
        }))
        .unwrap_or_else(|_| {
            format!("Lab staging controller job {controller_job_id} accepted for {run_id}")
        });
        return Ok(LabOffloadOutcome::InFlight {
            plan,
            stdout: format!("{stdout}\n"),
            stderr: format!(
                "Lab staging continues in controller job `{controller_job_id}`.\nNext: homeboy agent-task status {run_id}\nNext: {}\nNext: {}\n",
                controller_job_commands.show, controller_job_commands.watch,
            ),
            exit_code: 0,
            output_file_content: Some(format!("{stdout}\n")),
        });
    }
    let workspace_sync_timer = overhead.phase(LabOffloadPhase::WorkspaceSync);
    let workspace_stage = match prepare_lab_offload_workspace_stage(
        &request,
        crate::lab::offload::workspace_stage::LabWorkspaceStageCommand::from(&contract),
        plan,
        runner_id,
        &source_path,
        &command_prefix.argv,
        Some(&runner_workspace_root),
        run_isolation_token,
        pre_acceptance_lifecycle
            .as_ref()
            .map(|(args, _)| args.as_slice())
            .unwrap_or(request.normalized_args),
        pre_acceptance_run_id.as_deref(),
    ) {
        Ok(stage) => stage,
        Err(error) => {
            let error = if let Some(run_id) = pre_acceptance_run_id.as_deref() {
                error.with_hint(format!("Retry: homeboy agent-task retry {run_id} --run"))
            } else {
                error
            };
            if let Some(run_id) = pre_acceptance_run_id.as_deref() {
                if let Ok(plan) = agent_task_lifecycle::load_plan(run_id) {
                    // Staging is still controller-side. Make a failed handoff
                    // actionable instead of retaining an unclaimed proxy.
                    let _ = agent_task_lifecycle::record_pre_execution_failure(
                        run_id,
                        &plan,
                        "lab_workspace_stage",
                        &error,
                    );
                }
            }
            return Err(error);
        }
    };
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
        mut remapped_args,
        agent_task_run_id,
        runner_required_extensions,
        accepted_extension_settings,
        mut command,
        mut remote_command,
        remote_output_file,
        rig_component_path_overrides,
        dependency_cache_saves,
        runtime_overlay_env,
        runtime_overlay_metadata,
    } = workspace_stage;
    plan = next_plan;
    if agent_task_run_id != pre_acceptance_run_id {
        return Err(Error::internal_unexpected(
            "Lab workspace staging changed the pre-acceptance agent-task lifecycle identity",
        ));
    }
    // This workspace owns the admitted rig and extension snapshots. Every
    // terminal result, including failure and cancellation, must release them.
    // Detached and uncertain in-flight work explicitly relinquishes ownership.
    let cleanup_policy = WorkspaceCleanupPolicy::DeleteAlways;
    let mut workspace_resource_lifecycle = synced.resource_lifecycle.clone();
    workspace_resource_lifecycle.cleanup_policy =
        homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnTerminal;
    let mut materialized_workspace = MaterializedWorkspace::new(
        runner_id.to_string(),
        remote_cwd.clone(),
        Some(remote_lab_artifact_dir(&remote_cwd)),
        cleanup_policy,
    );

    let workspace_remaps = workspace_mapping
        .iter()
        .map(|entry| {
            (
                entry.local_path().to_string(),
                entry.remote_path().to_string(),
            )
        })
        .collect::<Vec<_>>();
    let runtime = materialize_lab_runtime(
        &runner,
        homeboy_path,
        &remote_cwd,
        &changed_since_preflight.args,
        &synced.local_path,
        &source_snapshot,
        &synced.snapshot_identity,
        &workspace_remaps,
        &runner_required_extensions,
        agent_task_run_id.as_deref(),
        plan,
        remapped_args,
        command,
        remote_command,
        || Ok(()),
    )?;
    plan = runtime.plan;
    let synced_rigs = runtime.synced_rigs;
    let rig_registry_root = runtime.rig_registry_root;
    let extension_overlays = runtime.extension_overlays;
    let extension_runtime_home = runtime.extension_runtime_home;
    let runtime_generation = runtime.runtime_generation;
    let runtime_env = runtime.runtime_env;
    let runtime_evidence = runtime.runtime_evidence;
    remapped_args = runtime.remapped_args;
    command = runtime.command;
    remote_command = runtime.remote_command;
    if let Some(run_id) = agent_task_run_id.as_deref() {
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            runner_id,
            "hydrating",
            Some(&remote_cwd),
            Some(&source_checkout),
            Some(&provider_rotation),
            request.durable_agent_task_plan,
        )?;
    }

    let recorded_dependency_hydration = hydrate_for_lab_workspace_exec_with_lifecycle(
        request.skip_deps_hydration,
        runner_id,
        &synced.local_path,
        &remote_cwd,
        plan,
        agent_task_run_id.as_deref(),
    )?;
    let dependency_hydration = recorded_dependency_hydration.hydration;
    plan = dependency_hydration.plan;
    if let Some(run_id) = agent_task_run_id.as_deref() {
        agent_task_lifecycle::record_lab_offload_phase(
            run_id,
            runner_id,
            "dispatching",
            Some(&remote_cwd),
            Some(&source_checkout),
            Some(&provider_rotation),
            request.durable_agent_task_plan,
        )?;
    }

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        redact_argv_shell_display(&command),
        runner_id,
        remote_cwd
    );
    eprintln!(
        "Lab offload provenance: controller_exe=`{}` controller_build=`{}` source_args=`{}` remapped_args=`{}` required_extensions={} final_argv=`{}`.",
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|error| format!("<unavailable: {error}>")),
        build_identity::current().display,
        redact_argv_shell_display(request.normalized_args),
        redact_argv_shell_display(&remapped_args),
        serde_json::to_string(&runner_required_extensions).unwrap_or_else(|_| "[]".to_string()),
        redact_argv_shell_display(&remote_command),
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
    attach_lab_workspace_metadata(
        &mut lab_metadata,
        LabWorkspaceMetadataInputs {
            source_snapshot: &source_snapshot,
            legacy_path_materialization_plan: &path_materialization_plan,
            primary_synced_workspace: &synced,
        },
    )?;
    if let Some(verified_cook_baseline) = request.verified_cook_baseline {
        lab_metadata["source_provenance"] = serde_json::json!({
            "verified_cook_baseline": verified_cook_baseline,
        });
    }
    lab_metadata["dependency_hydration"] =
        dependency_hydration_metadata(&dependency_hydration.record);
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
    env_delta.extend(lab_rig_registry_env(rig_registry_root.as_deref()));
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
    // These are job-scoped provider inputs. They never update the runner's
    // global environment or a previously admitted generation.
    for (name, value) in &runtime_env {
        env_delta.insert(name.clone(), value.clone());
    }
    let env_delta_before_secret_handoff = env_delta.clone();
    lab_metadata["runtime_overlays"] = runtime_overlay_metadata;
    lab_metadata["resolved_agent_runtime_generation"] =
        serde_json::to_value(&runtime_generation).unwrap_or(serde_json::json!(null));
    lab_metadata["runtime_evidence"] =
        serde_json::to_value(&runtime_evidence).unwrap_or(serde_json::json!(null));
    let secret_env_handoff = build_lab_secret_env_handoff_plan(
        &contract.secret_env_sources,
        &changed_since_preflight.args,
        env_delta,
    )?;
    lab_metadata["secret_env_handoff"] = secret_env_handoff.diagnostics.clone();
    let mut lab_runner_workload = build_lab_runner_workload_for_dispatched_command(
        LabRunnerWorkloadBuildInput {
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
            workspace_mapping_ref: path_materialization_plan.mapping_ref(),
            proof_id: lab_metadata
                .get("proof")
                .and_then(|proof| proof.get("id"))
                .and_then(|id| id.as_str()),
        },
        &command,
    );
    lab_runner_workload.agent_task =
        lab_runner_workload_agent_task_from_command(&command, agent_task_run_id.as_deref());
    lab_runner_workload.required_extensions = runner_required_extensions.clone();
    lab_runner_workload.required_secrets.secret_env_plan =
        secret_env_handoff.secret_env_plan.clone();
    lab_metadata["runner_workload"] =
        serde_json::to_value(&lab_runner_workload).unwrap_or(serde_json::json!(null));
    // Preserve the evidence with the serialized workload envelope as well as
    // Lab metadata, so direct and reverse runner evidence retain the exact
    // dispatch identity without widening the stable workload contract.
    lab_metadata["runner_workload"]["runtime_evidence"] =
        serde_json::to_value(&runtime_evidence).unwrap_or(serde_json::json!(null));
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
        "registry_root": rig_registry_root,
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
    let path_remaps = path_remaps_from_materialization_plan(
        &path_materialization_plan,
        Some((&source_path, &remote_cwd)),
    );
    // Reserve only after every local/pre-dispatch preparation, including runtime
    // publication, succeeded. The reservation's Drop implementation releases
    // the lease if the subsequent provider preflight rejects the handoff.
    //
    // A *retryable* pre-acceptance admission failure (a stale lease or a
    // transient `/admissions` transport error) means the workload never
    // started, yet all the expensive preparation — rig install/sync, workspace
    // and package snapshots/uploads, path translation — is already staged on the
    // remote workspace. Reaping it here (the DeleteAlways default) forces a
    // retry to repeat the entire prep sequence with new identities (#9469).
    // Preserve the prepared workspace so a retry can resume against the current
    // healthy daemon instead; a genuine terminal outcome still reaps as before.
    let admission = match direct_daemon_admission_coordinates(
        runner_id,
        &selection.mode,
        runner_status.session.as_ref(),
    )
    .and_then(|coordinates| {
        coordinates
            .map(|(local_url, expected_daemon_lease_id)| {
                reserve_daemon_admission_with_recovery(
                    runner_id,
                    local_url,
                    &redact_argv_shell_display(&command_prefix.argv),
                    expected_daemon_lease_id,
                    agent_task_run_id.as_deref(),
                    DaemonAdmissionPolicy::LegacyCompatible,
                )
            })
            .transpose()
    }) {
        Ok(admission) => admission,
        Err(error) => {
            if error.retryable == Some(true) {
                materialized_workspace.preserve();
                if let Some(run_id) = agent_task_run_id.as_deref() {
                    eprintln!(
                        "Lab offload: retryable admission failure for run `{run_id}` on runner \
                         `{runner_id}`; preserving prepared workspace `{remote_cwd}` for resume \
                         (rig install/sync and snapshots are reusable). Retry to resume from \
                         admission."
                    );
                }
            }
            return Err(error);
        }
    };
    lab_metadata["execution_bundle"] = serde_json::json!({
        "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
        "binary": {
            "path": homeboy_path,
            "build_identity": runner_homeboy.get("job_command_binary_build_identity")
                .or_else(|| runner_homeboy.get("active_daemon_build_identity")),
        },
        "admission": {
            "daemon_lease_id": admission
                .as_ref()
                .map(|reservation| reservation.authority().daemon_lease_id().to_string()),
            "reservation_job_id": admission.as_ref().map(|reservation| reservation.job_id()),
            "authority": admission.is_none().then_some("reverse_broker"),
        },
        "rig_registry_root": rig_registry_root,
        "rigs": synced_rigs,
        "extensions": extension_overlays,
        "extension_runtime_home": extension_runtime_home,
        "workspace_mapping": workspace_mapping_metadata,
        "identities": {
            "requested": {
                "runner_id": runner_id,
                "extensions": contract.required_extensions,
                "command": request.normalized_args,
            },
            "resolved": {
                "binary": homeboy_path,
                "rig_registry_root": rig_registry_root,
                "rigs": &synced_rigs,
                "extensions": &extension_overlays,
                "workspace_mapping": &workspace_mapping_metadata,
            },
            "executed": {
                "argv": &remote_command,
                "daemon_lease_id": admission
                    .as_ref()
                    .map(|reservation| reservation.authority().daemon_lease_id().to_string()),
                "reservation_job_id": admission.as_ref().map(|reservation| reservation.job_id()),
                "authority": admission.is_none().then_some("reverse_broker"),
            },
        },
    });
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
        lab_runner_workload: Some(lab_runner_workload),
        lab_metadata,
        rig_registry_root,
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
        path_materialization_plan: Some(path_materialization_plan),
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
        admission,
        plan,
        messages,
        overhead,
        mirror_evidence: true,
        print_handoff: true,
        detach_after_handoff: request.detach_after_handoff,
    })
}

pub(crate) struct ControllerJobRetrievalCommands {
    pub(crate) show: String,
    pub(crate) watch: String,
}

pub(crate) fn controller_job_retrieval_commands(job_id: &str) -> ControllerJobRetrievalCommands {
    ControllerJobRetrievalCommands {
        show: format!("homeboy activity show {job_id}"),
        watch: format!("homeboy activity watch {job_id}"),
    }
}

/// Keep the generated direct command and the reverse-transport command in
/// lockstep when resolving a controller policy into a runner generation.
fn rewrite_dispatched_runtime_args(
    prior_args: &[String],
    resolved_args: &[String],
    command: &mut [String],
    remote_command: &mut [String],
) {
    for (prior, resolved) in prior_args.iter().zip(resolved_args) {
        if prior == resolved {
            continue;
        }
        for arg in command.iter_mut().chain(remote_command.iter_mut()) {
            if *arg == *prior {
                *arg = resolved.clone();
            }
        }
    }
}

fn direct_daemon_admission_coordinates<'a>(
    runner_id: &str,
    mode: &super::super::super::RunnerTunnelMode,
    session: Option<&'a super::super::super::RunnerSession>,
) -> Result<Option<(&'a str, &'a str)>> {
    if *mode == super::super::super::RunnerTunnelMode::Reverse {
        return Ok(None);
    }
    let session = session.ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            format!("runner `{runner_id}` has no direct session for Lab admission"),
            Some(runner_id.to_string()),
            None,
        )
    })?;
    if session.mode != super::super::super::RunnerTunnelMode::DirectSsh {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!("runner `{runner_id}` direct Lab admission found a non-direct session"),
            Some(runner_id.to_string()),
            None,
        ));
    }
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            format!("runner `{runner_id}` has no direct daemon endpoint for Lab admission"),
            Some(runner_id.to_string()),
            None,
        )
    })?;
    let lease_id = session.remote_daemon_lease_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            format!("runner `{runner_id}` has no proven daemon lease for Lab admission"),
            Some(runner_id.to_string()),
            None,
        )
    })?;
    Ok(Some((local_url, lease_id)))
}

/// Gate Lab dispatch on the same authoritative availability verdict the
/// selection preflight uses, rather than a bare `connected` boolean.
///
/// A raw `connected` check is lossy in two directions and was the shared root
/// cause of several Lab reliability reports:
///
/// - A runner can be `connected: true` while `accepts_jobs: false` because its
///   daemon is version-stale (`stale_daemon`). The old boolean gate *passed*
///   such a runner, so dispatch proceeded and then failed opaquely downstream
///   (#8811), while the exact `runner refresh-homeboy ... --reconnect` recovery
///   command that status already computed was discarded.
/// - A disconnected runner was rejected with a generic "requires a connected
///   daemon" message that omitted the structured reason and recovery evidence.
///
/// Routing through `RunnerAvailability` + `lab_runner_availability_error` keeps
/// this deeper gate consistent with the preflight gate and preserves the
/// stale-daemon reason and its exact recovery command in the returned error.
fn require_available_lab_runner(
    runner_id: &str,
    runner_status: &RunnerStatusReport,
    concurrency_limit: Option<usize>,
    command_label: &str,
) -> Result<()> {
    let availability = RunnerAvailability::from_status_parts(
        runner_id.to_string(),
        runner_status.connected,
        runner_status.stale_daemon.is_some(),
        runner_status.active_jobs.len(),
        &runner_status.active_job_state,
        concurrency_limit,
    );
    if availability.accepts_jobs {
        return Ok(());
    }

    Err(lab_runner_availability_error(
        command_label,
        Some(&availability),
        Some(runner_status),
        vec![availability.clone()],
    ))
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

fn job_artifact_is_fuzz_result(artifact: &homeboy_core::api_jobs::JobArtifactMetadata) -> bool {
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

fn runner_artifact_is_fuzz_result(artifact: &crate::RunnerArtifactRef) -> bool {
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
    use crate::{
        RunnerActiveJobState, RunnerSessionState, RunnerStaleDaemonWarning, RunnerTunnelMode,
    };
    use std::sync::{Arc, Barrier, Mutex};

    #[test]
    fn concurrent_same_id_rigs_dispatch_from_their_admitted_registry_after_promotion() {
        let root = tempfile::tempdir().expect("runner root");
        let prepared = Arc::new(Barrier::new(3));
        let dispatch = Arc::new(Barrier::new(3));
        let runner_default = Arc::new(Mutex::new("/runner/homeboy-a".to_string()));
        let jobs = [
            ("a", "catalog-a", "/runner/homeboy-a", "lease-a"),
            ("b", "catalog-b", "/runner/homeboy-b", "lease-b"),
        ];
        let handles = jobs
            .into_iter()
            .map(|(job, catalog, binary, lease)| {
                let prepared = Arc::clone(&prepared);
                let dispatch = Arc::clone(&dispatch);
                let runner_default = Arc::clone(&runner_default);
                let registry_root = root.path().join(format!("{job}-homeboy-artifacts/rig-registry"));
                std::thread::spawn(move || {
                    // Fake runner install operation: it receives the same env
                    // shape as the real rig install and writes only there.
                    let install_env = lab_rig_registry_env(Some(&registry_root.display().to_string()));
                    let installed_root = install_env
                        .get(homeboy_core::paths::RIG_REGISTRY_ROOT_ENV)
                        .expect("admitted registry root")
                        .to_string();
                    let catalog_path = std::path::Path::new(&installed_root)
                        .join("rigs/same-id/catalog");
                    std::fs::create_dir_all(catalog_path.parent().expect("rig parent"))
                        .expect("private rig registry");
                    std::fs::write(&catalog_path, format!("{catalog}\n")).expect("rig catalog");

                    prepared.wait();
                    // Promotion/reconnect changes the administrative default
                    // between admission and dispatch. The bound command and
                    // lease must remain authoritative for this job.
                    dispatch.wait();
                    assert_eq!(
                        runner_default.lock().expect("default lock").as_str(),
                        "/runner/homeboy-promoted"
                    );
                    let output = std::process::Command::new("sh")
                        .arg("-c")
                        .arg("cat \"$HOMEBOY_RIG_REGISTRY_ROOT/rigs/same-id/catalog\"")
                        .envs(install_env)
                        .output()
                        .expect("final rig dispatch");
                    assert!(output.status.success());
                    assert_eq!(
                        String::from_utf8(output.stdout).expect("catalog utf8"),
                        format!("{catalog}\n")
                    );
                    let bundle = serde_json::json!({
                        "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
                        "binary": { "path": binary, "build_identity": format!("build-{job}") },
                        "admission": { "daemon_lease_id": lease, "reservation_job_id": format!("job-{job}") },
                        "rig_registry_root": installed_root,
                        "rigs": [{ "rig_id": "same-id", "workload_hashes": { "source_snapshot_hash": catalog } }],
                        "extensions": []
                    });
                    let env = crate::execution_bundle::bundle_env(&bundle);
                    assert!(crate::execution_bundle::validate_bundle_env(
                        &env,
                        &[binary.to_string(), "fuzz".to_string()],
                        &[]
                    ));
                })
            })
            .collect::<Vec<_>>();

        prepared.wait();
        *runner_default.lock().expect("default lock") = "/runner/homeboy-promoted".to_string();
        dispatch.wait();
        for handle in handles {
            handle.join().expect("rig job");
        }
    }

    #[test]
    fn resolved_runtime_policy_rewrites_direct_and_reverse_transport_commands() {
        let controller_policy =
            "--resolved-provider-policy={\"runtime_identity\":{\"runtime_path\":\"/controller/runtime\"}}"
                .to_string();
        let runner_policy =
            "--resolved-provider-policy={\"runtime_identity\":{\"runtime_path\":\"/runner/generation/runtime\"}}"
                .to_string();
        let prior = vec!["agent-task".to_string(), controller_policy.clone()];
        let resolved = vec!["agent-task".to_string(), runner_policy.clone()];
        let mut direct = vec!["homeboy".to_string(), controller_policy.clone()];
        let mut reverse = vec!["homeboy".to_string(), controller_policy];

        rewrite_dispatched_runtime_args(&prior, &resolved, &mut direct, &mut reverse);

        assert_eq!(direct[1], runner_policy);
        assert_eq!(reverse[1], runner_policy);
    }

    #[test]
    fn final_dispatch_reasserts_the_admitted_rig_registry_root() {
        let root = "/runner/app-homeboy-artifacts/rig-registry".to_string();
        let args = Vec::new();
        let request = LabOffloadRequest {
            command: None,
            normalized_args: &args,
            explicit_runner: None,
            placement: homeboy_cli_contract::Placement::Auto,
            allow_local_fallback: true,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            capture_patch: false,
            mutation_flag: None,
            detach_after_handoff: false,
            output_file_requested: false,
            read_only_polling: false,
            local_output_file: None,
            durable_agent_task_plan: None,
            source_path: None,
            verified_cook_baseline: None,
            require_controller_git_bundle: false,
            reuse_compatible_snapshot: false,
            job_overrides: LabJobOverrides {
                env: std::collections::HashMap::from([
                    (
                        homeboy_core::paths::RIG_REGISTRY_ROOT_ENV.to_string(),
                        "/caller/conflict".to_string(),
                    ),
                    (
                        "HOMEBOY_COMMAND".to_string(),
                        "/caller/new-homeboy".to_string(),
                    ),
                    ("HOME".to_string(), "/caller/home".to_string()),
                ]),
                ..Default::default()
            },
        };
        let selection = LabRunnerSelection {
            runner_id: "homeboy-lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::Reverse,
        };
        let contract = LabOffloadCommand {
            command: homeboy_core::lab_contract::LabCommandContract::portable(
                "test",
                None,
                true,
                &[],
            ),
            required_extensions: vec!["fixture".to_string()],
            required_capabilities: Vec::new(),
            workload: None,
        };
        let secret_env_handoff = build_lab_secret_env_handoff_plan(&[], &args, Default::default())
            .expect("empty secret handoff");
        let runner_status = runner_status(true);
        let context = LabDispatchExecutionContext {
            request: &request,
            selection: &selection,
            contract: &contract,
            runner: None,
            runner_id: "homeboy-lab",
            runner_status: &runner_status,
            source_path: std::path::PathBuf::from("/controller/app"),
            remote_cwd: "/runner/app".to_string(),
            command: vec!["/runner/admitted-homeboy".to_string(), "bench".to_string()],
            remote_command: Vec::new(),
            remapped_args: Vec::new(),
            accepted_extension_settings: Vec::new(),
            secret_preflight_args: Vec::new(),
            agent_task_run_id: None,
            lab_runner_workload: None,
            lab_metadata: serde_json::json!({
                "execution_bundle": {
                    "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
                    "binary": { "path": "/runner/admitted-homeboy" },
                    "admission": { "daemon_lease_id": "lease-before-promotion", "reservation_job_id": "job-a" },
                    "rig_registry_root": root,
                    "rigs": [{ "rig_id": "same-id", "workload_hashes": { "source_snapshot_hash": "catalog-a" } }],
                    "extensions": [{ "id": "fixture", "remote_path": "/runner/job/extensions/fixture/aaaaaaaaaaaaaaaa", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }],
                    "extension_runtime_home": "/runner/job/home",
                }
            }),
            rig_registry_root: Some(root.clone()),
            env_resolution_layers: Vec::new(),
            secret_env_handoff,
            source_snapshot: None,
            path_materialization_plan: None,
            capability_preflight: None,
            provider_preflight: None,
            path_remaps: Vec::new(),
            workspace_mapping_metadata: serde_json::json!({}),
            materialized_workspace: None,
            dependency_cache_saves: Vec::new(),
            remote_output_file: None,
            host_telemetry: None,
            admission: None,
            plan: base_lab_plan(None),
            messages: Vec::new(),
            overhead: LabOffloadOverhead::start(),
            mirror_evidence: false,
            print_handoff: false,
            detach_after_handoff: false,
        };
        let options =
            lab_runner_exec_options(&context, request.job_overrides.env.clone(), Vec::new());

        assert_eq!(
            options.env.get(homeboy_core::paths::RIG_REGISTRY_ROOT_ENV),
            Some(&root)
        );
        assert_eq!(
            options.env.get("HOMEBOY_COMMAND"),
            Some(&"/runner/admitted-homeboy".to_string())
        );
        assert_eq!(
            options.env.get("HOME"),
            Some(&"/runner/job/home".to_string())
        );
        assert!(crate::execution_bundle::validate_bundle_env(
            &options.env,
            &options.command,
            &contract.required_extensions,
        ));
    }

    #[test]
    fn inner_uses_authoritative_preflight_status_not_conflicting_cwd_projection() {
        let preflight_status = runner_status(true);
        let conflicting_cwd_projection = runner_status(false);

        require_available_lab_runner("homeboy-lab", &preflight_status, None, "cook")
            .expect("inner accepts the connected status selected during preflight");
        assert!(require_available_lab_runner(
            "homeboy-lab",
            &conflicting_cwd_projection,
            None,
            "cook"
        )
        .is_err());
    }

    #[test]
    fn inner_rejects_connected_but_stale_daemon_with_recovery_command() {
        // #8811: a runner can be `connected: true` while its daemon is
        // version-stale. The old bare-boolean gate passed such a runner and
        // dispatch then failed opaquely downstream. The unified availability
        // gate must reject it up front with the structured `stale_daemon`
        // reason and the exact `refresh-homeboy` recovery command that status
        // already computed, instead of the generic "not connected" message.
        let mut status = runner_status(true);
        status.stale_daemon = Some(RunnerStaleDaemonWarning {
            severity: "warning",
            session_homeboy_version: "0.289.1".to_string(),
            current_homeboy_version: "0.289.3".to_string(),
            session_homeboy_build_identity: None,
            current_homeboy_build_identity: None,
            active_daemon_control_plane_version: "0.289.1".to_string(),
            job_command_binary_version: "0.289.3".to_string(),
            active_daemon_control_plane_build_identity: None,
            job_command_binary_build_identity: None,
            refresh_command:
                "homeboy runner refresh-homeboy homeboy-lab --ref v0.289.3 --reconnect".to_string(),
            stale_runtime_paths: Vec::new(),
            changed_runtime_paths: Vec::new(),
            message: "daemon is stale".to_string(),
            recovery_commands: Vec::new(),
        });

        let error = require_available_lab_runner("homeboy-lab", &status, None, "cook")
            .expect_err("connected-but-stale runner must not pass the Lab dispatch gate");

        // The rejection is the structured availability diagnosis, not the
        // generic "requires a connected daemon" boolean error.
        assert!(
            !error.message.contains("requires a connected"),
            "stale-daemon rejection must not use the generic connectivity message: {}",
            error.message
        );
        let details = serde_json::to_string(&error.details).expect("serialize error details");
        assert!(
            details.contains("stale_daemon"),
            "error must preserve the stale_daemon reason: {details}"
        );
        assert!(
            details.contains("refresh-homeboy"),
            "error must surface the exact refresh-homeboy recovery command: {details}"
        );
    }

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

    #[test]
    fn direct_lab_admission_requires_and_preserves_daemon_coordinates() {
        let session = admission_session(
            RunnerTunnelMode::DirectSsh,
            Some("http://127.0.0.1:7421"),
            Some("lease-a"),
        );
        assert_eq!(
            direct_daemon_admission_coordinates(
                "homeboy-lab",
                &RunnerTunnelMode::DirectSsh,
                Some(&session),
            )
            .expect("direct admission coordinates"),
            Some(("http://127.0.0.1:7421", "lease-a")),
        );

        let missing_endpoint =
            admission_session(RunnerTunnelMode::DirectSsh, None, Some("lease-a"));
        let error = direct_daemon_admission_coordinates(
            "homeboy-lab",
            &RunnerTunnelMode::DirectSsh,
            Some(&missing_endpoint),
        )
        .expect_err("direct admission requires an endpoint");
        assert!(error.message.contains("direct daemon endpoint"));
    }

    #[test]
    fn reverse_lab_admission_keeps_broker_path_without_direct_reservation() {
        assert_eq!(
            direct_daemon_admission_coordinates("homeboy-lab", &RunnerTunnelMode::Reverse, None)
                .expect("reverse sessions use broker admission"),
            None,
        );
    }

    fn admission_session(
        mode: RunnerTunnelMode,
        local_url: Option<&str>,
        lease_id: Option<&str>,
    ) -> crate::RunnerSession {
        crate::RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode,
            role: crate::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: local_url.map(str::to_string),
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: lease_id.map(str::to_string),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    fn runner_status(connected: bool) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected,
            state: if connected {
                RunnerSessionState::Connected
            } else {
                RunnerSessionState::Disconnected
            },
            session: None,
            stale_daemon: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_jobs: Vec::new(),
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "/tmp/homeboy-lab.json".to_string(),
        }
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
