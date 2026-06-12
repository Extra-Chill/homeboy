use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::agent_task_lifecycle;
use crate::core::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use crate::core::observation::{PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV};
use crate::core::plan::{HomeboyPlan, PlanStep, PlanStepStatus, PlanValues};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{agent_task_secrets, config, Error, ErrorCode, Result};

use super::{
    evaluate_lab_runner_capabilities_for_runner, exec, lab_offload_changed_since_ref,
    lab_offload_metadata, lab_offload_metadata_with_workspace_mapping, lab_runner_capability_plan,
    load, preflight_lab_offload_changed_since, prepare_git_lab_offload_changed_since,
    rig_materialization, status, sync_workspace, LabRunnerGateDecision, RunnerCapabilityPreflight,
    RunnerExecOptions, RunnerStatusReport, RunnerTunnelMode, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions,
};

use super::daemon_health::runner_daemon_health_failure;
use super::lab_apply::apply_lab_offload_patch;
#[cfg(test)]
use super::lab_args::EXPLICIT_PASSTHROUGH_SENTINEL;
use super::lab_args::{
    lab_offload_source_path, remap_agent_task_plan_in_args, remap_path_settings_in_args,
    remap_provider_config_in_args, rewrite_lab_offload_args, LabPathRemap,
};
use super::lab_capabilities::lab_runner_capability_contract;
use super::lab_command::lab_offload_command_prefix;
use super::lab_env::{
    build_lab_offload_env, forward_env_if_present, forward_release_ci_env,
    forward_rig_component_path_env, misplaced_runner_exec_wait_timeout_warning,
    settings_env_diagnostics,
};
use super::lab_plan::{base_lab_plan, disabled_select_runner_plan, with_step};
pub use super::lab_selection::LabRunnerSelectionSource;
use super::lab_selection::{
    prepare_lab_runner_for_offload, resolve_lab_runner_selection, status_tunnel_mode,
    LabRunnerPreparation, LabRunnerSelection,
};
use super::lab_workspaces::{
    agent_task_plan_extra_workspaces, lab_extra_workspaces, lab_workspace_mapping_metadata,
    path_setting_extra_workspaces, preflight_provider_config_source_cli_dependencies,
    provider_config_extra_workspaces, rig_component_path_env_extra_workspaces,
    sync_extra_lab_workspaces, workspace_mapping_entries_for_git_dependency,
    workspace_mapping_entry,
};

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub allow_local_hot: bool,
    pub allow_local_fallback: bool,
    pub capture_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabOffloadCommand {
    pub hot_label: &'static str,
    pub portable: bool,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishedWorkflowBenchRun {
    run_id: String,
    run_dir: PathBuf,
    summary_path: PathBuf,
    manifest_path: PathBuf,
    passed_count: u64,
    status: Option<String>,
}

fn lab_offload_git_fetch_refs(
    args: &[String],
    source_path: &Path,
    sync_mode: RunnerWorkspaceSyncMode,
) -> Result<Vec<String>> {
    if sync_mode != RunnerWorkspaceSyncMode::Git {
        return Ok(Vec::new());
    }

    let mut refs = Vec::new();
    for target in lab_offload_trace_compare_targets(args) {
        if trace_compare_target_is_local_path(&target) || target.starts_with("origin/") {
            continue;
        }
        let git_ref = if target.starts_with("refs/") {
            Some(target.clone())
        } else {
            advertised_origin_ref_for_local_target(source_path, &target)?
        };
        if let Some(git_ref) = git_ref {
            if !refs.contains(&git_ref) {
                refs.push(git_ref);
            }
        }
    }
    Ok(refs)
}

fn lab_offload_trace_compare_targets(args: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let target = if arg == "--baseline-target" || arg == "--candidate" {
            iter.next().cloned()
        } else {
            arg.strip_prefix("--baseline-target=")
                .or_else(|| arg.strip_prefix("--candidate="))
                .map(str::to_string)
        };
        if let Some(target) = target {
            targets.push(target);
        }
    }
    targets
}

fn trace_compare_target_is_local_path(target: &str) -> bool {
    let expanded = shellexpand::tilde(target).to_string();
    Path::new(&expanded).exists()
}

fn advertised_origin_ref_for_local_target(
    source_path: &Path,
    target: &str,
) -> Result<Option<String>> {
    let commit = match super::workspace::git_output(
        source_path,
        &["rev-parse", "--verify", &format!("{target}^{{commit}}")],
    ) {
        Ok(commit) => commit,
        Err(_) => return Ok(None),
    };
    let output = Command::new("git")
        .args(["ls-remote", "origin"])
        .current_dir(source_path)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("run git ls-remote".to_string()))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "trace_compare_target",
            "Lab offload could not inspect origin refs for trace compare target materialization",
            Some(target.to_string()),
            Some(vec![
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
                "Run with --force-hot to execute trace compare locally while investigating remote ref availability.".to_string(),
            ]),
        ));
    }

    let refs = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (sha, git_ref) = line.split_once('\t')?;
            (sha == commit && !git_ref.ends_with("^{}")).then(|| git_ref.to_string())
        })
        .collect::<Vec<_>>();
    if refs.is_empty() && is_full_hex_sha(target) {
        return Err(Error::validation_invalid_argument(
            "trace_compare_target",
            "Lab offload could not find an advertised origin ref for the trace compare target commit",
            Some(target.to_string()),
            Some(vec![
                "Push the candidate commit to origin or pass an advertised ref such as refs/pull/<id>/head.".to_string(),
                "Run with --force-hot to execute trace compare locally while investigating remote ref availability.".to_string(),
            ]),
        ));
    }

    Ok(best_advertised_ref(refs))
}

fn best_advertised_ref(refs: Vec<String>) -> Option<String> {
    refs.iter()
        .find(|git_ref| git_ref.starts_with("refs/pull/") && git_ref.ends_with("/head"))
        .cloned()
        .or_else(|| {
            refs.iter()
                .find(|git_ref| git_ref.starts_with("refs/heads/"))
                .cloned()
        })
        .or_else(|| {
            refs.iter()
                .find(|git_ref| git_ref.starts_with("refs/tags/"))
                .cloned()
        })
        .or_else(|| refs.into_iter().next())
}

fn is_full_hex_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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

    let remote_url =
        super::workspace::git_output(source_path, &["config", "--get", "remote.origin.url"])?;
    if super::source_materialization::requires_controller_routed_workspace_sync(&remote_url) {
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
    if runner.kind != super::RunnerKind::Ssh {
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
    if let Some(published) = published_workflow_bench_run(request.normalized_args, &source_path) {
        plan = with_step(
            plan,
            PlanStep::builder(
                "lab.workflow_bench_published_guard",
                "lab.workflow_bench_published_guard",
                PlanStepStatus::Success,
            )
            .inputs(
                PlanValues::new()
                    .string("run_id", &published.run_id)
                    .string("run_dir", published.run_dir.to_string_lossy().to_string())
                    .string(
                        "summary_path",
                        published.summary_path.to_string_lossy().to_string(),
                    )
                    .string(
                        "manifest_path",
                        published.manifest_path.to_string_lossy().to_string(),
                    )
                    .json("passed_count", published.passed_count),
            )
            .build(),
        );
        let stdout = serde_json::json!({
            "schema": "homeboy/lab-workflow-bench-published-guard/v1",
            "command": "lab.workflow_bench_published_guard",
            "run_id": published.run_id,
            "status": published.status,
            "result_counts": {
                "passed": published.passed_count,
            },
            "published": {
                "manifest_path": published.manifest_path.to_string_lossy().to_string(),
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
            "Lab offload: Workflow Bench run `{}` already published a passing result at {}; skipping duplicate remote attempt.\n",
            published.run_id,
            published.manifest_path.display()
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
    let capability_plan = capability_contract.clone().map(lab_runner_capability_plan);
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
            git_fetch_refs: lab_offload_git_fetch_refs(
                &changed_since_preflight.args,
                &source_path,
                sync_mode,
            )?,
            snapshot_includes: Vec::new(),
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
                    .string("mode", sync_mode.label()),
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
    let remapped_args = remap_agent_task_plan_in_args(&remapped_args, &path_remaps);
    let remapped_args = remap_path_settings_in_args(&remapped_args, &path_remaps);
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
        if let Some(apply_output) = apply_output {
            plan = with_step(
                plan,
                PlanStep::builder(
                    "lab.apply_patch",
                    "lab.apply_patch",
                    PlanStepStatus::Success,
                )
                .inputs(PlanValues::new().json("apply", &apply_output))
                .build(),
            );
        } else {
            return Err(Error::internal_unexpected(
                "Lab offload command required source-tree mutation return, but the runner returned no patch to apply",
            ));
        }
    }
    mirror_agent_task_run_plan_lifecycle(request.normalized_args, &exec_output.stdout)?;

    let mut stderr = String::new();
    for message in messages {
        stderr.push_str(&message);
        stderr.push('\n');
    }
    stderr.push_str(&exec_output.stderr);
    if exit_code != 0 {
        if let Some(run_id) = agent_task_dispatch_requested_run_id(request.normalized_args) {
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

fn materialize_inline_agent_task_plan_arg(
    runner_id: &str,
    args: &[String],
) -> Result<(
    Vec<String>,
    Option<super::lab_workspaces::LabWorkspaceMappingEntry>,
)> {
    if subcommand_index(args, "agent-task")
        .and_then(|index| {
            args.get(index + 1)
                .filter(|arg| arg.as_str() == "run-plan")
                .map(|_| index + 1)
        })
        .is_none()
    {
        return Ok((args.to_vec(), None));
    }

    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        if arg == "--plan" {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                if let Some((remapped_spec, entry)) = sync_inline_agent_task_plan(runner_id, spec)?
                {
                    out.push(remapped_spec);
                    out.extend(iter.cloned());
                    return Ok((out, Some(entry)));
                }
                out.push(spec.clone());
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--plan=") {
            if let Some((remapped_spec, entry)) = sync_inline_agent_task_plan(runner_id, spec)? {
                out.push(format!("--plan={remapped_spec}"));
                out.extend(iter.cloned());
                return Ok((out, Some(entry)));
            }
        }
        out.push(arg.clone());
    }

    Ok((out, None))
}

fn sync_inline_agent_task_plan(
    runner_id: &str,
    spec: &str,
) -> Result<Option<(String, super::lab_workspaces::LabWorkspaceMappingEntry)>> {
    if spec == "-" || spec.starts_with('@') || !looks_like_inline_json(spec) {
        return Ok(None);
    }
    serde_json::from_str::<serde_json::Value>(spec).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse remapped agent-task plan".to_string()),
        )
    })?;

    let temp = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create remapped agent-task plan workspace".to_string()),
        )
    })?;
    let plan_file = temp.path().join("agent-task-plan.json");
    fs::write(&plan_file, spec).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write remapped agent-task plan".to_string()),
        )
    })?;
    let synced = sync_workspace(
        runner_id,
        RunnerWorkspaceSyncOptions {
            path: temp.path().display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
        },
    )?
    .0;
    let remote_spec = format!(
        "@{}/agent-task-plan.json",
        synced.remote_path.trim_end_matches('/')
    );
    let entry = workspace_mapping_entry("agent_task_plan_remapped", &synced);
    Ok(Some((remote_spec, entry)))
}

fn looks_like_inline_json(spec: &str) -> bool {
    let trimmed = spec.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

fn published_workflow_bench_run(
    args: &[String],
    source_path: &Path,
) -> Option<PublishedWorkflowBenchRun> {
    let run_id = workflow_bench_run_id(args)?;
    let mut candidates = workflow_bench_output_candidates(args, source_path, &run_id);
    candidates.sort();
    candidates.dedup();
    candidates
        .into_iter()
        .find_map(|candidate| published_workflow_bench_run_at(&run_id, candidate))
}

fn workflow_bench_run_id(args: &[String]) -> Option<String> {
    let mut saw_workflow_bench = false;
    let mut run_id = None;
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg.contains("workflow-bench") {
            saw_workflow_bench = true;
        }
        if arg == "--run-id" {
            run_id = iter.next().cloned();
            continue;
        }
        if let Some(value) = arg.strip_prefix("--run-id=") {
            if !value.is_empty() {
                run_id = Some(value.to_string());
            }
        }
    }
    saw_workflow_bench.then_some(run_id).flatten()
}

fn workflow_bench_output_candidates(
    args: &[String],
    source_path: &Path,
    run_id: &str,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for explicit in workflow_bench_explicit_output_paths(args) {
        candidates.push(explicit.clone());
        candidates.push(explicit.join(run_id));
        candidates.push(explicit.join("runs").join(run_id));
    }
    candidates.extend([
        source_path.to_path_buf(),
        source_path.join(run_id),
        source_path.join("runs").join(run_id),
        source_path.join("artifacts").join(run_id),
        source_path.join("workflow-bench").join("runs").join(run_id),
        source_path
            .join(".workflow-bench")
            .join("runs")
            .join(run_id),
        source_path.join("bench-runs").join(run_id),
    ]);
    candidates
}

fn workflow_bench_explicit_output_paths(args: &[String]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--output-dir" | "--output-root" | "--artifact-dir" | "--artifacts-dir" => {
                if let Some(path) = iter.next() {
                    paths.push(PathBuf::from(path));
                }
            }
            _ => {
                for prefix in [
                    "--output-dir=",
                    "--output-root=",
                    "--artifact-dir=",
                    "--artifacts-dir=",
                ] {
                    if let Some(path) = arg.strip_prefix(prefix) {
                        if !path.is_empty() {
                            paths.push(PathBuf::from(path));
                        }
                    }
                }
            }
        }
    }
    paths
}

fn published_workflow_bench_run_at(
    run_id: &str,
    run_dir: PathBuf,
) -> Option<PublishedWorkflowBenchRun> {
    let summary_path = run_dir.join("homeboy-summary.json");
    let manifest_path = run_dir.join("published").join("manifest.json");
    if !summary_path.is_file() || !manifest_path.is_file() {
        return None;
    }
    let summary: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&summary_path).ok()?).ok()?;
    if !summary_matches_workflow_bench_run(&summary, run_id) {
        return None;
    }
    let passed_count = summary
        .get("result_counts")
        .and_then(|counts| counts.get("passed"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let status = summary
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    if passed_count == 0
        && !matches!(
            status.as_deref(),
            Some("passed" | "completed" | "success" | "succeeded")
        )
    {
        return None;
    }
    Some(PublishedWorkflowBenchRun {
        run_id: run_id.to_string(),
        run_dir,
        summary_path,
        manifest_path,
        passed_count,
        status,
    })
}

fn summary_matches_workflow_bench_run(summary: &serde_json::Value, run_id: &str) -> bool {
    for key in ["run_id", "id"] {
        if let Some(value) = summary.get(key).and_then(serde_json::Value::as_str) {
            return value == run_id;
        }
    }
    true
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
        "Runner daemon job id `{job_id}` was already dispatched; use runner/job logs to decide whether to wait, cancel, or clean up."
    ));
    disconnected.retryable = Some(false);
    disconnected
}

fn mirror_agent_task_run_plan_lifecycle(args: &[String], stdout: &str) -> Result<()> {
    let Some((plan_spec, run_id)) = agent_task_run_plan_recording_args(args) else {
        return Ok(());
    };
    if plan_spec == "-" {
        return Ok(());
    }
    let envelope = parse_offloaded_run_plan_envelope(stdout)?;
    if !is_agent_task_run_plan_envelope(&envelope) {
        return Ok(());
    }
    let Some(aggregate_value) = envelope.get("data").cloned() else {
        return Ok(());
    };
    let raw_plan = config::read_json_spec_to_string(&plan_spec)?;
    let plan: AgentTaskPlan = serde_json::from_str(&raw_plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("read agent-task plan {plan_spec}")),
        )
    })?;
    let aggregate: AgentTaskAggregate =
        serde_json::from_value(aggregate_value).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse offloaded agent-task aggregate".to_string()),
            )
        })?;

    agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
    agent_task_lifecycle::mark_running(&run_id)?;
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    Ok(())
}

fn parse_offloaded_run_plan_envelope(stdout: &str) -> Result<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        return Ok(value);
    }

    let mut first_json = None;
    for (index, _) in stdout.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if is_agent_task_run_plan_envelope(&value) {
                return Ok(value);
            }
            if first_json.is_none() {
                first_json = Some(value);
            }
        }
    }
    if let Some(value) = first_json {
        return Ok(value);
    }

    serde_json::from_str(stdout).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse offloaded agent-task run-plan output".to_string()),
        )
    })
}

fn is_agent_task_run_plan_envelope(value: &serde_json::Value) -> bool {
    value
        .get("data")
        .and_then(|data| data.get("schema"))
        .and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-aggregate/v1")
        || value
            .get("data")
            .and_then(|data| data.get("plan_id"))
            .is_some()
}

fn parse_offloaded_dispatch_envelope(stdout: &str) -> Result<Option<serde_json::Value>> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        return Ok(agent_task_dispatch_envelope_value(&value).cloned());
    }

    for (index, _) in stdout.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if let Some(envelope) = agent_task_dispatch_envelope_value(&value) {
                return Ok(Some(envelope.clone()));
            }
        }
    }

    Ok(None)
}

fn parse_offloaded_dispatch_envelope_from_outputs(
    stdout: &str,
    stderr: &str,
) -> Result<Option<serde_json::Value>> {
    parse_offloaded_dispatch_envelope(stdout).and_then(|parsed| match parsed {
        Some(envelope) => Ok(Some(envelope)),
        None => parse_offloaded_dispatch_envelope(stderr),
    })
}

fn agent_task_dispatch_envelope_value(value: &serde_json::Value) -> Option<&serde_json::Value> {
    if value.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-dispatch/v1")
    {
        return Some(value);
    }
    let data = value.get("data")?;
    (data.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-dispatch/v1"))
    .then_some(data)
}

fn agent_task_run_plan_recording_args(args: &[String]) -> Option<(String, String)> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut plan = None;
    let mut record_run_id = None;
    let mut iter = args.iter().skip(run_plan_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--plan" => plan = iter.next().cloned(),
            "--record-run-id" => record_run_id = iter.next().cloned(),
            _ => {
                if let Some(value) = arg.strip_prefix("--plan=") {
                    plan = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--record-run-id=") {
                    record_run_id = Some(value.to_string());
                }
            }
        }
    }

    Some((plan?, record_run_id?))
}

fn agent_task_dispatch_requested_run_id(args: &[String]) -> Option<String> {
    let action_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| matches!(arg.as_str(), "dispatch" | "cook"))
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(action_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--run-id" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--run-id=") {
            return (!value.is_empty()).then(|| value.to_string());
        }
    }

    None
}

fn lab_pre_dispatch_failure_message(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn hydrate_agent_task_secret_env(
    args: &[String],
    env: &mut std::collections::HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_agent_task_secret_env(args);
    if names.is_empty() {
        return Ok(serde_json::json!({
            "schema": "homeboy/lab-agent-task-secret-env/v1",
            "secret_env": [],
        }));
    }

    let resolved = agent_task_secrets::resolve_secret_env(&names).map_err(|error| {
        Error::validation_invalid_argument(
            "secret-env",
            error.message,
            None,
            Some(vec![
                "Configure provider secrets with Homeboy's global agent-task secret config, for example `homeboy agent-task auth map-env` or `homeboy agent-task auth set-keychain`.".to_string(),
            ]),
        )
    })?;
    for (name, value) in resolved {
        env.insert(name, value);
    }

    Ok(serde_json::json!({
        "schema": "homeboy/lab-agent-task-secret-env/v1",
        "secret_env": agent_task_secrets::secret_env_status(&names),
    }))
}

fn hydrate_trace_secret_env(
    args: &[String],
    env: &mut std::collections::HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_trace_secret_env(args);
    if names.is_empty() {
        return Ok(crate::core::trace_secrets::empty_status());
    }

    let project_id = trace_project_id_from_args(args);
    let (resolved, statuses) =
        crate::core::trace_secrets::resolve_secret_env(&names, project_id.as_deref())?;
    for (name, value) in resolved {
        env.insert(name, value);
    }

    Ok(crate::core::trace_secrets::status_metadata(statuses))
}

fn declared_agent_task_secret_env(args: &[String]) -> Vec<String> {
    if subcommand_index(args, "agent-task").is_none() {
        return Vec::new();
    }

    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--secret-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--secret-env=") {
            names.push(name.to_string());
        }
        if arg == "--provider-config" {
            if let Some(spec) = args.get(index + 1) {
                names.extend(declared_provider_config_secret_env(spec));
            }
            index += 2;
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--provider-config=") {
            names.extend(declared_provider_config_secret_env(spec));
        }
        index += 1;
    }
    names.extend(declared_agent_task_run_plan_secret_env(args));
    names.sort();
    names.dedup();
    names
}

fn declared_trace_secret_env(args: &[String]) -> Vec<String> {
    if subcommand_index(args, "trace").is_none() {
        return Vec::new();
    }

    declared_secret_env_args(args)
}

fn trace_project_id_from_args(args: &[String]) -> Option<String> {
    let trace_index = subcommand_index(args, "trace")?;

    for index in (trace_index + 1)..args.len() {
        let arg = &args[index];
        if let Some(component) = arg.strip_prefix("--component=") {
            return non_empty_arg(component);
        }
        if arg == "--component" {
            return args.get(index + 1).and_then(|value| non_empty_arg(value));
        }
    }

    match args.get(trace_index + 1).map(String::as_str) {
        Some("compare") => args
            .get(trace_index + 2)
            .and_then(|value| non_empty_arg(value)),
        Some(command) if !command.starts_with('-') => Some(command.to_string()),
        _ => None,
    }
}

fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}

fn non_empty_arg(value: &str) -> Option<String> {
    (!value.trim().is_empty() && !value.starts_with('-')).then(|| value.to_string())
}

fn declared_secret_env_args(args: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--secret-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--secret-env=") {
            names.push(name.to_string());
        }
        index += 1;
    }
    names.sort();
    names.dedup();
    names
}

fn declared_provider_config_secret_env(spec: &str) -> Vec<String> {
    let raw = match config::read_json_spec_to_string(spec) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let Some(config) = value.as_object() else {
        return Vec::new();
    };

    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(serde_json::Value::Array(items)) => {
                names.extend(
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string),
                );
            }
            Some(serde_json::Value::String(name)) => names.push(name.clone()),
            _ => {}
        }
    }
    names
}

fn declared_agent_task_run_plan_secret_env(args: &[String]) -> Vec<String> {
    let Some(plan_spec) = agent_task_run_plan_plan_spec(args) else {
        return Vec::new();
    };
    if plan_spec == "-" {
        return Vec::new();
    }
    let raw = match config::read_json_spec_to_string(&plan_spec) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let plan: AgentTaskPlan = match serde_json::from_str(&raw) {
        Ok(plan) => plan,
        Err(_) => return Vec::new(),
    };

    let mut names = Vec::new();
    for request in plan.tasks {
        names.extend(request.executor.secret_env);
        names.extend(
            request
                .executor
                .config
                .get("secret_env")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string),
        );
    }
    names
}

fn agent_task_run_plan_plan_spec(args: &[String]) -> Option<String> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(run_plan_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--plan" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--plan=") {
            return Some(value.to_string());
        }
    }
    None
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
#[path = "lab_arg_tests.rs"]
mod lab_arg_tests;

#[cfg(test)]
mod preparation_tests;

#[cfg(test)]
mod tests {
    use super::super::lab_selection::resolve_lab_runner_selection_from_default;
    use super::super::lab_workspaces::LAB_WORKSPACE_MAPPING_SCHEMA;
    use super::*;
    use crate::core::observation::LAB_OFFLOAD_METADATA_ENV;
    use crate::core::plan::PlanKind;
    use crate::core::runner::{
        RunnerRequiredTool, RunnerSession, RunnerSessionState, RunnerTunnelMode,
        RunnerWorkspaceSyncOutput,
    };

    fn portable_lab_command(label: &'static str) -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: label,
            portable: true,
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
            unsupported_reason: Some(reason),
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            requires_extension_parity: false,
            required_extensions: Vec::new(),
            requires_playwright: false,
            infer_source_path_tools: false,
        }
    }

    #[test]
    fn trace_compare_targets_are_extracted_before_passthrough_args() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "--baseline-target".to_string(),
            "origin/develop".to_string(),
            "--candidate=32f68bb07ac0efa1d754f78e2adc8de115ddca6f".to_string(),
            "--".to_string(),
            "--candidate".to_string(),
            "ignored".to_string(),
        ];

        assert_eq!(
            lab_offload_trace_compare_targets(&args),
            vec![
                "origin/develop".to_string(),
                "32f68bb07ac0efa1d754f78e2adc8de115ddca6f".to_string(),
            ]
        );
    }

    #[test]
    fn compare_target_ref_selection_prefers_pull_head_refs() {
        let selected = best_advertised_ref(vec![
            "refs/heads/fix-branch".to_string(),
            "refs/pull/5530/head".to_string(),
            "refs/tags/v1".to_string(),
        ]);

        assert_eq!(selected, Some("refs/pull/5530/head".to_string()));
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

    #[test]
    fn workflow_bench_published_guard_detects_passed_published_run() {
        let source = tempfile::tempdir().expect("source tempdir");
        let run_dir = source.path().join("workflow-bench/runs/studio-web-r10");
        std::fs::create_dir_all(run_dir.join("published")).expect("mkdir published");
        std::fs::write(
            run_dir.join("homeboy-summary.json"),
            serde_json::json!({
                "run_id": "studio-web-r10",
                "status": "completed",
                "result_counts": { "passed": 1 }
            })
            .to_string(),
        )
        .expect("write summary");
        std::fs::write(run_dir.join("published/manifest.json"), "{}").expect("write manifest");

        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "studio-web".to_string(),
            "--".to_string(),
            "scripts/workflow-bench.mjs".to_string(),
            "run".to_string(),
            "--run-id".to_string(),
            "studio-web-r10".to_string(),
        ];

        let published = published_workflow_bench_run(&args, source.path())
            .expect("published passing run should be terminal");

        assert_eq!(published.run_id, "studio-web-r10");
        assert_eq!(published.passed_count, 1);
        assert_eq!(
            published.manifest_path,
            run_dir.join("published/manifest.json")
        );
    }

    #[test]
    fn workflow_bench_published_guard_ignores_unpublished_or_failed_runs() {
        let source = tempfile::tempdir().expect("source tempdir");
        let run_dir = source.path().join("workflow-bench/runs/studio-web-r11");
        std::fs::create_dir_all(&run_dir).expect("mkdir run");
        std::fs::write(
            run_dir.join("homeboy-summary.json"),
            serde_json::json!({
                "run_id": "studio-web-r11",
                "status": "failed",
                "result_counts": { "passed": 0 }
            })
            .to_string(),
        )
        .expect("write summary");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "studio-web".to_string(),
            "--".to_string(),
            "scripts/workflow-bench.mjs".to_string(),
            "run".to_string(),
            "--run-id=studio-web-r11".to_string(),
        ];

        assert!(published_workflow_bench_run(&args, source.path()).is_none());

        std::fs::create_dir_all(run_dir.join("published")).expect("mkdir published");
        std::fs::write(run_dir.join("published/manifest.json"), "{}").expect("write manifest");
        assert!(published_workflow_bench_run(&args, source.path()).is_none());
    }

    fn reverse_status(runner_id: &str) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: runner_id.to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: runner_id.to_string(),
                mode: RunnerTunnelMode::Reverse,
                role: super::super::RunnerSessionRole::Controller,
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
        };

        let entries = vec![
            workspace_mapping_entry("primary", &snapshot),
            workspace_mapping_entry("dependency", &git),
        ];
        let metadata = lab_workspace_mapping_metadata(&entries);

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
    fn offloaded_run_plan_envelope_parser_tolerates_extension_stdout_chatter() {
        let stdout = concat!(
            "Setting up WordPress extension...\n",
            "Installing npm dependencies...\n",
            "{\"success\":false,\"data\":{\"status\":\"failed\"}}\n",
            "trailing diagnostic\n"
        );

        let parsed = parse_offloaded_run_plan_envelope(stdout).expect("parse mixed stdout");

        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["data"]["status"], "failed");
    }

    #[test]
    fn offloaded_run_plan_envelope_parser_selects_aggregate_from_mixed_json() {
        let stdout = concat!(
            "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}\n",
            "setup complete\n",
            "{\"success\":true,\"data\":{\"schema\":\"homeboy/agent-task-aggregate/v1\",\"plan_id\":\"plan-1\",\"status\":\"succeeded\",\"totals\":{\"succeeded\":6}}}\n"
        );

        let parsed = parse_offloaded_run_plan_envelope(stdout).expect("parse aggregate envelope");

        assert_eq!(parsed["data"]["plan_id"], "plan-1");
        assert_eq!(parsed["data"]["totals"]["succeeded"], 6);
    }

    #[test]
    fn offloaded_dispatch_envelope_parser_selects_structured_failure_from_mixed_stdout() {
        let stdout = concat!(
            "remote setup complete\n",
            "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}\n",
            "{\"success\":false,\"data\":{\"schema\":\"homeboy/agent-task-dispatch/v1\",\"run_id\":\"run-1\",\"state\":\"failed\",\"record\":{},\"aggregate\":{\"status\":\"failed\"}}}\n"
        );

        let parsed = parse_offloaded_dispatch_envelope(stdout)
            .expect("parse dispatch stdout")
            .expect("dispatch envelope found");

        assert_eq!(parsed["run_id"], "run-1");
        assert_eq!(parsed["aggregate"]["status"], "failed");
    }

    #[test]
    fn offloaded_dispatch_envelope_parser_selects_structured_failure_from_stderr() {
        let stdout = "remote setup complete\n";
        let stderr = concat!(
            "{\n",
            "  \"success\": false,\n",
            "  \"data\": {\n",
            "    \"schema\": \"homeboy/agent-task-dispatch/v1\",\n",
            "    \"run_id\": \"conductor-full-loop-proof-retry3-20260612\",\n",
            "    \"state\": \"failed\",\n",
            "    \"aggregate\": {\n",
            "      \"status\": \"failed\",\n",
            "      \"outcomes\": [{\n",
            "        \"task_id\": \"cook-conductor\",\n",
            "        \"status\": \"failed\",\n",
            "        \"summary\": \"WP Codebox agent task failed.\",\n",
            "        \"metadata\": {\n",
            "          \"provider\": \"wordpress.codebox-agent-task-executor\",\n",
            "          \"codebox_run_result\": {\n",
            "            \"schema\": \"wp-codebox/agent-task-run-result/v1\",\n",
            "            \"status\": \"failed\",\n",
            "            \"failure_classification\": \"runtime\"\n",
            "          }\n",
            "        }\n",
            "      }]\n",
            "    }\n",
            "  }\n",
            "}\n"
        );

        let parsed = parse_offloaded_dispatch_envelope_from_outputs(stdout, stderr)
            .expect("parse dispatch outputs")
            .expect("dispatch envelope found");

        assert_eq!(
            parsed["run_id"],
            "conductor-full-loop-proof-retry3-20260612"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["task_id"],
            "cook-conductor"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["metadata"]["provider"],
            "wordpress.codebox-agent-task-executor"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["metadata"]["codebox_run_result"]
                ["failure_classification"],
            "runtime"
        );
    }

    #[test]
    fn non_aggregate_offloaded_run_plan_stdout_is_not_mirrored() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@/tmp/plan.json".to_string(),
            "--record-run-id".to_string(),
            "run-1".to_string(),
        ];
        let stdout = "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}";

        mirror_agent_task_run_plan_lifecycle(&args, stdout).expect("ignore non-aggregate output");
    }

    #[test]
    fn agent_task_dispatch_requested_run_id_accepts_cook_and_dispatch() {
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--run-id".to_string(),
                "cook-run".to_string(),
            ]),
            Some("cook-run".to_string())
        );
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--run-id=dispatch-run".to_string(),
            ]),
            Some("dispatch-run".to_string())
        );
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "status".to_string(),
                "dispatch-run".to_string(),
            ]),
            None
        );
    }

    #[test]
    fn agent_task_dispatch_requested_run_id_allows_global_flags_before_agent_task() {
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--run-id=dispatch-run".to_string(),
            ]),
            Some("dispatch-run".to_string())
        );
    }

    #[test]
    fn declared_agent_task_secret_env_parses_repeated_and_equals_args() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_TEST_REFRESH_TOKEN".to_string(),
            "--secret-env=HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
                "HOMEBOY_TEST_REFRESH_TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_allows_global_flags_before_agent_task() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--secret-env=HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
        ]);

        assert_eq!(names, vec!["HOMEBOY_TEST_ACCESS_TOKEN".to_string()]);
    }

    #[test]
    fn declared_agent_task_secret_env_ignores_trace_secret_env() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
        ]);

        assert!(names.is_empty());
    }

    #[test]
    fn declared_trace_secret_env_parses_repeated_and_equals_args() {
        let names = declared_trace_secret_env(&[
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env".to_string(),
            "STRIPE_PUBLISHABLE_KEY".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
            "--secret-env".to_string(),
            "STRIPE_SECRET_KEY".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "STRIPE_PUBLISHABLE_KEY".to_string(),
                "STRIPE_SECRET_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn declared_trace_secret_env_allows_global_flags_before_trace() {
        let names = declared_trace_secret_env(&[
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--force-hot".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
        ]);

        assert_eq!(names, vec!["STRIPE_SECRET_KEY".to_string()]);
    }

    #[test]
    fn trace_project_id_from_args_reads_compare_and_component_forms() {
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "--force-hot".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "trace".to_string(),
                "compare-variant".to_string(),
                "--component".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "--scenario".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
    }

    #[test]
    fn hydrate_trace_secret_env_reports_redacted_status_without_values() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=HOMEBOY_TRACE_SECRET_TEST_KEY".to_string(),
        ];
        let mut env = std::collections::HashMap::new();
        std::env::set_var("HOMEBOY_TRACE_SECRET_TEST_KEY", "sk_test_fake_not_real");

        let diagnostics = hydrate_trace_secret_env(&args, &mut env).expect("hydrate trace secret");

        assert_eq!(
            env.get("HOMEBOY_TRACE_SECRET_TEST_KEY").map(String::as_str),
            Some("sk_test_fake_not_real")
        );
        let rendered = diagnostics.to_string();
        assert!(rendered.contains("HOMEBOY_TRACE_SECRET_TEST_KEY"));
        assert!(rendered.contains("env"));
        assert!(!rendered.contains("sk_test_fake_not_real"));

        std::env::remove_var("HOMEBOY_TRACE_SECRET_TEST_KEY");
    }

    #[test]
    fn declared_agent_task_secret_env_includes_provider_config_secrets() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({
                "provider": "example",
                "secret_env": [
                    "HOMEBOY_PROVIDER_ACCESS_TOKEN",
                    "HOMEBOY_PROVIDER_REFRESH_TOKEN"
                ],
                "secretEnv": "HOMEBOY_PROVIDER_ACCOUNT_ID"
            })
            .to_string(),
            "--secret-env=OPENAI_API_KEY".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "HOMEBOY_PROVIDER_ACCESS_TOKEN".to_string(),
                "HOMEBOY_PROVIDER_ACCOUNT_ID".to_string(),
                "HOMEBOY_PROVIDER_REFRESH_TOKEN".to_string(),
                "OPENAI_API_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_includes_run_plan_task_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "codebox",
                            "secret_env": ["OPENAI_API_KEY"],
                            "config": {
                                "secret_env": ["GITHUB_TOKEN"]
                            }
                        },
                        "instructions": "Generate an idea."
                    },
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "design",
                        "executor": {
                            "backend": "codebox",
                            "secret_env": ["OPENAI_API_KEY"]
                        },
                        "instructions": "Design the idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
            "--record-run-id".to_string(),
            "site-generation-loop".to_string(),
        ]);

        assert_eq!(
            names,
            vec!["GITHUB_TOKEN".to_string(), "OPENAI_API_KEY".to_string()]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_dedupes_cli_and_run_plan_task_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "codebox",
                            "secret_env": ["GITHUB_TOKEN"]
                        },
                        "instructions": "Generate an idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--secret-env".to_string(),
            "GITHUB_TOKEN".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
            "--record-run-id".to_string(),
            "site-generation-loop".to_string(),
        ]);

        assert_eq!(names, vec!["GITHUB_TOKEN".to_string()]);
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
