//! Core `run_lab_offload_inner` remote-exec path, @file materialization,
//! output-file download, and stream-truncation guard.

use super::*;

/// Homeboy-owned Lab artifact directory for a given runner checkout root.
///
/// Lab structured output is a Homeboy-owned artifact, not part of the synced
/// source tree. Writing it inside `checkout_root` made the runner checkout
/// dirty and the next Lab run failed the dirty-workspace preflight (#6219).
/// Derive a sibling directory (a `-homeboy-artifacts` suffix on the checkout
/// path) so the artifact lives OUTSIDE the git checkout and never dirties it.
pub(crate) fn remote_lab_artifact_dir(checkout_root: &str) -> String {
    format!("{}-homeboy-artifacts", checkout_root.trim_end_matches('/'))
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
    let client = ssh_client_for_lab_runner(runner_id)?;
    let mkdir = client.execute(&format!("mkdir -p {}", shell::quote_arg(parent)));
    if !mkdir.success {
        return Err(Error::internal_unexpected(format!(
            "Lab offload could not create Homeboy-owned artifact directory `{parent}` on runner `{runner_id}`: {}",
            mkdir.stderr.trim()
        ))
        .with_hint(
            "Lab structured output is written outside the synced checkout so it does not dirty the runner workspace; the runner workspace parent must be writable.".to_string(),
        ));
    }
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

    let client = ssh_client_for_lab_runner(runner_id)?;
    for spec in specs {
        let parent = spec
            .remote_path
            .rsplit_once('/')
            .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
            .unwrap_or(".");
        let mkdir = client.execute(&format!("mkdir -p {}", shell::quote_arg(parent)));
        if !mkdir.success {
            return Err(lab_at_file_materialization_error(
                runner_id,
                spec,
                format!("failed to create remote parent directory: {}", mkdir.stderr),
            ));
        }
        let upload = client.upload_file(&spec.local_path.display().to_string(), &spec.remote_path);
        if !upload.success {
            return Err(lab_at_file_materialization_error(
                runner_id,
                spec,
                format!("failed to upload file: {}", upload.stderr),
            ));
        }
    }

    Ok(())
}

pub(crate) fn ssh_client_for_lab_runner(runner_id: &str) -> Result<SshClient> {
    let runner = load(runner_id)?;
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            "Lab offload @file materialization requires an SSH runner with server_id",
            Some(runner_id.to_string()),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner before Lab offload.".to_string(),
            ]),
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env);
    Ok(client)
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
            runner.settings.homeboy_path.as_deref().unwrap_or("homeboy"),
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
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let runner_homeboy = lab_runner_homeboy_metadata(runner_id, homeboy_path, &runner_status);
    plan = with_step(
        plan,
        PlanStep::builder(
            "lab.runner_homeboy",
            "lab.runner_homeboy",
            if runner_status.stale_daemon.is_some() {
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
    if runner_status.stale_daemon.is_some() {
        return Err(stale_runner_homeboy_error(
            runner_id,
            homeboy_path,
            &runner_status,
        ));
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
        source_snapshot,
        remapped_args,
        agent_task_run_id,
        command,
        remote_command,
        remote_output_file,
        synced_rigs,
        rig_component_path_overrides,
        runtime_overlay_env,
        runtime_overlay_metadata,
    } = workspace_stage;
    plan = next_plan;

    // The structured-output file lives in a Homeboy-owned artifact directory
    // that is a sibling of the synced checkout (#6219), so it is never created
    // by workspace sync. Create it explicitly before dispatch so the remote
    // command can write its `--output` target outside the git tree.
    if let Some(output_file) = remote_output_file.as_deref() {
        ensure_remote_lab_artifact_dir(runner_id, output_file)?;
    }

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        redact_argv_display(&command),
        runner_id,
        remote_cwd
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
    let runner_workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        workspace_mapping_ref: Some("workspace_mapping"),
        proof_id: lab_metadata
            .get("proof")
            .and_then(|proof| proof.get("id"))
            .and_then(|id| id.as_str()),
    });
    lab_metadata["runner_workload"] =
        serde_json::to_value(&runner_workload).unwrap_or(serde_json::json!(null));
    lab_metadata["source_snapshot"] =
        serde_json::to_value(&source_snapshot).unwrap_or(serde_json::json!(null));
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
    let wordpress_dependency_paths_env =
        forward_wordpress_dependency_paths_env(&mut env_delta, &workspace_mapping);
    apply_rig_component_path_overrides(&mut env_delta, &rig_component_path_overrides);
    // Surface synced runtime-overlay remote paths into the command env so a hot
    // command (e.g. a CLI-runner env entry) points at the real remote runtime
    // directory rather than a controller-local path (#3831).
    for (name, value) in &runtime_overlay_env {
        env_delta.insert(name.clone(), value.clone());
    }
    lab_metadata["runtime_overlays"] = runtime_overlay_metadata;
    let secret_env_handoff =
        build_lab_secret_env_handoff_plan(&changed_since_preflight.args, env_delta)?;
    lab_metadata["secret_env_handoff"] = secret_env_handoff.diagnostics.clone();
    lab_metadata["rig_component_path_env"] = rig_component_path_env;
    lab_metadata["wordpress_dependency_paths_env"] = wordpress_dependency_paths_env;
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
    // Snapshot the setup overhead (selection + preflight + workspace sync)
    // gathered so far into the metadata embedded in the runner env (#3001), so
    // reports reading the offload metadata can separate `lab_overhead_ms` from
    // the workload command duration. Post-exec phases (remote_exec, output
    // parse, artifact import) are refreshed into the returned/captured metadata
    // after the run completes below.
    attach_lab_offload_overhead(&mut lab_metadata, &overhead);
    // Embed the host-telemetry opening snapshot + runner machine identity into
    // the metadata handed to the runner (#3258). The before/after delta is
    // emitted controller-side after the run, but recording the pre-run host
    // state and machine id here keeps the embedded metadata self-describing.
    lab_metadata["lab_host_telemetry"] = host_telemetry.before_metadata();
    let mut env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    env.extend(secret_env_handoff.env_delta.clone());
    for (name, value) in &request.job_overrides.env {
        env.insert(name.clone(), value.clone());
    }
    let mut secret_env_names = secret_env_handoff.secret_env_names;
    secret_env_names.extend(request.job_overrides.secret_env_names.clone());
    secret_env_names.sort();
    secret_env_names.dedup();
    // The remaining pre-dispatch checks (secret-env, provider, path translation)
    // are still runner setup overhead before the workload executes.
    let pre_dispatch_started = std::time::Instant::now();
    preflight_agent_task_runner_secret_env(
        runner_id,
        &runner,
        &changed_since_preflight.args,
        &env,
    )?;
    preflight_agent_task_provider_on_runner(
        runner_id,
        &command_prefix.argv,
        &remote_cwd,
        &source_path,
        &remapped_args,
        env.clone(),
        source_snapshot.clone(),
        contract.required_extensions.clone(),
        capability_preflight.clone(),
        &runner_homeboy,
        &runner_status,
    )?;
    // Path-translation preflight: the argv has already been routed through the
    // `rewrite_lab_offload_args` / `remap_*` translation pipeline above. Assert
    // that no controller-local source-checkout path survived un-translated
    // before we dispatch the command to the remote runner, so a missed remap
    // fails loudly here instead of corrupting the remote run.
    preflight_remote_argv_path_translation(
        "Lab offload",
        runner_id,
        &command,
        &source_path,
        &remote_cwd,
    )?;
    overhead.record(LabOffloadPhase::Preflight, pre_dispatch_started.elapsed());
    // Time the workload command itself (kept separate from overhead so reports
    // can subtract `lab_overhead_ms` from the workload duration).
    let remote_exec_started = std::time::Instant::now();
    let exec_result = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.clone()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env,
            secret_env_names,
            capture_patch: request.capture_patch,
            raw_exec: false,
            source_snapshot: Some(source_snapshot),
            capability_preflight,
            required_extensions: contract.required_extensions.clone(),
            require_paths: Vec::new(),
            runner_workload: Some(runner_workload),
            run_id: None,
            detach_after_handoff: request.detach_after_handoff,
        },
    );
    overhead.record(LabOffloadPhase::RemoteExec, remote_exec_started.elapsed());
    let (exec_output, exit_code) = match exec_result {
        Ok(output) => output,
        Err(err) => {
            if let Some(health) = runner_daemon_health_failure(&err) {
                let reason = health.reason.clone();
                plan = with_step(
                    plan,
                    PlanStep::builder("lab.exec", "lab.exec", PlanStepStatus::Failed)
                        .skip_reason(reason.clone())
                        .build(),
                );
                if let Some(job_id) = health.job_id.as_deref() {
                    if let Some(run_id) = agent_task_run_id.as_deref() {
                        return Ok(in_flight_daemon_disconnect_outcome(
                            plan, runner_id, job_id, run_id, &reason, &err,
                        ));
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
                                &selection,
                                "Lab offload selected a runner but its daemon did not respond",
                                &reason,
                                vec![format!(
                                    "Reconnect runner `{runner_id}` before retrying Lab offload."
                                )],
                            ))
                        } else {
                            Ok(LabOffloadOutcome::RunLocal {
                                metadata: Some(lab_offload_metadata_with_workspace_mapping(
                                    &plan,
                                    selection.source.metadata_value(),
                                    Some(runner_id),
                                    Some(status_tunnel_mode(&runner_status).metadata_value()),
                                    "fallback",
                                    Some(&remote_cwd),
                                    Some(&reason),
                                    Some(&workspace_mapping_metadata),
                                )),
                                plan,
                                messages: vec![format!(
                                    "Lab offload: {reason}; running locally."
                                )],
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

    let add_success_step = |plan, id| {
        with_step(
            plan,
            PlanStep::builder(id, id, PlanStepStatus::Success).build(),
        )
    };
    plan = add_success_step(plan, "lab.exec");
    if exec_output.mirror_run_id.is_some() {
        plan = add_success_step(plan, "lab.mirror_evidence");
    }
    // Parsing/applying the remote command output (patch extraction) is post-
    // workload overhead, not workload time.
    let output_parse_started = std::time::Instant::now();
    if request.capture_patch && exit_code == 0 {
        let apply_output = apply_lab_offload_patch(&exec_output)?;
        if apply_output.is_none() {
            return Err(missing_mutation_patch_error(
                request.normalized_args,
                request.mutation_flag,
                &exec_output,
            ));
        }
        plan = with_lab_apply_patch_step(plan, apply_output);
    }
    overhead.record(LabOffloadPhase::OutputParse, output_parse_started.elapsed());
    // Importing the structured-output artifact back from the runner is overhead.
    let artifact_import_started = std::time::Instant::now();
    let output_file_content = match remote_output_file.as_deref() {
        Some(path) => Some(download_lab_output_file(runner_id, path)?),
        None => None,
    };
    overhead.record(
        LabOffloadPhase::ArtifactImport,
        artifact_import_started.elapsed(),
    );
    // The workload + artifact import are done: take the closing host snapshot
    // and surface the before/after delta controller-side (#3258). Best-effort
    // diagnostic only — it never affects the run outcome.
    let host_telemetry = host_telemetry.finish();
    eprintln!(
        "Lab offload host telemetry: {}",
        host_telemetry.to_metadata()
    );
    ensure_lab_offload_streams_not_truncated(&exec_output, output_file_content.is_some())?;
    mirror_agent_task_run_plan_lifecycle(request.normalized_args, &exec_output.stdout)?;

    let mut stderr = String::new();
    for message in messages {
        stderr.push_str(&message);
        stderr.push('\n');
    }
    stderr.push_str(&exec_output.stderr);
    if exit_code != 0 {
        // Remote-failure clarity (#3815): a non-zero offloaded exit can be
        // confusing because the failure surfaces controller-side as if it ran
        // locally. Lead with an explicit banner that names the runner and
        // remote workspace so a controller-vs-runner mismatch (a path/file that
        // exists locally but not on the runner, a missing remote dependency,
        // etc.) is obviously a remote failure, not a bug in the command itself.
        // This runs for every offloaded failure, including plain cooks that
        // have no agent-task run id.
        stderr.push_str(&format!(
            "Lab offload FAILED REMOTELY: command exited {exit_code} on runner `{runner_id}` (remote workspace `{remote_cwd}`), NOT on this machine. If the error references a path or file, check that it exists on runner `{runner_id}`, not just locally.\n"
        ));
        append_runner_component_registry_repair_hint(
            &mut stderr,
            &contract,
            runner_id,
            &remote_cwd,
            &exec_output.stdout,
            &exec_output.stderr,
        );
        append_runner_failure_context_summary(&mut stderr, &exec_output);
        if let Some(run_id) = agent_task_run_id.as_deref() {
            if let Some(handoff) = parse_offloaded_agent_task_handoff_from_outputs(
                &exec_output.stdout,
                &exec_output.stderr,
            )? {
                if let Some(record) = agent_task_lifecycle::record_remote_dispatch_failure(
                    agent_task_lifecycle::AgentTaskRemoteDispatchFailure {
                        identity: agent_task_lifecycle::RunDispatchIdentity { run_id, runner_id },
                        local_command: redact_argv(request.normalized_args),
                        remote_command: redact_argv(&remote_command),
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
                        plan,
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
                    remote_command: redact_argv(&remote_command),
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

    Ok(LabOffloadOutcome::Offloaded {
        plan,
        stdout: exec_output.stdout,
        stderr,
        exit_code,
        output_file_content,
    })
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
    if structured_output_available {
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
    error.details["capture"] =
        serde_json::to_value(capture).unwrap_or_else(|_| serde_json::json!({}));
    Err(error)
}

pub(crate) fn download_lab_output_file(runner_id: &str, remote_path: &str) -> Result<String> {
    let temp = local_lab_output_temp_path(runner_id, remote_path);
    let temp_text = temp.display().to_string();
    let client = ssh_client_for_lab_runner(runner_id)?;
    let output = client.download_file(remote_path, &temp_text);
    if !output.success {
        return Err(Error::internal_unexpected(format!(
            "Lab offload could not retrieve remote structured output `{remote_path}` from runner `{runner_id}`: {}",
            output.stderr.trim()
        ))
        .with_hint("The remote command was invoked with --output, but the runner-side file was not readable after execution.".to_string()));
    }
    let content = std::fs::read_to_string(&temp).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("read downloaded Lab output {}", temp.display())),
        )
    })?;
    let _ = std::fs::remove_file(&temp);
    Ok(content)
}

pub(crate) fn local_lab_output_temp_path(runner_id: &str, remote_path: &str) -> PathBuf {
    let sanitized_runner = runner_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let sanitized_remote = remote_path
        .chars()
        .rev()
        .take(48)
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    std::env::temp_dir().join(format!(
        "homeboy-lab-output-{sanitized_runner}-{sanitized_remote}"
    ))
}
