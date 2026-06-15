//! Lab offload request execution and runner-selection orchestration.
//!
//! This module is the working core of `core::runners::execute_lab_offload`. It
//! turns a `LabOffloadRequest` into either a local-fallback `RunLocal` outcome
//! or an `Offloaded` outcome by:
//!
//! 1. Validating the command contract and the user's runner choice
//!    (`execute_lab_offload`).
//! 2. Preparing/connecting the chosen runner.
//! 3. Walking through workspace sync, capability preflight, argument
//!    remapping, secret hydration, and remote exec inside
//!    `run_lab_offload_inner`.
//! 4. Translating runner failures into either a structured fallback outcome or
//!    a precise validation error, via the helpers at the bottom of the file.
//!
//! Trace-target git-fetch calculation lives in `trace_fetch_refs`; this module
//! only decides when those refs participate in workspace sync.

use std::path::Path;

use crate::core::agent_task_lifecycle;
use crate::core::observation::{PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV};
use crate::core::plan::{HomeboyPlan, PlanStep, PlanStepStatus, PlanValues};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, ErrorCode, Result};

use super::super::daemon_health::runner_daemon_health_failure;
use super::super::lab_apply::apply_lab_offload_patch;
use super::super::lab_args::{
    lab_offload_source_path, remap_agent_task_plan_in_args, remap_path_settings_in_args,
    remap_provider_config_in_args, rewrite_lab_offload_args, LabPathRemap,
};
use super::super::lab_capabilities::lab_runner_capability_contract;
use super::super::lab_command::lab_offload_command_prefix;
use super::super::lab_env::{
    build_lab_offload_env, forward_env_if_present, forward_release_ci_env,
    forward_rig_component_path_env, misplaced_runner_exec_wait_timeout_warning,
    settings_env_diagnostics,
};
use super::super::lab_plan::{base_lab_plan, disabled_select_runner_plan, with_step};
use super::super::lab_selection::{
    prepare_lab_runner_for_offload, resolve_lab_runner_selection, status_tunnel_mode,
    LabRunnerPreparation, LabRunnerSelection, LabRunnerSelectionSource,
};
use super::super::lab_workspaces::{
    agent_task_plan_extra_workspaces, lab_extra_workspaces, lab_workspace_mapping_metadata,
    path_setting_extra_workspaces, preflight_provider_config_source_cli_dependencies,
    provider_config_extra_workspaces, rig_component_path_env_extra_workspaces,
    sync_extra_lab_workspaces, workspace_mapping_entries_for_git_dependency,
    workspace_mapping_entry,
};
use super::super::{
    evaluate_lab_runner_capabilities_for_runner, exec, lab_offload_changed_since_ref,
    lab_offload_metadata, lab_offload_metadata_with_workspace_mapping, load,
    preflight_lab_offload_changed_since, prepare_git_lab_offload_changed_since,
    prepare_lab_runner_capability, rig_materialization, status, sync_workspace,
    LabRunnerGateDecision, RunnerCapabilityPreflight, RunnerExecOptions, RunnerStatusReport,
    RunnerTunnelMode, RunnerWorkspaceApplyOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions,
};

use super::agent_task_bridge::{
    ensure_agent_task_dispatch_run_id, lab_pre_dispatch_failure_message,
    materialize_inline_agent_task_plan_arg, materialize_inline_agent_task_tasks_arg,
    mirror_agent_task_run_plan_lifecycle, parse_offloaded_dispatch_envelope_from_outputs,
};
use super::evidence::terminal_lab_run_evidence;
use super::secrets::{hydrate_agent_task_secret_env, hydrate_trace_secret_env};
use super::trace_fetch_refs::lab_offload_git_fetch_refs;

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub allow_local_hot: bool,
    pub allow_local_fallback: bool,
    pub allow_dirty_lab_workspace: bool,
    pub capture_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabOffloadCommand {
    pub hot_label: &'static str,
    pub portable: bool,
    pub default_lab_offload: bool,
    pub unsupported_reason: Option<&'static str>,
    pub workspace_mode_policy: LabOffloadWorkspaceModePolicy,
    pub requires_extension_parity: bool,
    pub required_extensions: Vec<String>,
    pub requires_playwright: bool,
    pub infer_source_path_tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabOffloadWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
}

pub enum LabOffloadOutcome {
    RunLocal {
        plan: HomeboyPlan,
        metadata: Option<serde_json::Value>,
        messages: Vec<String>,
    },
    Offloaded {
        plan: HomeboyPlan,
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
}

fn requested_lab_workspace_sync_mode(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
) -> RunnerWorkspaceSyncMode {
    match policy {
        LabOffloadWorkspaceModePolicy::Git | LabOffloadWorkspaceModePolicy::GitCheckoutRequired => {
            RunnerWorkspaceSyncMode::Git
        }
        LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
            if lab_offload_changed_since_ref(args).is_some() {
                RunnerWorkspaceSyncMode::Git
            } else {
                RunnerWorkspaceSyncMode::Snapshot
            }
        }
    }
}

fn lab_workspace_sync_mode(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
    source_path: &Path,
) -> Result<RunnerWorkspaceSyncMode> {
    let requested = requested_lab_workspace_sync_mode(policy, args);
    if requested != RunnerWorkspaceSyncMode::Git {
        return Ok(requested);
    }

    if policy == LabOffloadWorkspaceModePolicy::GitCheckoutRequired {
        return Ok(RunnerWorkspaceSyncMode::Git);
    }

    let remote_url = super::super::workspace::git_output(
        source_path,
        &["config", "--get", "remote.origin.url"],
    )?;
    if super::super::source_materialization::requires_controller_routed_workspace_sync(&remote_url)
    {
        return Ok(RunnerWorkspaceSyncMode::Snapshot);
    }

    Ok(requested)
}

pub fn execute_lab_offload(request: LabOffloadRequest<'_>) -> Result<LabOffloadOutcome> {
    let unsupported_runner_error = |runner_id: &str, message: String| {
        Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            Some(vec!["Current Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts, audit, bench run, full lint, full test, trace, and refactor source runs.".to_string()]),
        )
    };
    let mut plan = base_lab_plan(request.command.as_ref());
    let Some(contract) = request.command.clone() else {
        if let Some(runner_id) = request.explicit_runner {
            return Err(unsupported_runner_error(
                runner_id,
                "--runner is only supported for commands with portable Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts, lint, test, audit, bench, trace, and refactor source runs".to_string(),
            ));
        }
        return Ok(LabOffloadOutcome::RunLocal {
            plan: disabled_select_runner_plan(plan, "command has no Lab contract"),
            metadata: None,
            messages: Vec::new(),
        });
    };

    if !contract.portable {
        if let Some(runner_id) = request.explicit_runner {
            let message = contract.unsupported_reason.map_or_else(
                || "--runner is only supported for commands with portable Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts, lint, test, audit, bench, trace, and refactor source runs".to_string(),
                |reason| format!("--runner is unavailable for this local-only resource-pressure command. {reason}"),
            );
            return Err(unsupported_runner_error(runner_id, message));
        }
        let reason = contract
            .unsupported_reason
            .unwrap_or("command is local-only");
        plan = disabled_select_runner_plan(plan, reason);
        return Ok(LabOffloadOutcome::RunLocal {
            metadata: Some(lab_offload_metadata(
                &plan,
                "automatic",
                None,
                None,
                "skipped",
                None,
                Some(reason),
            )),
            plan,
            messages: Vec::new(),
        });
    }

    if request.explicit_runner.is_none() && !contract.default_lab_offload {
        return Ok(LabOffloadOutcome::RunLocal {
            plan: disabled_select_runner_plan(plan, "automatic Lab offload disabled"),
            metadata: None,
            messages: Vec::new(),
        });
    }

    let selection = resolve_lab_runner_selection(
        &contract,
        request.explicit_runner,
        request.force_hot,
        request.allow_local_hot,
    )?;
    let Some(selection) = selection else {
        let reason = if request.force_hot && request.allow_local_hot {
            "force_hot_local_override"
        } else if request.force_hot {
            "force_hot"
        } else {
            "no_default_runner"
        };
        plan = with_step(
            plan,
            PlanStep::builder(
                "lab.select_runner",
                "lab.select_runner",
                PlanStepStatus::Skipped,
            )
            .skip_reason(reason)
            .build(),
        );
        return Ok(LabOffloadOutcome::RunLocal {
            metadata: Some(lab_offload_metadata(
                &plan,
                "automatic",
                None,
                None,
                "skipped",
                None,
                Some(reason),
            )),
            plan,
            messages: Vec::new(),
        });
    };

    let mut messages = Vec::new();
    if matches!(selection.source, LabRunnerSelectionSource::Default) {
        messages.push(format!(
            "Lab offload: auto-selected default {} runner `{}`.",
            selection.mode.label(),
            selection.runner_id
        ));
    }

    plan = with_step(
        plan,
        PlanStep::ready("lab.select_runner", "lab.select_runner")
            .inputs(
                PlanValues::new()
                    .string("runner_id", &selection.runner_id)
                    .string("source", selection.source.metadata_value())
                    .string("mode", selection.mode.metadata_value()),
            )
            .build(),
    );

    match prepare_lab_runner_for_offload(&selection)? {
        LabRunnerPreparation::Ready => {
            plan = with_step(
                plan,
                PlanStep::ready("lab.connect_runner", "lab.connect_runner").build(),
            );
        }
        LabRunnerPreparation::FallBackLocal { reason } => {
            plan = with_step(
                plan,
                PlanStep::builder(
                    "lab.connect_runner",
                    "lab.connect_runner",
                    PlanStepStatus::Failed,
                )
                .skip_reason(reason.clone())
                .build(),
            );
            if !request.allow_local_fallback {
                return Err(selected_runner_fallback_error(
                    &selection,
                    "Lab offload selected a runner but could not prepare it for remote execution",
                    &reason,
                    vec![format!(
                        "Reconnect runner `{}` before retrying Lab offload.",
                        selection.runner_id
                    )],
                ));
            }
            return Ok(LabOffloadOutcome::RunLocal {
                metadata: Some(lab_offload_metadata(
                    &plan,
                    selection.source.metadata_value(),
                    Some(&selection.runner_id),
                    Some(selection.mode.metadata_value()),
                    "fallback",
                    None,
                    Some(&reason),
                )),
                plan,
                messages: vec![format!("Lab offload: {reason}; running locally.")],
            });
        }
    }

    run_lab_offload_inner(request, selection, contract, plan, messages)
}

fn run_lab_offload_inner(
    request: LabOffloadRequest<'_>,
    selection: LabRunnerSelection,
    contract: LabOffloadCommand,
    mut plan: HomeboyPlan,
    mut messages: Vec<String>,
) -> Result<LabOffloadOutcome> {
    let runner_id = &selection.runner_id;
    let runner = load(runner_id)?;
    let runner_status = status(runner_id)?;
    if runner.kind != super::super::RunnerKind::Ssh {
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

    if request.capture_patch && status_tunnel_mode(&runner_status) == RunnerTunnelMode::Reverse {
        let reason =
            "Lab offload cannot yet return source-tree mutations from reverse runners".to_string();
        plan = with_step(
            plan,
            PlanStep::builder(
                "lab.mutation_return",
                "lab.mutation_return",
                PlanStepStatus::Missing,
            )
            .skip_reason(reason.clone())
            .build(),
        );
        return mutation_return_unavailable_outcome(plan, &selection, &runner_status, reason);
    }

    runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "Lab offload requires runner.workspace_root so the local checkout can be mapped remotely",
            Some(runner.id.clone()),
            Some(vec![
                "This Wave 3 adapter assumes workspace sync/provenance has placed the same checkout basename under runner.workspace_root.".to_string(),
            ]),
        )
    })?;

    let source_path =
        rig_materialization::lab_offload_rig_component_checkout_root(request.normalized_args)?
            .unwrap_or(lab_offload_source_path(request.normalized_args)?);
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
        });
    }
    if let Some(warning) = misplaced_runner_exec_wait_timeout_warning(request.normalized_args) {
        messages.push(warning);
    }
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let command_prefix = lab_offload_command_prefix(&source_path, homeboy_path);
    let capability_contract =
        lab_runner_capability_contract(&contract, &source_path, &command_prefix.required_tools);
    let capability_plan = capability_contract
        .clone()
        .map(prepare_lab_runner_capability);
    if let Some(capability_plan) = &capability_plan {
        let decision = match evaluate_lab_runner_capabilities_for_runner(
            &runner,
            capability_plan,
            selection.source.gate_mode(),
        ) {
            Ok(decision) => decision,
            Err(err) if matches!(selection.source, LabRunnerSelectionSource::Default) => {
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
                return Ok(automatic_capability_fallback(
                    plan,
                    runner_id,
                    &runner_status,
                    reason,
                ));
            }
            Err(err) => return Err(err),
        };

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
                    return automatic_capability_fallback_or_error(
                        plan,
                        &selection,
                        &runner_status,
                        reason,
                        remediation,
                        request.allow_local_fallback,
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
    let sync_mode = lab_workspace_sync_mode(
        contract.workspace_mode_policy,
        request.normalized_args,
        &source_path,
    )?;
    let changed_since_preflight = if sync_mode == RunnerWorkspaceSyncMode::Git {
        prepare_git_lab_offload_changed_since(request.normalized_args, &source_path)?
    } else {
        preflight_lab_offload_changed_since(request.normalized_args, sync_mode)?
    };
    let mut git_fetch_refs = changed_since_preflight.git_fetch_refs.clone();
    for git_ref in
        lab_offload_git_fetch_refs(&changed_since_preflight.args, &source_path, sync_mode)?
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
    let mut extra_workspaces = lab_extra_workspaces(&source_path)?;
    // Sync any controller-local directories referenced by --provider-config
    // (runtime components, provider plugins, extra mount sources) so the cook
    // config's paths resolve on the runner after remapping.
    extra_workspaces.extend(provider_config_extra_workspaces(
        &changed_since_preflight.args,
        &source_path,
    )?);
    extra_workspaces.extend(agent_task_plan_extra_workspaces(
        &changed_since_preflight.args,
        &source_path,
    )?);
    extra_workspaces.extend(path_setting_extra_workspaces(
        &changed_since_preflight.args,
        &source_path,
    )?);
    extra_workspaces.extend(rig_component_path_env_extra_workspaces(&source_path)?);
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
        },
    )?
    .0;
    let remote_cwd = synced.remote_path.clone();
    let mut workspace_mapping = vec![workspace_mapping_entry("primary", &synced)];
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

    let source_snapshot = SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&remote_cwd),
        "lab_offload",
    );
    if contract.requires_extension_parity {
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
        &synced.local_path,
        &remote_cwd,
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

    let synced_rig_dependencies = rig_materialization::sync_lab_offload_rig_component_dependencies(
        runner_id,
        &changed_since_preflight.args,
        &synced.local_path,
        &remote_cwd,
    )?;
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
    let path_remaps: Vec<LabPathRemap> = workspace_mapping
        .iter()
        .map(|entry| LabPathRemap {
            local: entry.local_path().to_string(),
            remote: entry.remote_path().to_string(),
        })
        .collect();
    preflight_provider_config_source_cli_dependencies(
        &changed_since_preflight.args,
        &synced.excludes,
    )?;
    let remapped_args = rig_materialization::remap_bench_rig_default_component_to_primary_snapshot(
        &changed_since_preflight.args,
        &remote_cwd,
    );
    let remapped_args = remap_provider_config_in_args(&remapped_args, &path_remaps);
    let remapped_args =
        remap_agent_task_plan_in_args(&remapped_args, &path_remaps, Path::new(&synced.local_path))?;
    let remapped_args = remap_path_settings_in_args(&remapped_args, &path_remaps);
    let (remapped_args, synced_remapped_tasks) =
        materialize_inline_agent_task_tasks_arg(runner_id, &remapped_args)?;
    if let Some(entry) = synced_remapped_tasks {
        workspace_mapping.push(entry.clone());
        plan = with_step(
            plan,
            PlanStep::ready(
                "lab.sync_remapped_agent_task_tasks",
                "lab.sync_remapped_agent_task_tasks",
            )
            .inputs(PlanValues::new().json("workspace", &entry))
            .build(),
        );
    }
    let (remapped_args, synced_remapped_plan) =
        materialize_inline_agent_task_plan_arg(runner_id, &remapped_args)?;
    if let Some(entry) = synced_remapped_plan {
        workspace_mapping.push(entry.clone());
        plan = with_step(
            plan,
            PlanStep::ready(
                "lab.sync_remapped_agent_task_plan",
                "lab.sync_remapped_agent_task_plan",
            )
            .inputs(PlanValues::new().json("workspace", &entry))
            .build(),
        );
    }
    let (remapped_args, agent_task_run_id) = ensure_agent_task_dispatch_run_id(&remapped_args)
        .map_or((remapped_args, None), |(args, run_id)| (args, Some(run_id)));

    let mut command = command_prefix.argv;
    command.extend(
        rewrite_lab_offload_args(&remapped_args, &remote_cwd, &path_remaps)
            .into_iter()
            .skip(1),
    );
    let remote_command = command.clone();
    plan = with_step(
        plan,
        PlanStep::ready("lab.rewrite_args", "lab.rewrite_args")
            .inputs(PlanValues::new().json("argv", &command))
            .build(),
    );

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        command.join(" "),
        runner_id,
        remote_cwd
    );
    if let Some(run_id) = &agent_task_run_id {
        eprintln!(
            "Lab offload: agent-task run id `{run_id}` will be persisted before provider execution. Inspect with `homeboy agent-task status {run_id}`."
        );
        messages.push(format!(
            "Lab offload: agent-task run id `{run_id}`. Inspect with `homeboy agent-task status {run_id}`."
        ));
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
    let mut env = build_lab_offload_env(&lab_metadata);
    forward_env_if_present(&mut env, PREVIEW_METADATA_ENV);
    forward_env_if_present(&mut env, PREVIEW_PUBLIC_URL_ENV);
    forward_release_ci_env(&mut env);
    let rig_component_path_env = forward_rig_component_path_env(&mut env, &workspace_mapping)?;
    let agent_task_secret_env =
        hydrate_agent_task_secret_env(&changed_since_preflight.args, &mut env)?;
    let trace_secret_env = hydrate_trace_secret_env(&changed_since_preflight.args, &mut env)?;
    lab_metadata["agent_task_secret_env"] = agent_task_secret_env;
    lab_metadata["trace_secret_env"] = trace_secret_env;
    lab_metadata["rig_component_path_env"] = rig_component_path_env;
    lab_metadata["settings_env"] = settings_env_diagnostics(&remapped_args, &env);
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
    env = build_lab_offload_env(&lab_metadata);
    forward_env_if_present(&mut env, PREVIEW_METADATA_ENV);
    forward_env_if_present(&mut env, PREVIEW_PUBLIC_URL_ENV);
    forward_release_ci_env(&mut env);
    forward_rig_component_path_env(&mut env, &workspace_mapping)?;
    hydrate_agent_task_secret_env(&changed_since_preflight.args, &mut env)?;
    hydrate_trace_secret_env(&changed_since_preflight.args, &mut env)?;
    let exec_result = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.clone()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env,
            capture_patch: request.capture_patch,
            raw_exec: false,
            source_snapshot: Some(source_snapshot),
            capability_preflight,
            required_extensions: contract.required_extensions,
            require_paths: Vec::new(),
        },
    );
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
                    return Err(in_flight_daemon_disconnect_error(
                        runner_id, job_id, &reason, &err,
                    ));
                }
                return match selection.source {
                    LabRunnerSelectionSource::Default => {
                        if !request.allow_local_fallback {
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
    if request.capture_patch && exit_code == 0 {
        let apply_output = apply_lab_offload_patch(&exec_output)?;
        plan = with_lab_apply_patch_step(plan, apply_output);
    }
    mirror_agent_task_run_plan_lifecycle(request.normalized_args, &exec_output.stdout)?;

    let mut stderr = String::new();
    for message in messages {
        stderr.push_str(&message);
        stderr.push('\n');
    }
    stderr.push_str(&exec_output.stderr);
    if exit_code != 0 {
        if let Some(run_id) = agent_task_run_id.as_deref() {
            if let Some(envelope) = parse_offloaded_dispatch_envelope_from_outputs(
                &exec_output.stdout,
                &exec_output.stderr,
            )? {
                if let Some(record) = agent_task_lifecycle::record_remote_dispatch_failure(
                    agent_task_lifecycle::AgentTaskRemoteDispatchFailure {
                        run_id: &run_id,
                        local_command: request.normalized_args.to_vec(),
                        remote_command: remote_command.clone(),
                        runner_id,
                        remote_workspace: &remote_cwd,
                        stdout: &exec_output.stdout,
                        stderr: &exec_output.stderr,
                        exit_code,
                    },
                    &envelope,
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
                    });
                }
            }
            let failure_message = lab_pre_dispatch_failure_message(&exec_output.stderr)
                .or_else(|| lab_pre_dispatch_failure_message(&exec_output.stdout))
                .unwrap_or_else(|| format!("offloaded agent-task command exited with {exit_code}"));
            stderr.push_str(&format!("Lab pre-dispatch failure: {failure_message}\n"));
            let record = agent_task_lifecycle::record_pre_dispatch_failure(
                agent_task_lifecycle::AgentTaskPreDispatchFailure {
                    run_id: &run_id,
                    local_command: request.normalized_args.to_vec(),
                    remote_command: remote_command.clone(),
                    runner_id,
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
    })
}

fn with_lab_apply_patch_step(
    plan: HomeboyPlan,
    apply_output: Option<RunnerWorkspaceApplyOutput>,
) -> HomeboyPlan {
    let mut inputs = PlanValues::new();
    if let Some(apply_output) = apply_output {
        inputs = inputs.json("apply", &apply_output);
    } else {
        inputs = inputs.json(
            "apply",
            &serde_json::json!({
                "applied": false,
                "reason": "no_patch",
            }),
        );
    }

    with_step(
        plan,
        PlanStep::builder(
            "lab.apply_patch",
            "lab.apply_patch",
            PlanStepStatus::Success,
        )
        .inputs(inputs)
        .build(),
    )
}

fn in_flight_daemon_disconnect_error(
    runner_id: &str,
    job_id: &str,
    reason: &str,
    err: &Error,
) -> Error {
    let mut disconnected = Error::new(
        ErrorCode::InternalUnexpected,
        format!(
            "Lab offload controller disconnected while runner `{runner_id}` daemon job `{job_id}` was still in flight: {}",
            err.message
        ),
        serde_json::json!({
            "runner_id": runner_id,
            "job_id": job_id,
            "reason": reason,
            "source": err.details,
        }),
    )
    .with_hint(format!(
        "Do not retry until checking the remote run; inspect running records with `homeboy runs list --runner {runner_id} --status running --limit 20`."
    ))
    .with_hint(format!(
        "If daemon polling is unavailable, inspect from the runner with `homeboy runner exec {runner_id} -- homeboy runs list --status running --limit 20`."
    ))
    .with_hint(format!(
        "Runner daemon job id `{job_id}` was already dispatched; tail it with `homeboy runner job logs {runner_id} {job_id} --follow` to decide whether to wait, cancel, or clean up."
    ));
    disconnected.retryable = Some(false);
    disconnected
}

fn automatic_capability_fallback(
    plan: HomeboyPlan,
    runner_id: &str,
    runner_status: &RunnerStatusReport,
    reason: String,
) -> LabOffloadOutcome {
    LabOffloadOutcome::RunLocal {
        metadata: Some(lab_offload_metadata(
            &plan,
            "automatic",
            Some(runner_id),
            Some(status_tunnel_mode(runner_status).metadata_value()),
            "fallback",
            None,
            Some(&reason),
        )),
        plan,
        messages: vec![format!("Lab offload: {reason}; running locally.")],
    }
}

fn automatic_capability_fallback_or_error(
    plan: HomeboyPlan,
    selection: &LabRunnerSelection,
    runner_status: &RunnerStatusReport,
    reason: String,
    remediation: Vec<String>,
    allow_local_fallback: bool,
) -> Result<LabOffloadOutcome> {
    if !allow_local_fallback {
        return Err(selected_runner_fallback_error(
            selection,
            "Lab offload selected a runner that is missing required capability parity",
            &reason,
            remediation,
        ));
    }

    Ok(automatic_capability_fallback(
        plan,
        &selection.runner_id,
        runner_status,
        reason,
    ))
}

fn selected_runner_fallback_error(
    selection: &LabRunnerSelection,
    message: &str,
    reason: &str,
    mut remediation: Vec<String>,
) -> Error {
    remediation.push(
        "Pass --allow-local-fallback only when local execution is intentional and safe for this controller."
            .to_string(),
    );
    remediation.push("Use --force-hot to bypass Lab offload selection entirely.".to_string());

    Error::validation_invalid_argument(
        "runner",
        format!("{message}: {reason}"),
        Some(selection.runner_id.clone()),
        Some(remediation),
    )
}

fn mutation_return_unavailable_outcome(
    plan: HomeboyPlan,
    selection: &LabRunnerSelection,
    runner_status: &RunnerStatusReport,
    reason: String,
) -> Result<LabOffloadOutcome> {
    match selection.source {
        LabRunnerSelectionSource::Default => Ok(automatic_capability_fallback(
            plan,
            &selection.runner_id,
            runner_status,
            reason,
        )),
        LabRunnerSelectionSource::Explicit => Err(Error::validation_invalid_argument(
            "runner",
            reason,
            Some(selection.runner_id.clone()),
            Some(vec![
                "Use --force-hot to run the command locally until reverse Lab mutation return is supported."
                    .to_string(),
            ]),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::lab_capabilities::lab_runner_capability_contract;
    use super::super::super::lab_env::build_lab_offload_env;
    use super::super::super::lab_plan::base_lab_plan;
    use super::super::super::lab_selection::resolve_lab_runner_selection_from_default;
    use super::super::super::lab_workspaces::{
        workspace_mapping_entry, LAB_WORKSPACE_MAPPING_SCHEMA,
    };
    use super::*;
    use crate::core::observation::LAB_OFFLOAD_METADATA_ENV;
    use crate::core::plan::PlanKind;
    use crate::core::runner::{
        RunnerRequiredTool, RunnerSession, RunnerSessionState, RunnerTunnelMode,
        RunnerWorkspaceSyncOutput,
    };

    pub(super) fn portable_lab_command(label: &'static str) -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: label,
            portable: true,
            default_lab_offload: true,
            unsupported_reason: None,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            requires_extension_parity: true,
            required_extensions: Vec::new(),
            requires_playwright: false,
            infer_source_path_tools: true,
        }
    }

    fn local_only_lab_command(reason: &'static str) -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: "rig up",
            portable: false,
            default_lab_offload: false,
            unsupported_reason: Some(reason),
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            requires_extension_parity: false,
            required_extensions: Vec::new(),
            requires_playwright: false,
            infer_source_path_tools: false,
        }
    }

    #[test]
    fn lab_git_workspace_sync_uses_snapshot_for_private_proxied_sources() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .status()
            .expect("init git repo");
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "git@github.a8c.com:Automattic/a8c-intelligence.git",
            ])
            .current_dir(dir.path())
            .status()
            .expect("add origin");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let mode = lab_workspace_sync_mode(LabOffloadWorkspaceModePolicy::Git, &args, dir.path())
            .expect("sync mode");

        assert_eq!(mode, RunnerWorkspaceSyncMode::Snapshot);
    }

    #[test]
    fn required_git_checkout_sync_keeps_git_for_private_sources() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .status()
            .expect("init git repo");
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "git@github.a8c.com:chubes4/conductor.git",
            ])
            .current_dir(dir.path())
            .status()
            .expect("add origin");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "prove it".to_string(),
        ];
        let mode = lab_workspace_sync_mode(
            LabOffloadWorkspaceModePolicy::GitCheckoutRequired,
            &args,
            dir.path(),
        )
        .expect("sync mode");

        assert_eq!(mode, RunnerWorkspaceSyncMode::Git);
    }

    #[test]
    fn lab_git_workspace_sync_keeps_git_for_public_sources() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .status()
            .expect("init git repo");
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy.git",
            ])
            .current_dir(dir.path())
            .status()
            .expect("add origin");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let mode = lab_workspace_sync_mode(LabOffloadWorkspaceModePolicy::Git, &args, dir.path())
            .expect("sync mode");

        assert_eq!(mode, RunnerWorkspaceSyncMode::Git);
    }

    #[test]
    fn in_flight_daemon_disconnect_error_surfaces_inspection_commands() {
        let source = Error::new(
            ErrorCode::InternalUnexpected,
            "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
            serde_json::json!({
                "runner_id": "homeboy-lab",
                "job_id": "job-123",
            }),
        );

        let err = in_flight_daemon_disconnect_error(
            "homeboy-lab",
            "job-123",
            "runner daemon health check failed",
            &source,
        );

        assert_eq!(err.code, ErrorCode::InternalUnexpected);
        assert_eq!(err.retryable, Some(false));
        assert_eq!(err.details["runner_id"], "homeboy-lab");
        assert_eq!(err.details["job_id"], "job-123");
        assert!(err.message.contains("still in flight"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("homeboy runs list --runner homeboy-lab")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy runner exec homeboy-lab")));
    }

    fn reverse_status(runner_id: &str) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: runner_id.to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: runner_id.to_string(),
                mode: RunnerTunnelMode::Reverse,
                role: super::super::super::RunnerSessionRole::Controller,
                server_id: None,
                controller_id: Some("controller".to_string()),
                broker_url: Some("http://127.0.0.1:9876".to_string()),
                remote_daemon_address: None,
                local_port: None,
                local_url: None,
                tunnel_pid: None,
                remote_daemon_pid: None,
                homeboy_version: "homeboy 0.0.0".to_string(),
                homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
                connected_at: "2026-06-03T00:00:00Z".to_string(),
            }),
            stale_daemon: None,
            active_jobs: Vec::new(),
            session_path: "/tmp/lab.json".to_string(),
        }
    }

    #[test]
    fn command_prefix_tools_are_included_in_capability_contract() {
        let dir = tempfile::tempdir().expect("temp dir");
        let contract = lab_runner_capability_contract(
            &portable_lab_command("lint"),
            dir.path(),
            &[RunnerRequiredTool::Cargo],
        )
        .expect("capability contract");

        assert!(contract.required_tools.contains(&RunnerRequiredTool::Cargo));
    }

    #[test]
    fn full_workspace_lab_contract_infers_source_path_tools() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("package signal");
        std::fs::write(dir.path().join("docker-compose.yml"), "services: {}")
            .expect("docker signal");

        let contract = lab_runner_capability_contract(
            &portable_lab_command("test"),
            dir.path(),
            &[RunnerRequiredTool::Homeboy],
        )
        .expect("capability contract");

        assert!(contract
            .required_tools
            .contains(&RunnerRequiredTool::Homeboy));
        assert!(contract.required_tools.contains(&RunnerRequiredTool::Node));
        assert!(contract.required_tools.contains(&RunnerRequiredTool::Npm));
        assert!(contract
            .required_tools
            .contains(&RunnerRequiredTool::Docker));
    }

    #[test]
    fn workload_scoped_lab_contract_ignores_source_path_docker_signal() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("package signal");
        std::fs::write(dir.path().join("docker-compose.yml"), "services: {}")
            .expect("docker signal");
        let mut command = portable_lab_command("trace");
        command.infer_source_path_tools = false;

        let contract =
            lab_runner_capability_contract(&command, dir.path(), &[RunnerRequiredTool::Homeboy])
                .expect("capability contract");

        assert!(contract
            .required_tools
            .contains(&RunnerRequiredTool::Homeboy));
        assert!(!contract.required_tools.contains(&RunnerRequiredTool::Node));
        assert!(!contract.required_tools.contains(&RunnerRequiredTool::Npm));
        assert!(!contract
            .required_tools
            .contains(&RunnerRequiredTool::Docker));
    }

    #[test]
    fn lab_workspace_mapping_metadata_records_local_to_remote_paths() {
        let snapshot = RunnerWorkspaceSyncOutput {
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/Users/chubes/Developer/app".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: "snapshot:abc".to_string(),
            files: 3,
            bytes: 12,
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "snapshot_unique_workspace".to_string(),
        };
        let git = RunnerWorkspaceSyncOutput {
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/Users/chubes/Developer/dep".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Git,
            snapshot_identity: "abc123".to_string(),
            files: 0,
            bytes: 0,
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "clean_remote_required".to_string(),
        };

        let entries = vec![
            workspace_mapping_entry("primary", &snapshot),
            workspace_mapping_entry("dependency", &git),
        ];
        let metadata =
            super::super::super::lab_workspaces::lab_workspace_mapping_metadata(&entries);

        assert_eq!(metadata["schema"], LAB_WORKSPACE_MAPPING_SCHEMA);
        assert_eq!(metadata["workspaces"][0]["role"], "primary");
        assert_eq!(metadata["workspaces"][0]["sync_mode"], "snapshot");
        assert_eq!(metadata["workspaces"][1]["role"], "dependency");
        assert_eq!(metadata["workspaces"][1]["sync_mode"], "git");
        assert_eq!(
            metadata["local_to_remote"]["/Users/chubes/Developer/dep"],
            "/srv/homeboy/_lab_workspaces/dep-def"
        );
    }

    #[test]
    fn lab_offload_env_contains_workspace_mapping_metadata() {
        let mapping = serde_json::json!({
            "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
            "local_to_remote": {
                "/Users/chubes/Developer/app": "/srv/homeboy/_lab_workspaces/app-abc"
            },
            "workspaces": []
        });
        let metadata = serde_json::json!({
            "schema": "homeboy/lab-offload/v1",
            "workspace_mapping": mapping,
        });

        let env = build_lab_offload_env(&metadata);
        let parsed: serde_json::Value = serde_json::from_str(
            env.get(LAB_OFFLOAD_METADATA_ENV)
                .expect("lab offload env metadata"),
        )
        .expect("parse lab offload metadata");

        assert_eq!(parsed["workspace_mapping"], mapping);
    }

    #[test]
    fn lab_runner_selection_keeps_explicit_runner_precedence() {
        let command = portable_lab_command("test");
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
            false,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("explicit runner selected");

        assert_eq!(selection.runner_id, "lab-explicit");
        assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
    }

    #[test]
    fn lab_runner_selection_force_hot_keeps_explicit_runner_precedence() {
        let command = portable_lab_command("test");
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
            true,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("explicit runner selected");

        assert_eq!(selection.runner_id, "lab-explicit");
        assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
    }

    #[test]
    fn lab_runner_selection_uses_default_for_supported_commands() {
        let command = portable_lab_command("test");
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            None,
            false,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("default runner selected");

        assert_eq!(selection.runner_id, "lab-default");
        assert_eq!(selection.source, LabRunnerSelectionSource::Default);
    }

    #[test]
    fn lab_runner_selection_ignores_default_when_auto_offload_is_disabled() {
        let mut command = portable_lab_command("extension update");
        command.default_lab_offload = false;

        let selection = resolve_lab_runner_selection_from_default(
            &command,
            None,
            false,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection");

        assert!(selection.is_none());
    }

    #[test]
    fn lab_runner_selection_honors_explicit_runner_when_auto_offload_is_disabled() {
        let mut command = portable_lab_command("extension update");
        command.default_lab_offload = false;

        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
            false,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("explicit runner selected");

        assert_eq!(selection.runner_id, "lab-explicit");
        assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
    }

    #[test]
    fn lab_runner_selection_runs_locally_without_default_runner() {
        let command = portable_lab_command("test");

        assert!(resolve_lab_runner_selection_from_default(
            &command, None, false, false, false, None
        )
        .expect("selection")
        .is_none());
    }

    #[test]
    fn lab_runner_selection_force_hot_refuses_local_when_default_runner_exists() {
        let command = portable_lab_command("test");

        let err = resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect_err("force-hot should require explicit local override");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("--force-hot would run portable hot command"));
        assert!(err.message.contains("lab-default"));
        let tried = err.details["tried"].as_array().expect("tried");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("--allow-local-hot"))));
    }

    #[test]
    fn lab_runner_selection_force_hot_runs_locally_without_default_runner() {
        let command = portable_lab_command("test");

        assert!(resolve_lab_runner_selection_from_default(
            &command, None, true, false, false, None
        )
        .expect("selection")
        .is_none());
    }

    #[test]
    fn lab_runner_selection_allow_local_hot_overrides_default_runner_gate() {
        let command = portable_lab_command("test");

        assert!(resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            true,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .is_none());
    }

    #[test]
    fn lab_runner_selection_explains_hot_commands_that_stay_local() {
        let err = resolve_lab_runner_selection_from_default(
            &local_only_lab_command("current single-workspace Lab snapshot cannot safely mirror"),
            Some("lab-explicit"),
            false,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect_err("rig up rejects explicit runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("single-workspace Lab snapshot"));
    }

    #[test]
    fn lab_runner_selection_denies_local_bench_when_host_policy_requires_lab() {
        let command = portable_lab_command("bench");

        let err = resolve_lab_runner_selection_from_default(&command, None, true, true, true, None)
            .expect_err("bench local execution should be denied by config policy");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("/bench/local_execution"));
        assert!(err.message.contains("denied"));
        let tried = err.details["tried"].as_array().expect("tried");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("--runner <runner-id>"))));
    }

    #[test]
    fn mutation_return_gap_falls_back_for_default_reverse_runner() {
        let plan = base_lab_plan(Some(&portable_lab_command("audit")));
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: RunnerTunnelMode::Reverse,
        };
        let status = reverse_status("lab");

        let outcome = mutation_return_unavailable_outcome(
            plan,
            &selection,
            &status,
            "Lab offload cannot yet return source-tree mutations from reverse runners".to_string(),
        )
        .expect("default runner falls back");

        let LabOffloadOutcome::RunLocal {
            messages, metadata, ..
        } = outcome
        else {
            panic!("expected local fallback");
        };
        assert!(messages[0].contains("running locally"));
        assert_eq!(metadata.expect("metadata")["status"], "fallback");
    }

    #[test]
    fn mutation_return_gap_rejects_explicit_reverse_runner() {
        let plan = base_lab_plan(Some(&portable_lab_command("audit")));
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::Reverse,
        };
        let status = reverse_status("lab");

        let result = mutation_return_unavailable_outcome(
            plan,
            &selection,
            &status,
            "Lab offload cannot yet return source-tree mutations from reverse runners".to_string(),
        );
        let Err(err) = result else {
            panic!("expected explicit runner rejection");
        };

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("cannot yet return source-tree mutations"));
        assert_eq!(err.details["id"], "lab");
    }

    #[test]
    fn apply_patch_step_accepts_noop_mutation_return() {
        let plan = base_lab_plan(Some(&portable_lab_command("refactor")));

        let plan = with_lab_apply_patch_step(plan, None);

        let step = plan
            .steps
            .iter()
            .find(|step| step.id == "lab.apply_patch")
            .expect("apply patch step");
        assert_eq!(step.status, PlanStepStatus::Success);
        assert_eq!(step.inputs["apply"]["applied"], serde_json::json!(false));
        assert_eq!(
            step.inputs["apply"]["reason"],
            serde_json::json!("no_patch")
        );
    }

    #[test]
    fn default_runner_missing_capabilities_fails_without_local_fallback_opt_in() {
        let plan = base_lab_plan(Some(&portable_lab_command("trace")));
        let selection = LabRunnerSelection {
            runner_id: "homeboy-lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: RunnerTunnelMode::Reverse,
        };
        let status = reverse_status("homeboy-lab");

        let result = automatic_capability_fallback_or_error(
            plan,
            &selection,
            &status,
            "Runner 'homeboy-lab' is missing required capability parity for `trace`: tools: playwright."
                .to_string(),
            vec!["Install Playwright and browser binaries on the runner.".to_string()],
            false,
        );

        let Err(err) = result else {
            panic!("expected selected default runner to fail fast");
        };
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("missing required capability parity"));
        assert!(err.message.contains("playwright"));
        assert_eq!(err.details["id"], "homeboy-lab");
        let tried = err.details["tried"].as_array().expect("tried");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("--allow-local-fallback"))));
    }

    #[test]
    fn default_runner_missing_capabilities_can_fallback_with_explicit_opt_in() {
        let plan = base_lab_plan(Some(&portable_lab_command("trace")));
        let selection = LabRunnerSelection {
            runner_id: "homeboy-lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: RunnerTunnelMode::Reverse,
        };
        let status = reverse_status("homeboy-lab");

        let outcome = automatic_capability_fallback_or_error(
            plan,
            &selection,
            &status,
            "Runner 'homeboy-lab' is missing required capability parity for `trace`: tools: playwright."
                .to_string(),
            Vec::new(),
            true,
        )
        .expect("explicit fallback opt-in should allow local run");

        let LabOffloadOutcome::RunLocal {
            messages, metadata, ..
        } = outcome
        else {
            panic!("expected local fallback");
        };
        assert!(messages[0].contains("running locally"));
        assert_eq!(metadata.expect("metadata")["status"], "fallback");
    }

    #[test]
    fn plan_records_skipped_auto_offload() {
        let outcome = execute_lab_offload(LabOffloadRequest {
            command: Some(portable_lab_command("test")),
            normalized_args: &["homeboy".to_string(), "test".to_string()],
            explicit_runner: None,
            force_hot: true,
            allow_local_hot: true,
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            capture_patch: false,
        })
        .expect("outcome");

        let LabOffloadOutcome::RunLocal { plan, metadata, .. } = outcome else {
            panic!("force-hot should run locally");
        };
        assert_eq!(plan.kind, PlanKind::LabOffload);
        assert_eq!(plan.steps[0].id, "lab.select_runner");
        assert_eq!(plan.steps[0].status, PlanStepStatus::Skipped);
        assert_eq!(metadata.expect("metadata")["status"], "skipped");
    }
}
