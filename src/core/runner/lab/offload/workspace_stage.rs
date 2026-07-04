//! Workspace sync / remap staging for the standard (non-resident) offload path.

use super::*;
use crate::core::rig;
use crate::core::runner_execution_envelope::{
    PathMaterializationEntry, PathMaterializationPlan,
    PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
    PATH_MATERIALIZATION_OWNER_LAB_PROVIDER_CONFIG, PATH_MATERIALIZATION_STATUS_MATERIALIZED,
};

pub(crate) struct LabOffloadWorkspaceStage {
    pub(crate) plan: HomeboyPlan,
    pub(crate) sync_mode: RunnerWorkspaceSyncMode,
    pub(crate) changed_since_preflight: LabOffloadChangedSincePreflight,
    pub(crate) synced: RunnerWorkspaceSyncOutput,
    pub(crate) remote_cwd: String,
    pub(crate) workspace_mapping: Vec<LabWorkspaceMappingEntry>,
    pub(crate) path_materialization_plan: PathMaterializationPlan,
    pub(crate) source_snapshot: SourceSnapshot,
    pub(crate) remapped_args: Vec<String>,
    pub(crate) agent_task_run_id: Option<String>,
    pub(crate) runner_required_extensions: Vec<String>,
    pub(crate) command: Vec<String>,
    pub(crate) remote_command: Vec<String>,
    pub(crate) remote_output_file: Option<String>,
    pub(crate) synced_rigs: Vec<rig_materialization::LabOffloadRigSync>,
    pub(crate) rig_component_path_overrides: Vec<(String, String)>,
    pub(crate) dependency_cache_saves: Vec<RunnerDependencyCacheSaveRequest>,
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
    let runner = load(runner_id)?;
    preflight_agent_task_secret_env_before_workspace_stage(
        contract,
        runner_id,
        &runner,
        &offload_args,
    )?;
    let (offload_args, workspace_ref_resolutions) =
        resolve_path_setting_workspace_refs_in_args(&offload_args)?;
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
    extra_workspaces.extend(workspace_ref_extra_workspaces(
        &workspace_ref_resolutions,
        source_path,
    )?);
    extra_workspaces.extend(path_setting_extra_workspaces(&offload_args, source_path)?);
    extra_workspaces.extend(runtime_refresh_source_extra_workspaces(
        &offload_args,
        source_path,
        request.allow_dirty_lab_workspace,
    )?);
    extra_workspaces.extend(extension_source_extra_workspaces(
        &offload_args,
        source_path,
        request.allow_dirty_lab_workspace,
    )?);
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
                    .json("materialization_plan", &synced.materialization_plan)
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

    let mut source_snapshot = SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&remote_cwd),
        "lab_offload",
    );
    source_snapshot.workspace_snapshot_identity = Some(synced.snapshot_identity.clone());
    validate_lab_source_snapshot_handoff(source_path, &synced, &source_snapshot)?;
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
    let dependency_cache_saves = rig_component_sync.dependency_cache_saves;
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
    let path_remaps = path_remaps_from_workspace_mapping(
        &workspace_mapping,
        Some(source_path),
        Some(&remote_cwd),
    );
    preflight_provider_config_source_cli_dependencies(&offload_args, &synced.excludes)?;
    preflight_provider_config_paths_materialized_in_args(&offload_args, &path_remaps)?;
    let remapped_args = rig_materialization::remap_rig_default_component_to_primary_snapshot(
        &offload_args,
        &remote_cwd,
    );
    let provider_config_materialization_plan =
        provider_config_path_materialization_plan(contract, sync_mode, &workspace_mapping);
    let remapped_args = remap_provider_config_with_materialization_plan_in_args(
        &remapped_args,
        &provider_config_materialization_plan,
    )?;
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
    let late_path_setting_workspaces =
        path_setting_extra_workspaces(&remapped_args, Path::new(&synced.local_path))?;
    let late_synced_path_settings = sync_extra_lab_workspaces(
        runner_id,
        &synced.local_path,
        late_path_setting_workspaces,
        &mut workspace_mapping,
    )?;
    if !late_synced_path_settings.is_empty() {
        plan = with_step(
            plan,
            PlanStep::ready("lab.sync_late_path_settings", "lab.sync_late_path_settings")
                .inputs(
                    PlanValues::new()
                        .json("count", late_synced_path_settings.len())
                        .json("workspaces", &late_synced_path_settings),
                )
                .build(),
        );
    }
    let path_remaps = path_remaps_from_workspace_mapping(
        &workspace_mapping,
        Some(source_path),
        Some(&remote_cwd),
    );
    let remapped_args = remap_path_settings_in_args(&remapped_args, &path_remaps);
    let remapped_args = remap_lab_at_file_args(&remapped_args, &at_file_specs);
    let (remapped_args, agent_task_run_id) =
        ensure_agent_task_dispatch_run_id_with(&remapped_args, run_isolation_token.as_deref())
            .map_or((remapped_args, None), |(args, run_id)| (args, Some(run_id)));

    let remote_output_file = request
        .output_file_requested
        .then(|| remote_lab_output_file(&remote_cwd));
    let runner_command_plan = RunnerCommandPlan::for_offload(
        &remapped_args,
        &contract.required_extensions,
        Path::new(&synced.local_path),
    )?;
    let runner_required_extensions = runner_command_plan.required_extensions.clone();
    let command = build_lab_offload_remote_command(
        command_prefix_argv,
        &remapped_args,
        &remote_cwd,
        &path_remaps,
        remote_output_file.as_deref(),
        &runner_command_plan,
    );
    let remote_command = command.clone();
    plan = with_step(
        plan,
        PlanStep::ready("lab.rewrite_args", "lab.rewrite_args")
            .inputs(PlanValues::new().json("argv", &redact_argv(&command)))
            .build(),
    );
    let path_materialization_plan =
        lab_execution_path_materialization_plan(sync_mode, &workspace_mapping);

    Ok(LabOffloadWorkspaceStage {
        plan,
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
        command,
        remote_command,
        remote_output_file,
        synced_rigs,
        rig_component_path_overrides,
        dependency_cache_saves,
        runtime_overlay_env,
        runtime_overlay_metadata,
    })
}

pub(crate) fn lab_execution_path_materialization_plan(
    sync_mode: RunnerWorkspaceSyncMode,
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> PathMaterializationPlan {
    PathMaterializationPlan::new(workspace_mapping.iter().map(|entry| {
        PathMaterializationEntry::new(
            entry.role(),
            PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT,
            Some(entry.local_path().to_string()),
            entry.remote_path(),
            sync_mode.label(),
            PATH_MATERIALIZATION_STATUS_MATERIALIZED,
        )
    }))
}

fn preflight_agent_task_secret_env_before_workspace_stage(
    contract: &LabOffloadCommand,
    runner_id: &str,
    runner: &crate::core::runner::Runner,
    args: &[String],
) -> Result<()> {
    if !contract
        .secret_env_sources
        .contains(&crate::command_contract::LabSecretEnvSource::AgentTask)
    {
        return Ok(());
    }

    let handoff = build_lab_secret_env_handoff_plan(
        &contract.secret_env_sources,
        args,
        std::collections::HashMap::new(),
    )?;
    preflight_lab_secret_env_handoff(runner_id, Some(runner), &handoff.env_delta, &handoff)?;
    preflight_agent_task_runner_secret_env_plan(
        runner_id,
        runner,
        args,
        &handoff.env_delta,
        &handoff.secret_env_plan,
    )
}

pub(crate) fn validate_lab_source_snapshot_handoff(
    requested_source_path: &Path,
    synced: &RunnerWorkspaceSyncOutput,
    source_snapshot: &SourceSnapshot,
) -> Result<()> {
    let expected_local_path = requested_source_path
        .canonicalize()
        .unwrap_or_else(|_| requested_source_path.to_path_buf())
        .display()
        .to_string();
    let actual_synced_local_path = synced.local_path.trim();
    let actual_snapshot_local_path = source_snapshot.local_path.as_deref().unwrap_or("").trim();
    let expected_remote_path = synced.remote_path.trim();
    let actual_snapshot_remote_path = source_snapshot.remote_path.as_deref().unwrap_or("").trim();
    let expected_workspace_identity = synced.snapshot_identity.trim();
    let actual_workspace_identity = source_snapshot
        .workspace_snapshot_identity
        .as_deref()
        .unwrap_or("")
        .trim();

    let mut mismatches = Vec::new();
    if actual_synced_local_path != expected_local_path {
        mismatches.push(format!(
            "workspace sync local_path `{actual_synced_local_path}` did not match requested source path `{expected_local_path}`"
        ));
    }
    if actual_snapshot_local_path != expected_local_path {
        mismatches.push(format!(
            "source snapshot local_path `{actual_snapshot_local_path}` did not match requested source path `{expected_local_path}`"
        ));
    }
    if actual_snapshot_remote_path != expected_remote_path {
        mismatches.push(format!(
            "source snapshot remote_path `{actual_snapshot_remote_path}` did not match synced remote path `{expected_remote_path}`"
        ));
    }
    if actual_workspace_identity != expected_workspace_identity {
        mismatches.push(format!(
            "source snapshot workspace identity `{actual_workspace_identity}` did not match synced workspace identity `{expected_workspace_identity}`"
        ));
    }

    if mismatches.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "source_snapshot",
        "Lab offload source snapshot does not match the materialized runner workspace",
        Some(format!(
            "requested_source_path={expected_local_path}; synced_local_path={actual_synced_local_path}; synced_remote_path={expected_remote_path}; snapshot_local_path={actual_snapshot_local_path}; snapshot_remote_path={actual_snapshot_remote_path}; synced_workspace_identity={expected_workspace_identity}; snapshot_workspace_identity={actual_workspace_identity}; snapshot_hash={}",
            source_snapshot.snapshot_hash
        )),
        Some(
            mismatches
                .into_iter()
                .chain(std::iter::once(
                    "Retry after syncing the intended local worktree; Homeboy refuses to dispatch Lab work against an ambiguous source snapshot.".to_string(),
                ))
                .collect(),
        ),
    ))
}

pub(crate) fn path_remaps_from_workspace_mapping(
    workspace_mapping: &[LabWorkspaceMappingEntry],
    primary_source_path: Option<&Path>,
    primary_remote_path: Option<&str>,
) -> Vec<LabPathRemap> {
    let mut remaps = workspace_mapping
        .iter()
        .map(|entry| LabPathRemap {
            local: entry.local_path().to_string(),
            remote: entry.remote_path().to_string(),
        })
        .collect::<Vec<_>>();

    if let (Some(source_path), Some(remote_path)) = (primary_source_path, primary_remote_path) {
        let source_path = source_path.display().to_string();
        if !source_path.trim().is_empty() && !remaps.iter().any(|remap| remap.local == source_path)
        {
            remaps.push(LabPathRemap {
                local: source_path,
                remote: remote_path.to_string(),
            });
        }
    }

    remaps
}

fn provider_config_path_materialization_plan(
    contract: &LabOffloadCommand,
    sync_mode: RunnerWorkspaceSyncMode,
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> PathMaterializationPlan {
    let materialization_mode = lab_path_materialization_mode(contract, sync_mode);
    PathMaterializationPlan::new(workspace_mapping.iter().map(|entry| {
        PathMaterializationEntry::new(
            entry.role(),
            PATH_MATERIALIZATION_OWNER_LAB_PROVIDER_CONFIG,
            Some(entry.local_path().to_string()),
            entry.remote_path(),
            materialization_mode.clone(),
            PATH_MATERIALIZATION_STATUS_MATERIALIZED,
        )
    }))
}

fn lab_path_materialization_mode(
    contract: &LabOffloadCommand,
    sync_mode: RunnerWorkspaceSyncMode,
) -> String {
    if matches!(
        contract.workspace_mode_policy,
        LabOffloadWorkspaceModePolicy::RunnerResident
    ) {
        return "existing_remote".to_string();
    }
    sync_mode.label().to_string()
}

fn rewrite_lab_offload_remote_command_args(
    args: &[String],
    remote_cwd: &str,
    path_remaps: &[LabPathRemap],
    remote_output_file: Option<&str>,
) -> Vec<String> {
    let args = rewrite_lab_offload_args(args, remote_cwd, path_remaps, remote_output_file);
    remap_path_settings_in_args(&args, path_remaps)
}

fn build_lab_offload_remote_command(
    command_prefix_argv: &[String],
    remapped_args: &[String],
    remote_cwd: &str,
    path_remaps: &[LabPathRemap],
    remote_output_file: Option<&str>,
    plan: &RunnerCommandPlan,
) -> Vec<String> {
    let mut command = command_prefix_argv.to_vec();
    if !args_contain_output_file(remapped_args) {
        if let Some(path) = remote_output_file {
            command.push("--output".to_string());
            command.push(path.to_string());
        }
    }

    let remote_args = rewrite_lab_offload_remote_command_args(
        remapped_args,
        remote_cwd,
        path_remaps,
        remote_output_file,
    );
    let remote_args = inject_required_extension_args(remote_args, &plan.command_extensions);
    command.extend(remote_args.into_iter().skip(1));
    command
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunnerCommandPlan {
    required_extensions: Vec<String>,
    command_extensions: Vec<String>,
}

impl RunnerCommandPlan {
    fn for_offload(
        args: &[String],
        route_required_extensions: &[String],
        primary_source_path: &Path,
    ) -> Result<Self> {
        let rig_extensions = rig_required_extensions_from_primary_rig(args, primary_source_path)?;
        let rig_dispatch_extensions =
            rig_dispatch_extensions_from_primary_rig(args, primary_source_path)?;
        let mut required_extensions = std::collections::BTreeSet::new();
        required_extensions.extend(route_required_extensions.iter().cloned());
        required_extensions.extend(rig_extensions);
        let command_extensions = if rig_dispatch_extensions.is_empty() {
            route_required_extensions.to_vec()
        } else {
            rig_dispatch_extensions
        };
        Ok(Self {
            required_extensions: required_extensions.into_iter().collect(),
            command_extensions,
        })
    }
}

fn rig_required_extensions_from_primary_rig(
    args: &[String],
    primary_source_path: &Path,
) -> Result<Vec<String>> {
    let Some(command) = rig_workload_command(args) else {
        return Ok(Vec::new());
    };

    let rig_ids = rig_ids_from_args(args);
    if rig_ids.is_empty() {
        return Ok(Vec::new());
    }

    let explicit_component = rig_workload_component_from_args(args, command);
    let explicit_extensions = extension_overrides_from_args(args);
    let mut extension_ids = std::collections::BTreeSet::new();
    for rig_id in rig_ids {
        let Some(spec) = load_primary_rig_spec(primary_source_path, &rig_id)? else {
            continue;
        };
        if !explicit_extensions.is_empty() {
            extension_ids.extend(explicit_extensions.iter().cloned());
            continue;
        }
        for extension_id in rig::extension_ids_for_workloads(&spec, command.workload_kind()) {
            extension_ids.insert(extension_id.clone());
            extension_ids.extend(rig::env_provider_extensions_for_extension_workloads(
                &spec,
                command.workload_kind(),
                &extension_id,
            ));
        }
        extension_ids.extend(rig_component_extensions_from_workload_command(
            &spec,
            command,
            explicit_component.as_deref(),
        ));
    }

    Ok(extension_ids.into_iter().collect())
}

fn rig_dispatch_extensions_from_primary_rig(
    args: &[String],
    primary_source_path: &Path,
) -> Result<Vec<String>> {
    let Some(command) = rig_workload_command(args) else {
        return Ok(Vec::new());
    };

    let rig_ids = rig_ids_from_args(args);
    if rig_ids.is_empty() {
        return Ok(Vec::new());
    }

    let explicit_component = rig_workload_component_from_args(args, command);
    let explicit_extensions = extension_overrides_from_args(args);
    let mut extension_ids = std::collections::BTreeSet::new();
    for rig_id in rig_ids {
        let Some(spec) = load_primary_rig_spec(primary_source_path, &rig_id)? else {
            continue;
        };
        if !explicit_extensions.is_empty() {
            extension_ids.extend(explicit_extensions.iter().cloned());
            continue;
        }
        let workload_extensions = rig::extension_ids_for_workloads(&spec, command.workload_kind());
        if workload_extensions.len() == 1 {
            extension_ids.extend(workload_extensions);
        }
        extension_ids.extend(rig_component_extensions_from_workload_command(
            &spec,
            command,
            explicit_component.as_deref(),
        ));
    }

    Ok(extension_ids.into_iter().collect())
}

#[derive(Clone, Copy)]
enum RigWorkloadCommand {
    Bench,
    Fuzz,
}

impl RigWorkloadCommand {
    fn workload_kind(self) -> rig::RigWorkloadKind {
        match self {
            Self::Bench => rig::RigWorkloadKind::Bench,
            Self::Fuzz => rig::RigWorkloadKind::Fuzz,
        }
    }
}

fn rig_workload_command(args: &[String]) -> Option<RigWorkloadCommand> {
    if args.iter().any(|arg| arg == "bench") {
        return Some(RigWorkloadCommand::Bench);
    }
    if args.iter().any(|arg| arg == "fuzz") {
        return Some(RigWorkloadCommand::Fuzz);
    }
    None
}

fn load_primary_rig_spec(primary_source_path: &Path, rig_id: &str) -> Result<Option<rig::RigSpec>> {
    let Some(discovered) = rig::discover_rigs(primary_source_path)?
        .into_iter()
        .find(|candidate| candidate.id == rig_id)
    else {
        return Ok(None);
    };
    let spec = rig::load_local_source(
        &discovered.rig_path.to_string_lossy(),
        Some(discovered.id.as_str()),
    )?;
    Ok(Some(spec))
}

fn rig_component_extensions_from_workload_command(
    spec: &rig::RigSpec,
    command: RigWorkloadCommand,
    explicit_component: Option<&str>,
) -> Vec<String> {
    let component_ids = explicit_component
        .map(|id| vec![id.to_string()])
        .or_else(|| match command {
            RigWorkloadCommand::Bench => spec.bench.as_ref().map(|bench| {
                if bench.components.is_empty() {
                    bench.default_component.iter().cloned().collect()
                } else {
                    bench.components.clone()
                }
            }),
            RigWorkloadCommand::Fuzz => spec
                .fuzz
                .as_ref()
                .and_then(|fuzz| fuzz.default_component.as_ref())
                .map(|component| vec![component.clone()]),
        })
        .unwrap_or_default();

    let mut extension_ids = std::collections::BTreeSet::new();
    for component_id in component_ids {
        if let Some(extensions) = spec
            .components
            .get(&component_id)
            .and_then(|component| component.extensions.as_ref())
        {
            extension_ids.extend(extensions.keys().cloned());
        }
    }
    extension_ids.into_iter().collect()
}

fn rig_ids_from_args(args: &[String]) -> Vec<String> {
    values_for_flag(args, "--rig")
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn extension_overrides_from_args(args: &[String]) -> Vec<String> {
    values_for_flag(args, "--extension")
}

fn values_for_flag(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut passthrough = false;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        if arg == flag {
            if let Some(value) = iter.peek() {
                values.push((*value).to_string());
            }
            continue;
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            values.push(value.to_string());
        }
    }
    values
}

fn rig_workload_component_from_args(
    args: &[String],
    command: RigWorkloadCommand,
) -> Option<String> {
    let mut command_seen = false;
    let mut subcommand_seen = false;
    let mut skip_next = false;
    let flags_with_values = [
        "--extension",
        "--path",
        "--iterations",
        "--warmup",
        "--runs",
        "--run-id",
        "--shared-state",
        "--concurrency",
        "--matrix",
        "--runner-pool",
        "--matrix-max-tasks",
        "--matrix-max-queue-depth",
        "--expected-artifact",
        "--regression-threshold",
        "--setting",
        "--setting-json",
        "--status-file",
        "--report",
        "--rig",
        "--rig-order",
        "--rig-concurrency",
        "--scenario",
        "--profile",
        "--ci-profile",
        "--output",
        "--runner",
    ];
    let fuzz_flags_with_values = [
        "--extension",
        "--path",
        "--rig",
        "--setting",
        "--setting-json",
        "--workload",
        "--run-id",
        "--tracker-ref",
        "--seed",
        "--inventory",
        "--sequence-plan",
        "--max-duration",
        "--gate-profile",
        "--isolation",
        "--isolation-proof",
        "--expect-metric",
        "--action-model",
        "--exploration-policy",
        "--request-id",
        "--operation",
        "--operation-family",
        "--case-budget",
        "--duration-budget-seconds",
        "--campaign-manifest",
        "--campaign-workload",
        "--lab-runner",
        "--required-artifact",
        "--output",
        "--runner",
    ];
    for arg in args.iter().skip(1) {
        if arg == "--" {
            return None;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if !command_seen {
            if matches!(
                (command, arg.as_str()),
                (RigWorkloadCommand::Bench, "bench") | (RigWorkloadCommand::Fuzz, "fuzz")
            ) {
                command_seen = true;
            }
            continue;
        }
        if matches!(command, RigWorkloadCommand::Fuzz)
            && !subcommand_seen
            && matches!(arg.as_str(), "list" | "run" | "plan" | "run-campaign")
        {
            subcommand_seen = true;
            continue;
        }
        if arg.starts_with('-') {
            let takes_value = match command {
                RigWorkloadCommand::Bench => flags_with_values.contains(&arg.as_str()),
                RigWorkloadCommand::Fuzz => fuzz_flags_with_values.contains(&arg.as_str()),
            };
            if takes_value {
                skip_next = true;
            }
            continue;
        }
        return Some(arg.clone());
    }
    None
}

fn inject_required_extension_args(
    mut args: Vec<String>,
    required_extensions: &[String],
) -> Vec<String> {
    let missing_extensions = missing_required_extensions(&args, required_extensions);
    if missing_extensions.is_empty() {
        return args;
    }

    let Some(command_index) = args
        .iter()
        .position(|arg| command_accepts_extension_override(arg))
    else {
        return args;
    };

    let insert_at = command_index + 1;
    for extension in missing_extensions.iter().rev() {
        args.insert(insert_at, extension.clone());
        args.insert(insert_at, "--extension".to_string());
    }
    args
}

fn missing_required_extensions(args: &[String], required_extensions: &[String]) -> Vec<String> {
    let existing: std::collections::BTreeSet<String> =
        extension_overrides_from_args(args).into_iter().collect();
    required_extensions
        .iter()
        .filter(|extension| !existing.contains(*extension))
        .cloned()
        .collect()
}

fn command_accepts_extension_override(arg: &str) -> bool {
    matches!(
        arg,
        "audit" | "bench" | "fuzz" | "lint" | "refactor" | "review" | "test"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_plan(required_extensions: &[&str]) -> RunnerCommandPlan {
        RunnerCommandPlan {
            required_extensions: required_extensions
                .iter()
                .map(|extension| extension.to_string())
                .collect(),
            command_extensions: required_extensions
                .iter()
                .map(|extension| extension.to_string())
                .collect(),
        }
    }

    #[test]
    fn final_remote_command_remaps_bench_env_path_settings() {
        let controller_workspace = "/controller/workspaces/toolkit";
        let fixture_root = format!("{controller_workspace}/fixtures/websites");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "fixture-matrix".to_string(),
            "--setting".to_string(),
            format!("bench_env.FIXTURE_ROOT={fixture_root}"),
            format!("--setting=bench_env.TOOLKIT_ROOT={controller_workspace}"),
        ];
        let mappings = vec![
            LabPathRemap {
                local: fixture_root,
                remote: "/runner/workspaces/fixtures-websites".to_string(),
            },
            LabPathRemap {
                local: controller_workspace.to_string(),
                remote: "/runner/workspaces/toolkit".to_string(),
            },
        ];

        let command = rewrite_lab_offload_remote_command_args(
            &args,
            "/runner/workspaces/primary",
            &mappings,
            None,
        );

        assert_eq!(
            command,
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "bench".to_string(),
                "--rig".to_string(),
                "fixture-matrix".to_string(),
                "--setting".to_string(),
                "bench_env.FIXTURE_ROOT=/runner/workspaces/fixtures-websites".to_string(),
                "--setting=bench_env.TOOLKIT_ROOT=/runner/workspaces/toolkit".to_string(),
            ]
        );
        assert!(!command.iter().any(|arg| arg.contains("/controller/")));
    }

    #[test]
    fn final_remote_command_forwards_required_bench_extension() {
        let args = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "bench".to_string(),
            "node-project".to_string(),
            "--path".to_string(),
            "/controller/workspaces/node-project".to_string(),
            "--rig".to_string(),
            "wordpress-fixture".to_string(),
        ];

        let command = build_lab_offload_remote_command(
            &["/runner/bin/homeboy".to_string()],
            &args,
            "/runner/workspaces/node-project",
            &[],
            None,
            &command_plan(&["wordpress"]),
        );

        assert_eq!(
            command,
            vec![
                "/runner/bin/homeboy".to_string(),
                "--force-hot".to_string(),
                "bench".to_string(),
                "--extension".to_string(),
                "wordpress".to_string(),
                "node-project".to_string(),
                "--path".to_string(),
                "/runner/workspaces/node-project".to_string(),
                "--rig".to_string(),
                "wordpress-fixture".to_string(),
            ]
        );
    }

    #[test]
    fn final_remote_command_forwards_rig_component_bench_extension() {
        crate::test_support::with_isolated_home(|home| {
            let rigs_dir = home.path().join(".config/homeboy/rigs");
            std::fs::create_dir_all(&rigs_dir).expect("rigs dir");
            std::fs::write(
                rigs_dir.join("fixture-matrix.json"),
                r#"{
                    "components": {
                        "node-project": {
                            "path": "/controller/workspaces/node-project",
                            "extensions": { "wordpress": {} }
                        }
                    },
                    "bench": { "default_component": "node-project" }
                }"#,
            )
            .expect("rig spec");

            let args = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "bench".to_string(),
                "node-project".to_string(),
                "--path".to_string(),
                "/controller/workspaces/node-project".to_string(),
                "--rig".to_string(),
                "fixture-matrix".to_string(),
            ];
            let command = build_lab_offload_remote_command(
                &["/runner/bin/homeboy".to_string()],
                &args,
                "/runner/workspaces/node-project",
                &[],
                None,
                &command_plan(&["wordpress"]),
            );

            assert_eq!(
                command,
                vec![
                    "/runner/bin/homeboy".to_string(),
                    "--force-hot".to_string(),
                    "bench".to_string(),
                    "--extension".to_string(),
                    "wordpress".to_string(),
                    "node-project".to_string(),
                    "--path".to_string(),
                    "/runner/workspaces/node-project".to_string(),
                    "--rig".to_string(),
                    "fixture-matrix".to_string(),
                ]
            );
        });
    }

    #[test]
    fn final_remote_command_forwards_rig_bench_env_provider_extension() {
        crate::test_support::with_isolated_home(|home| {
            let rigs_dir = home.path().join(".config/homeboy/rigs");
            std::fs::create_dir_all(&rigs_dir).expect("rigs dir");
            std::fs::write(
                rigs_dir.join("static-site-importer-fixture-matrix.json"),
                r#"{
                    "components": {
                        "static-site-importer": {
                            "component_id": "static-site-importer",
                            "path": "/controller/workspaces/static-site-importer"
                        }
                    },
                    "bench": { "default_component": "static-site-importer" },
                    "bench_workloads": {
                        "nodejs": [
                            {
                                "path": "${package.root}/bench/static-site-fixture-matrix.bench.mjs",
                                "env_provider_extensions": ["wordpress"]
                            }
                        ]
                    }
                }"#,
            )
            .expect("rig spec");

            let args = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "bench".to_string(),
                "static-site-importer".to_string(),
                "--path".to_string(),
                "/controller/workspaces/static-site-importer".to_string(),
                "--rig".to_string(),
                "static-site-importer-fixture-matrix".to_string(),
            ];
            let command = build_lab_offload_remote_command(
                &["/runner/bin/homeboy".to_string()],
                &args,
                "/runner/workspaces/static-site-importer",
                &[],
                None,
                &command_plan(&["nodejs"]),
            );

            let nodejs_flag = command
                .windows(2)
                .position(|window| window == ["--extension", "nodejs"])
                .expect("final remote command includes --extension nodejs");
            let rig_flag = command
                .iter()
                .position(|arg| arg == "--rig")
                .expect("final remote command includes --rig");
            assert!(
                nodejs_flag < rig_flag,
                "--extension nodejs must be injected before --rig in final runner command: {}",
                command.join(" ")
            );
            assert_eq!(
                command,
                vec![
                    "/runner/bin/homeboy".to_string(),
                    "--force-hot".to_string(),
                    "bench".to_string(),
                    "--extension".to_string(),
                    "nodejs".to_string(),
                    "static-site-importer".to_string(),
                    "--path".to_string(),
                    "/runner/workspaces/static-site-importer".to_string(),
                    "--rig".to_string(),
                    "static-site-importer-fixture-matrix".to_string(),
                ]
            );
        });
    }

    #[test]
    fn runner_command_plan_for_primary_rig_adds_env_provider_extension_and_remaps_settings() {
        crate::test_support::with_isolated_home(|home| {
            let primary = home.path().join("primary-static-site-importer");
            let rig_dir = primary.join("rigs/static-site-importer-fixture-matrix");
            std::fs::create_dir_all(&rig_dir).expect("primary rig dir");
            std::fs::write(
                rig_dir.join("rig.json"),
                r#"{
                    "components": {
                        "static-site-importer": {
                            "component_id": "static-site-importer",
                            "path": "/controller/workspaces/static-site-importer"
                        }
                    },
                    "bench": { "default_component": "static-site-importer" },
                    "bench_workloads": {
                        "nodejs": [
                            {
                                "path": "${package.root}/bench/static-site-fixture-matrix.bench.mjs",
                                "env_provider_extensions": ["wordpress"]
                            }
                        ]
                    }
                }"#,
            )
            .expect("primary rig spec");

            let args = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "bench".to_string(),
                "static-site-importer".to_string(),
                "--path".to_string(),
                "/controller/workspaces/static-site-importer".to_string(),
                "--rig".to_string(),
                "static-site-importer-fixture-matrix".to_string(),
                "--setting".to_string(),
                "bench_env.FIXTURE_ROOT=/controller/workspaces/static-site-importer/fixtures"
                    .to_string(),
            ];
            let path_remaps = vec![LabPathRemap {
                local: "/controller/workspaces/static-site-importer".to_string(),
                remote: "/runner/workspaces/static-site-importer".to_string(),
            }];
            let plan =
                RunnerCommandPlan::for_offload(&args, &[], &primary).expect("runner command plan");

            let command = build_lab_offload_remote_command(
                &["/runner/bin/homeboy".to_string()],
                &args,
                "/runner/workspaces/static-site-importer",
                &path_remaps,
                None,
                &plan,
            );

            assert_eq!(
                plan.required_extensions,
                vec!["nodejs".to_string(), "wordpress".to_string()]
            );
            assert_eq!(plan.command_extensions, vec!["nodejs".to_string()]);
            assert_eq!(
                command,
                vec![
                    "/runner/bin/homeboy".to_string(),
                    "--force-hot".to_string(),
                    "bench".to_string(),
                    "--extension".to_string(),
                    "nodejs".to_string(),
                    "static-site-importer".to_string(),
                    "--path".to_string(),
                    "/runner/workspaces/static-site-importer".to_string(),
                    "--rig".to_string(),
                    "static-site-importer-fixture-matrix".to_string(),
                    "--setting".to_string(),
                    "bench_env.FIXTURE_ROOT=/runner/workspaces/static-site-importer/fixtures"
                        .to_string(),
                ]
            );
        });
    }

    #[test]
    fn runner_command_plan_for_primary_fuzz_rig_adds_workload_and_component_extensions() {
        crate::test_support::with_isolated_home(|home| {
            let primary = home.path().join("primary-homeboy-rigs");
            let rig_dir = primary.join("rigs/jetpack-api-route-inventory");
            std::fs::create_dir_all(&rig_dir).expect("primary rig dir");
            std::fs::write(
                rig_dir.join("rig.json"),
                r#"{
                    "components": {
                        "jetpack": {
                            "component_id": "jetpack",
                            "path": "/controller/workspaces/jetpack",
                            "extensions": { "wordpress": {} }
                        }
                    },
                    "fuzz": { "default_component": "jetpack" },
                    "fuzz_workloads": {
                        "nodejs": [
                            {
                                "path": "${package.root}/fuzz/jetpack-api-route-inventory.fuzz.mjs",
                                "env_provider_extensions": ["browser"]
                            }
                        ]
                    }
                }"#,
            )
            .expect("primary rig spec");

            let args = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "fuzz".to_string(),
                "list".to_string(),
                "--rig".to_string(),
                "jetpack-api-route-inventory".to_string(),
            ];
            let plan =
                RunnerCommandPlan::for_offload(&args, &[], &primary).expect("runner command plan");
            let command = build_lab_offload_remote_command(
                &["/runner/bin/homeboy".to_string()],
                &args,
                "/runner/workspaces/homeboy-rigs",
                &[],
                None,
                &plan,
            );

            assert_eq!(
                plan.required_extensions,
                vec![
                    "browser".to_string(),
                    "nodejs".to_string(),
                    "wordpress".to_string(),
                ]
            );
            assert_eq!(
                plan.command_extensions,
                vec!["nodejs".to_string(), "wordpress".to_string()]
            );
            assert_eq!(
                command,
                vec![
                    "/runner/bin/homeboy".to_string(),
                    "--force-hot".to_string(),
                    "fuzz".to_string(),
                    "--extension".to_string(),
                    "nodejs".to_string(),
                    "--extension".to_string(),
                    "wordpress".to_string(),
                    "list".to_string(),
                    "--rig".to_string(),
                    "jetpack-api-route-inventory".to_string(),
                ]
            );
        });
    }

    #[test]
    fn runner_command_plan_for_primary_fuzz_rig_preserves_campaign_workload() {
        crate::test_support::with_isolated_home(|home| {
            let primary = home.path().join("primary-homeboy-rigs");
            let rig_dir = primary.join("rigs/jetpack-api-route-inventory");
            std::fs::create_dir_all(&rig_dir).expect("primary rig dir");
            std::fs::write(
                rig_dir.join("rig.json"),
                r#"{
                    "components": {
                        "jetpack": {
                            "component_id": "jetpack",
                            "path": "/controller/workspaces/jetpack"
                        }
                    },
                    "fuzz": { "default_component": "jetpack" },
                    "fuzz_workloads": {
                        "nodejs": [
                            { "path": "${package.root}/fuzz/jetpack-api-route-inventory.fuzz.mjs" }
                        ]
                    }
                }"#,
            )
            .expect("primary rig spec");

            let args = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "fuzz".to_string(),
                "run-campaign".to_string(),
                "--execute".to_string(),
                "--rig".to_string(),
                "jetpack-api-route-inventory".to_string(),
                "--campaign-workload".to_string(),
                "rest-api-read".to_string(),
            ];
            let plan =
                RunnerCommandPlan::for_offload(&args, &[], &primary).expect("runner command plan");
            let command = build_lab_offload_remote_command(
                &["/runner/bin/homeboy".to_string()],
                &args,
                "/runner/workspaces/homeboy-rigs",
                &[],
                None,
                &plan,
            );

            assert_eq!(plan.required_extensions, vec!["nodejs".to_string()]);
            assert_eq!(plan.command_extensions, vec!["nodejs".to_string()]);
            assert_eq!(
                command,
                vec![
                    "/runner/bin/homeboy".to_string(),
                    "--force-hot".to_string(),
                    "fuzz".to_string(),
                    "--extension".to_string(),
                    "nodejs".to_string(),
                    "run-campaign".to_string(),
                    "--execute".to_string(),
                    "--rig".to_string(),
                    "jetpack-api-route-inventory".to_string(),
                    "--campaign-workload".to_string(),
                    "rest-api-read".to_string(),
                ]
            );
        });
    }

    #[test]
    fn final_remote_command_keeps_explicit_extension_override() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--extension".to_string(),
            "custom".to_string(),
            "node-project".to_string(),
            "--rig".to_string(),
            "wordpress-fixture".to_string(),
        ];

        let command = build_lab_offload_remote_command(
            &["/runner/bin/homeboy".to_string()],
            &args,
            "/runner/workspaces/node-project",
            &[],
            None,
            &command_plan(&["wordpress"]),
        );

        assert_eq!(
            command,
            vec![
                "/runner/bin/homeboy".to_string(),
                "--force-hot".to_string(),
                "bench".to_string(),
                "--extension".to_string(),
                "wordpress".to_string(),
                "--extension".to_string(),
                "custom".to_string(),
                "node-project".to_string(),
                "--rig".to_string(),
                "wordpress-fixture".to_string(),
            ]
        );
    }

    #[test]
    fn final_remote_command_remaps_bench_env_subdirectory_under_extra_workspace() {
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting".to_string(),
            "bench_env.CONFIG_DIR=/controller/workspaces/toolkit/config/matrix".to_string(),
        ];
        let mappings = vec![LabPathRemap {
            local: "/controller/workspaces/toolkit".to_string(),
            remote: "/runner/workspaces/toolkit".to_string(),
        }];

        let command = rewrite_lab_offload_remote_command_args(
            &args,
            "/runner/workspaces/primary",
            &mappings,
            None,
        );

        assert_eq!(
            command,
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "bench".to_string(),
                "--setting".to_string(),
                "bench_env.CONFIG_DIR=/runner/workspaces/toolkit/config/matrix".to_string(),
            ]
        );
    }

    #[test]
    fn final_remote_command_remaps_requested_primary_source_path_alias_in_passthrough_args() {
        let requested_source = Path::new(
            "/Users/chubes/Developer/static-site-importer@feat-imported-block-validity-gate",
        );
        let canonical_synced_source = "/private/var/folders/source-snapshot/static-site-importer";
        let remote_workspace = "/home/chubes/Developer/_lab_workspaces/static-site-importer";
        let synced = test_synced_workspace(canonical_synced_source, remote_workspace);
        let workspace_mapping = vec![workspace_mapping_entry("primary", &synced)];
        let path_remaps = path_remaps_from_workspace_mapping(
            &workspace_mapping,
            Some(requested_source),
            Some(remote_workspace),
        );
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "static-site-importer".to_string(),
            "--path".to_string(),
            requested_source.display().to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--lab-only".to_string(),
            "--rig".to_string(),
            "static-site-importer-fixture-matrix".to_string(),
            "--".to_string(),
            "--fixture-root".to_string(),
            "/Users/chubes/Developer/blocks-engine@fixtures-static-import-corpus/fixtures/websites/2-onepager-coffee".to_string(),
            "--max-depth".to_string(),
            "0".to_string(),
            "--static-site-importer-path".to_string(),
            requested_source.display().to_string(),
            "--batch-size".to_string(),
            "1".to_string(),
            "--run".to_string(),
        ];

        let command =
            rewrite_lab_offload_remote_command_args(&args, remote_workspace, &path_remaps, None);

        assert_eq!(
            command,
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "bench".to_string(),
                "static-site-importer".to_string(),
                "--path".to_string(),
                remote_workspace.to_string(),
                "--rig".to_string(),
                "static-site-importer-fixture-matrix".to_string(),
                "--".to_string(),
                "--fixture-root".to_string(),
                "/Users/chubes/Developer/blocks-engine@fixtures-static-import-corpus/fixtures/websites/2-onepager-coffee".to_string(),
                "--max-depth".to_string(),
                "0".to_string(),
                "--static-site-importer-path".to_string(),
                remote_workspace.to_string(),
                "--batch-size".to_string(),
                "1".to_string(),
                "--run".to_string(),
            ]
        );
        assert!(!command
            .iter()
            .any(|arg| arg == &requested_source.display().to_string()));
    }

    #[test]
    fn provider_config_materialization_plan_projects_lab_policy_and_mappings() {
        let synced = test_synced_workspace(
            "/controller/workspaces/provider-runtime",
            "/runner/workspaces/provider-runtime",
        );
        let workspace_mapping = vec![workspace_mapping_entry("primary", &synced)];
        let contract = LabOffloadCommand {
            hot_label: "agent-task.run",
            portable: true,
            unsupported_reason: None,
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::GitCheckoutRequired,
            secret_env_sources: Vec::new(),
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            routing_policy: crate::command_contract::LabRoutingPolicy::default(),
        };

        let plan = provider_config_path_materialization_plan(
            &contract,
            RunnerWorkspaceSyncMode::Git,
            &workspace_mapping,
        );

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].role, "primary");
        assert_eq!(plan.entries[0].owner, "lab.provider_config");
        assert_eq!(
            plan.entries[0].local_path.as_deref(),
            Some("/controller/workspaces/provider-runtime")
        );
        assert_eq!(
            plan.entries[0].remote_path,
            "/runner/workspaces/provider-runtime"
        );
        assert_eq!(plan.entries[0].materialization_mode, "git");
    }

    #[test]
    fn lab_execution_path_materialization_plan_projects_standard_workspace_mappings() {
        let synced = test_synced_workspace(
            "/controller/workspaces/homeboy",
            "/runner/workspaces/homeboy",
        );
        let workspace_mapping = vec![workspace_mapping_entry("primary", &synced)];

        let plan = lab_execution_path_materialization_plan(
            RunnerWorkspaceSyncMode::Snapshot,
            &workspace_mapping,
        );

        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].role, "primary");
        assert_eq!(plan.entries[0].owner, "lab.execution_context");
        assert_eq!(
            plan.entries[0].local_path.as_deref(),
            Some("/controller/workspaces/homeboy")
        );
        assert_eq!(plan.entries[0].remote_path, "/runner/workspaces/homeboy");
        assert_eq!(plan.entries[0].materialization_mode, "snapshot");
        assert_eq!(plan.entries[0].validation_status, "materialized");
    }

    #[test]
    fn preflight_agent_task_secret_env_before_workspace_stage_fails_missing_controller_secret() {
        let _secret = RemovedEnvVar::new("HOMEBOY_LAB_EARLY_MISSING_SECRET");
        let contract = test_lab_contract_with_agent_task_secrets();
        let runner = test_runner();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--backend".to_string(),
            "opencode".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_LAB_EARLY_MISSING_SECRET".to_string(),
        ];

        let err = preflight_agent_task_secret_env_before_workspace_stage(
            &contract, "lab", &runner, &args,
        )
        .expect_err("missing controller-forwarded secret should fail before workspace sync");

        assert_eq!(err.details["field"].as_str(), Some("secret-env"));
        assert!(err.message.contains("HOMEBOY_LAB_EARLY_MISSING_SECRET"));
        assert!(!err.to_string().contains("secret-value"));
    }

    fn test_lab_contract_with_agent_task_secrets() -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: "agent-task.run",
            portable: true,
            unsupported_reason: None,
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::GitCheckoutRequired,
            secret_env_sources: vec![crate::command_contract::LabSecretEnvSource::AgentTask],
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            routing_policy: crate::command_contract::LabRoutingPolicy::default(),
        }
    }

    fn test_runner() -> crate::core::runner::Runner {
        crate::core::runner::Runner {
            id: "lab".to_string(),
            kind: crate::core::runner::RunnerKind::Ssh,
            server_id: Some("server-a".to_string()),
            workspace_root: Some("/runner/workspaces".to_string()),
            settings: crate::core::server::RunnerSettings::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: crate::core::server::RunnerPolicy::default(),
        }
    }

    struct RemovedEnvVar {
        name: &'static str,
        value: Option<String>,
    }

    impl RemovedEnvVar {
        fn new(name: &'static str) -> Self {
            let value = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, value }
        }
    }

    impl Drop for RemovedEnvVar {
        fn drop(&mut self) {
            if let Some(value) = &self.value {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    fn test_synced_workspace(local_path: &str, remote_path: &str) -> RunnerWorkspaceSyncOutput {
        RunnerWorkspaceSyncOutput {
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: local_path.to_string(),
            remote_path: remote_path.to_string(),
            materialization_plan:
                crate::core::runner::RunnerWorkspaceMaterializationPlan::from_test_parts(
                    "/runner/workspaces",
                    local_path,
                    Path::new(local_path)
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("workspace"),
                    remote_path,
                    RunnerWorkspaceSyncMode::Snapshot,
                    "snapshot:primary",
                ),
            current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
                local_path: local_path.to_string(),
                remote_path: remote_path.to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                materialized: true,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
                synthetic_checkout_commit: None,
            },
            workspace_lease: crate::core::runner::RunnerWorkspaceLease {
                runner_id: "lab".to_string(),
                local_path: local_path.to_string(),
                remote_path: remote_path.to_string(),
                sync_mode: "snapshot".to_string(),
                materialized: true,
                lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
            },
            resource_lifecycle: crate::core::runner::workspace_resource_lifecycle(
                "lab",
                remote_path,
                None,
                crate::core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
            ),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: "snapshot:primary".to_string(),
            counts: crate::core::runner::ByteFileCounts::default(),
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "snapshot_unique_workspace".to_string(),
            validation_dependencies: Vec::new(),
        }
    }
}
