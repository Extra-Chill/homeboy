//! Workspace sync / remap staging for the standard (non-resident) offload path.

use super::*;

pub(crate) struct LabOffloadWorkspaceStage {
    pub(crate) plan: HomeboyPlan,
    pub(crate) sync_mode: RunnerWorkspaceSyncMode,
    pub(crate) changed_since_preflight: LabOffloadChangedSincePreflight,
    pub(crate) synced: RunnerWorkspaceSyncOutput,
    pub(crate) remote_cwd: String,
    pub(crate) workspace_mapping: Vec<LabWorkspaceMappingEntry>,
    pub(crate) source_snapshot: SourceSnapshot,
    pub(crate) remapped_args: Vec<String>,
    pub(crate) agent_task_run_id: Option<String>,
    pub(crate) command: Vec<String>,
    pub(crate) remote_command: Vec<String>,
    pub(crate) remote_output_file: Option<String>,
    pub(crate) synced_rigs: Vec<rig_materialization::LabOffloadRigSync>,
    pub(crate) rig_component_path_overrides: Vec<(String, String)>,
    /// Env-var overrides surfacing synced runtime-overlay remote paths to the
    /// hot command. Empty when no overlay declared `expose_remote_path_env`.
    pub(crate) runtime_overlay_env: Vec<(String, String)>,
    /// Offload-evidence metadata for the synced runtime overlays.
    pub(crate) runtime_overlay_metadata: serde_json::Value,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_lab_offload_workspace_stage(
    request: &LabOffloadRequest<'_>,
    contract: &LabOffloadCommand,
    plan: HomeboyPlan,
    runner_id: &str,
    source_path: &Path,
    homeboy_path: &str,
    command_prefix_argv: &[String],
    runner_workspace_root: Option<&str>,
) -> Result<LabOffloadWorkspaceStage> {
    // Capture the orchestration facts known *before* staging so any
    // Lab-cannot-proceed error bubbling out of the pre-execution/dispatch path
    // names the selected runner, primary workspace, and ref/base, plus a
    // Homeboy command to fix it — instead of a bare resolver/sync error that
    // forces the operator to SSH into the runner to reconstruct context
    // (#4336).
    let context = LabOrchestrationContext::for_runner_workspace(
        runner_id,
        &source_path.display().to_string(),
    )
    .with_ref_base(lab_offload_changed_since_ref(request.normalized_args));

    prepare_lab_offload_workspace_stage_inner(
        request,
        contract,
        plan,
        runner_id,
        source_path,
        homeboy_path,
        command_prefix_argv,
        runner_workspace_root,
    )
    .map_err(|error| enrich_lab_cannot_proceed_error(error, &context))
}

#[allow(clippy::too_many_arguments)]
fn prepare_lab_offload_workspace_stage_inner(
    request: &LabOffloadRequest<'_>,
    contract: &LabOffloadCommand,
    mut plan: HomeboyPlan,
    runner_id: &str,
    source_path: &Path,
    homeboy_path: &str,
    command_prefix_argv: &[String],
    runner_workspace_root: Option<&str>,
) -> Result<LabOffloadWorkspaceStage> {
    let sync_mode = lab_workspace_sync_mode(
        contract.workspace_mode_policy,
        request.normalized_args,
        source_path,
    )?;
    let changed_since_preflight = if sync_mode == RunnerWorkspaceSyncMode::Git {
        prepare_git_lab_offload_changed_since(request.normalized_args, source_path)?
    } else {
        preflight_lab_offload_changed_since(request.normalized_args, sync_mode)?
    };
    let mut git_fetch_refs = changed_since_preflight.git_fetch_refs.clone();
    for git_ref in
        lab_offload_git_fetch_refs(&changed_since_preflight.args, source_path, sync_mode)?
    {
        if !git_fetch_refs.contains(&git_ref) {
            git_fetch_refs.push(git_ref);
        }
    }
    if let Some(resolved_base) = &changed_since_preflight.resolved_base {
        eprintln!(
            "Lab offload: changed-since requested `{}` resolved to `{}`; runner fetch refs: {}.",
            changed_since_preflight
                .requested_ref
                .as_deref()
                .unwrap_or(resolved_base),
            resolved_base,
            if git_fetch_refs.is_empty() {
                "<none>".to_string()
            } else {
                git_fetch_refs.join(", ")
            }
        );
    }
    let offload_args =
        inject_agent_task_default_provider_config_in_args(&changed_since_preflight.args)?;
    let mut extra_workspaces = lab_extra_workspaces(source_path)?;
    // Sync any controller-local directories referenced by --provider-config
    // (runtime components, provider plugins, extra mount sources) so the cook
    // config's paths resolve on the runner after remapping.
    extra_workspaces.extend(provider_config_extra_workspaces(
        &offload_args,
        source_path,
    )?);
    extra_workspaces.extend(agent_task_plan_extra_workspaces(
        &offload_args,
        source_path,
    )?);
    extra_workspaces.extend(agent_task_fanout_extra_workspaces(
        &offload_args,
        source_path,
    )?);
    extra_workspaces.extend(agent_task_provider_runtime_component_extra_workspaces(
        &offload_args,
        source_path,
    )?);
    extra_workspaces.extend(path_setting_extra_workspaces(&offload_args, source_path)?);
    extra_workspaces.extend(rig_component_path_env_extra_workspaces(source_path)?);
    // Isolate the primary workspace per cook/dispatch run. Without a per-run
    // token the git-mode remote path is keyed only on (source path, HEAD), so a
    // later unrelated run at the same HEAD reuses the earlier run's checkout and
    // can observe its leftover untracked artifacts (#4393). Resolve the
    // agent-task run id (existing or freshly generated) up front and fold it
    // into the workspace identity so each run gets a clean, isolated directory.
    let run_isolation_token = agent_task_dispatch_run_isolation_token(request.normalized_args);
    let synced = sync_workspace(
        runner_id,
        RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            mode: sync_mode,
            controller_routed_git: false,
            changed_since_base: changed_since_preflight.resolved_base.clone(),
            git_fetch_refs: git_fetch_refs.clone(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
            run_isolation_token: run_isolation_token.clone(),
        },
    )?
    .0;
    let remote_cwd = synced.remote_path.clone();
    let mut workspace_mapping = vec![workspace_mapping_entry("primary", &synced)];
    // The primary workspace sync materializes each declared dependency checkout
    // alongside the primary remote path (as a sibling) and reports them as
    // `validation_dependencies`. Fold those into the offload workspace mapping so
    // their controller-local -> remote path pairs propagate into the remote
    // command's path remaps. Without this the dependency graph exists on the
    // runner but the offloaded command still carries controller-local dependency
    // paths, so a remote dependency resolver cannot find the materialized
    // checkouts (#3292). Components with no declared dependencies produce an
    // empty list here, leaving the single-checkout offload path unchanged.
    for dependency in &synced.validation_dependencies {
        workspace_mapping.push(workspace_mapping_entry_for_validation_dependency(
            dependency,
        ));
    }
    if !synced.validation_dependencies.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready(
                "lab.materialize_dependency_graph",
                "lab.materialize_dependency_graph",
            )
            .inputs(
                PlanValues::new()
                    .json("count", synced.validation_dependencies.len())
                    .json("dependencies", &synced.validation_dependencies),
            )
            .build(),
        );
    }
    plan = with_step(
        plan,
        PlanStep::ready("lab.sync_workspace", "lab.sync_workspace")
            .inputs(
                PlanValues::new()
                    .string("local_path", &synced.local_path)
                    .string("remote_path", &remote_cwd)
                    .string("mode", sync_mode.label())
                    .json(
                        "allow_dirty_lab_workspace",
                        request.allow_dirty_lab_workspace,
                    )
                    .json(
                        "changed_since_requested_ref",
                        &changed_since_preflight.requested_ref,
                    )
                    .json(
                        "changed_since_resolved_base",
                        &changed_since_preflight.resolved_base,
                    )
                    .json("git_fetch_refs", &git_fetch_refs)
                    .string("workspace_cleanliness", &synced.workspace_cleanliness),
            )
            .build(),
    );

    let at_file_specs =
        lab_at_file_specs(&offload_args, Path::new(&synced.local_path), &remote_cwd)?;
    materialize_lab_at_files_on_runner(runner_id, &at_file_specs)?;
    if !at_file_specs.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready("lab.materialize_at_files", "lab.materialize_at_files")
                .inputs(
                    PlanValues::new().json("count", at_file_specs.len()).json(
                        "files",
                        at_file_specs
                            .iter()
                            .map(|spec| {
                                serde_json::json!({
                                    "local_path": spec.local_path.display().to_string(),
                                    "remote_path": spec.remote_path.as_str(),
                                })
                            })
                            .collect::<Vec<_>>(),
                    ),
                )
                .build(),
        );
    }

    let synced_extra_workspaces = sync_extra_lab_workspaces(
        runner_id,
        &synced.local_path,
        extra_workspaces,
        &mut workspace_mapping,
    )?;
    if !synced_extra_workspaces.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready("lab.sync_extra_workspaces", "lab.sync_extra_workspaces")
                .inputs(
                    PlanValues::new()
                        .json("count", synced_extra_workspaces.len())
                        .json("workspaces", &synced_extra_workspaces),
                )
                .build(),
        );
    }

    // Materialize any declared runtime overlays: sync the built artifact
    // directory, then run the overlay's opaque dependency-install step on the
    // runner (after sync, before the hot command). This gives offloaded
    // runtimes a deterministic way to install their deps remotely without
    // syncing huge dependency trees. No-overlay offload returns an empty list
    // here, leaving behavior unchanged (#3831).
    let synced_runtime_overlays = sync_lab_runtime_overlays(
        runner_id,
        &synced.local_path,
        lab_runtime_overlays()?,
        &mut workspace_mapping,
    )?;
    let runtime_overlay_env = runtime_overlay_env_overrides(&synced_runtime_overlays);
    let runtime_overlay_metadata = lab_runtime_overlay_metadata(&synced_runtime_overlays);
    if !synced_runtime_overlays.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready("lab.sync_runtime_overlays", "lab.sync_runtime_overlays")
                .inputs(
                    PlanValues::new()
                        .json("count", synced_runtime_overlays.len())
                        .json("overlays", &synced_runtime_overlays),
                )
                .build(),
        );
    }

    let source_snapshot = SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&remote_cwd),
        "lab_offload",
    );
    if contract.routing_policy.requires_extension_parity {
        plan = with_step(
            plan,
            PlanStep::ready("lab.extension_parity", "lab.extension_parity").build(),
        );
    }

    let synced_rigs = rig_materialization::sync_lab_offload_rigs(
        runner_id,
        homeboy_path,
        &remote_cwd,
        &changed_since_preflight.args,
        rig_materialization::LabOffloadPrimaryRigSource {
            local_path: &synced.local_path,
            remote_path: &remote_cwd,
            source_snapshot: &source_snapshot,
            workspace_snapshot_identity: &synced.snapshot_identity,
        },
    )?;
    if !synced_rigs.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready("lab.sync_rigs", "lab.sync_rigs")
                .inputs(
                    PlanValues::new()
                        .json("count", synced_rigs.len())
                        .string("source_snapshot_remote_path", &remote_cwd)
                        .json("rigs", &synced_rigs),
                )
                .build(),
        );
    }

    let rig_component_sync = rig_materialization::sync_lab_offload_rig_component_dependencies(
        runner_id,
        &changed_since_preflight.args,
        &synced.local_path,
        &remote_cwd,
        runner_workspace_root,
        request.allow_dirty_lab_workspace,
    )?;
    let synced_rig_dependencies = rig_component_sync.materializations;
    let rig_component_path_overrides = rig_component_sync.component_path_env;
    if !synced_rig_dependencies.is_empty() {
        for dependency in &synced_rig_dependencies {
            workspace_mapping.extend(workspace_mapping_entries_for_git_dependency(
                "rig_component_dependency",
                dependency,
            ));
        }
        plan = with_step(
            plan,
            PlanStep::ready(
                "lab.sync_rig_component_dependencies",
                "lab.sync_rig_component_dependencies",
            )
            .inputs(
                PlanValues::new()
                    .json("count", synced_rig_dependencies.len())
                    .json("dependencies", &synced_rig_dependencies),
            )
            .build(),
        );
    }

    // Remap controller-local absolute paths embedded in --provider-config
    // (mounts, workspace_root, runtime_component_paths, provider_plugin_paths)
    // to their synced remote locations, using every local->remote pair recorded
    // during workspace sync. Without this the remote sandbox cannot resolve the
    // workspace or runtime components a hand-authored cook config references.
    let path_remaps = path_remaps_from_workspace_mapping(&workspace_mapping);
    preflight_provider_config_source_cli_dependencies(&offload_args, &synced.excludes)?;
    preflight_provider_config_paths_materialized_in_args(&offload_args, &path_remaps)?;
    let remapped_args = rig_materialization::remap_bench_rig_default_component_to_primary_snapshot(
        &offload_args,
        &remote_cwd,
    );
    let remapped_args = remap_provider_config_in_args(&remapped_args, &path_remaps)?;
    let agent_task_specs = materialize_agent_task_specs_in_args(
        &remapped_args,
        &path_remaps,
        Path::new(&synced.local_path),
        |spec| sync_inline_agent_task_file(runner_id, spec),
    )?;
    let remapped_args = agent_task_specs.argv;
    for synced_entry in agent_task_specs.workspace_entries {
        plan = record_synced_remapped_workspace_entry(
            plan,
            &mut workspace_mapping,
            Some(synced_entry.entry),
            synced_entry.step_id,
        );
    }
    let path_remaps = path_remaps_from_workspace_mapping(&workspace_mapping);
    let remapped_args = remap_path_settings_in_args(&remapped_args, &path_remaps);
    let remapped_args = remap_lab_at_file_args(&remapped_args, &at_file_specs);
    let (remapped_args, agent_task_run_id) =
        ensure_agent_task_dispatch_run_id_with(&remapped_args, run_isolation_token.as_deref())
            .map_or((remapped_args, None), |(args, run_id)| (args, Some(run_id)));

    let remote_output_file = request
        .output_file_requested
        .then(|| remote_lab_output_file(&remote_cwd));
    let mut command = command_prefix_argv.to_vec();
    if !args_contain_output_file(&remapped_args) {
        if let Some(path) = &remote_output_file {
            command.push("--output".to_string());
            command.push(path.clone());
        }
    }
    command.extend(
        rewrite_lab_offload_args(
            &remapped_args,
            &remote_cwd,
            &path_remaps,
            remote_output_file.as_deref(),
        )
        .into_iter()
        .skip(1),
    );
    let remote_command = command.clone();
    plan = with_step(
        plan,
        PlanStep::ready("lab.rewrite_args", "lab.rewrite_args")
            .inputs(PlanValues::new().json("argv", &redact_argv(&command)))
            .build(),
    );

    Ok(LabOffloadWorkspaceStage {
        plan,
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
    })
}

fn path_remaps_from_workspace_mapping(
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> Vec<LabPathRemap> {
    workspace_mapping
        .iter()
        .map(|entry| LabPathRemap {
            local: entry.local_path().to_string(),
            remote: entry.remote_path().to_string(),
        })
        .collect()
}
