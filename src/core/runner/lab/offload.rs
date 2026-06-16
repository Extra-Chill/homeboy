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

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::agent_task_lifecycle;
use crate::core::agent_tasks::provider::{
    AgentTaskExecutorProvider, ExtensionProviderAgentTaskExecutor,
};
use crate::core::engine::shell;
use crate::core::observation::{PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV};
use crate::core::plan::{HomeboyPlan, PlanStep, PlanStepStatus, PlanValues};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, ErrorCode, Result};

use super::super::daemon_health::runner_daemon_health_failure;
use super::super::execution::{lab_offload_handoff_hints, DaemonJobHandoffState};
use super::super::lab_apply::apply_lab_offload_patch;
use super::super::lab_args::{
    inline_agent_task_prompt_files_in_args, lab_offload_source_path, remap_agent_task_plan_in_args,
    remap_path_settings_in_args, remap_provider_config_in_args, rewrite_lab_offload_args,
    rewrite_runner_resident_lab_offload_args, LabPathRemap,
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
use super::secrets::{
    hydrate_agent_task_secret_env, hydrate_trace_secret_env, hydrate_tunnel_secret_env,
};
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
    /// Human-readable flag (e.g. `--write`, `--fix`) that requested the
    /// source-tree mutation. Used to render actionable diagnostics when the
    /// remote runner finishes cleanly but returns no patch to apply.
    pub mutation_flag: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabOffloadCommand {
    pub hot_label: &'static str,
    pub portable: bool,
    pub default_lab_offload: bool,
    pub unsupported_reason: Option<&'static str>,
    pub source_path_mode: LabOffloadSourcePathMode,
    pub workspace_mode_policy: LabOffloadWorkspaceModePolicy,
    pub requires_extension_parity: bool,
    pub required_extensions: Vec<String>,
    pub requires_playwright: bool,
    pub infer_source_path_tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabOffloadSourcePathMode {
    CwdOrPathFlag,
    RunnerResident,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabOffloadWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
    RunnerResident,
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
        LabOffloadWorkspaceModePolicy::RunnerResident => RunnerWorkspaceSyncMode::Snapshot,
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

fn preflight_required_git_checkout_workspace(
    policy: LabOffloadWorkspaceModePolicy,
    args: &[String],
) -> Result<()> {
    if policy != LabOffloadWorkspaceModePolicy::GitCheckoutRequired {
        return Ok(());
    }

    let source_path = rig_materialization::lab_offload_rig_component_checkout_root(args)?
        .unwrap_or(lab_offload_source_path(args)?);
    preflight_patch_provider_git_checkout(&source_path)
}

fn preflight_patch_provider_git_checkout(source_path: &Path) -> Result<()> {
    let path = source_path.display().to_string();
    let unsupported = |message: &str, hints: Vec<String>| {
        Error::validation_invalid_argument(
            "cwd",
            message.to_string(),
            Some(path.clone()),
            Some(hints),
        )
    };

    let inside_work_tree =
        super::super::workspace::git_output(source_path, &["rev-parse", "--is-inside-work-tree"])
            .map(|value| value == "true")
            .unwrap_or(false);
    if !inside_work_tree {
        return Err(unsupported(
            "Lab offload for patch-producing agent-task providers requires --cwd to be a git checkout so generated files can be returned as a patch artifact",
            vec![
                "Use a Homeboy/Data Machine Code worktree or another existing git checkout for --cwd.".to_string(),
                "Initialize the target as a git checkout before using Lab offload with a patch-producing provider.".to_string(),
                "Use a provider without a git-checkout materialization requirement only if it has an explicit non-git apply-back artifact contract.".to_string(),
            ],
        ));
    }

    let remote_url =
        super::super::workspace::git_output(source_path, &["config", "--get", "remote.origin.url"])
            .unwrap_or_default();
    if remote_url.trim().is_empty() {
        return Err(unsupported(
            "Lab offload for patch-producing agent-task providers requires --cwd to have remote.origin.url so the runner can materialize a real git checkout",
            vec![
                "Set remote.origin.url on the source checkout before retrying Lab offload.".to_string(),
                "Use a Homeboy/Data Machine Code worktree or another checkout cloned from the canonical remote.".to_string(),
            ],
        ));
    }

    let status = super::super::workspace::git_output(source_path, &["status", "--porcelain=v1"])
        .unwrap_or_default();
    if !status.trim().is_empty() {
        return Err(unsupported(
            "Lab offload for patch-producing agent-task providers requires --cwd to be a clean git checkout before runner-side patch capture",
            vec![
                "Commit or stash local changes before offloading the patch-producing agent task.".to_string(),
                "Run with --force-hot to execute locally while the worktree is dirty.".to_string(),
            ],
        ));
    }

    Ok(())
}

pub fn execute_lab_offload(request: LabOffloadRequest<'_>) -> Result<LabOffloadOutcome> {
    let unsupported_runner_error = |runner_id: &str, message: String| {
        Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            Some(unsupported_runner_hints(runner_id, request.normalized_args)),
        )
    };
    let mut plan = base_lab_plan(request.command.as_ref());
    let Some(contract) = request.command.clone() else {
        if let Some(runner_id) = request.explicit_runner {
            return Err(unsupported_runner_error(
                runner_id,
                "--runner is only supported for commands with portable Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts/review/providers, agent-task auth status, lint, test, audit, bench, trace, refactor source runs, tunnel preview-consumer run, and tunnel service start".to_string(),
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
                || "--runner is only supported for commands with portable Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts/review/providers, agent-task auth status, lint, test, audit, bench, trace, refactor source runs, tunnel preview-consumer run, and tunnel service start".to_string(),
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

    preflight_required_git_checkout_workspace(
        contract.workspace_mode_policy,
        request.normalized_args,
    )?;

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

fn unsupported_runner_hints(runner_id: &str, normalized_args: &[String]) -> Vec<String> {
    let mut hints = vec!["Current Lab offload support: agent-task dispatch/cook/loop/run-plan/status/logs/artifacts/review/providers, agent-task auth status, audit, bench run, full lint, full test, trace, refactor source runs, tunnel preview-consumer run, and tunnel service start.".to_string()];

    if let Some(service_command) = tunnel_service_command(normalized_args) {
        hints.push(format!(
            "`tunnel service {service_command} --runner {runner_id}` is not routed directly; inspect runner-side tunnel state with `homeboy runner exec {runner_id} --ssh --raw -- homeboy tunnel service {service_command} ...` until service inspection supports native --runner routing."
        ));
    }

    hints
}

fn tunnel_service_command(normalized_args: &[String]) -> Option<&str> {
    normalized_args.windows(3).find_map(|window| {
        let [first, second, third] = window else {
            return None;
        };
        if first == "tunnel" && second == "service" {
            match third.as_str() {
                "list" | "show" | "status" | "url" | "expose" | "set" | "remove" => {
                    Some(third.as_str())
                }
                _ => None,
            }
        } else {
            None
        }
    })
}

fn run_runner_resident_lab_offload(
    request: LabOffloadRequest<'_>,
    selection: LabRunnerSelection,
    contract: LabOffloadCommand,
    mut plan: HomeboyPlan,
    mut messages: Vec<String>,
    runner_workspace_root: &str,
    homeboy_path: &str,
    runner_status: &RunnerStatusReport,
) -> Result<LabOffloadOutcome> {
    let runner_id = &selection.runner_id;
    let runner_homeboy = lab_runner_homeboy_metadata(runner_id, homeboy_path, runner_status);
    plan = with_step(
        plan,
        PlanStep::ready("lab.runner_homeboy", "lab.runner_homeboy")
            .inputs(PlanValues::new().json("runner_homeboy", &runner_homeboy))
            .build(),
    );

    let remapped_args = rewrite_runner_resident_lab_offload_args(request.normalized_args);
    let mut command = vec![homeboy_path.to_string()];
    command.extend(remapped_args.iter().skip(1).cloned());
    plan = with_step(
        plan,
        PlanStep::ready("lab.rewrite_args", "lab.rewrite_args")
            .inputs(PlanValues::new().json("argv", &command))
            .build(),
    );

    eprintln!(
        "Lab offload: running runner-resident `{}` on runner `{}` in `{}`.",
        command.join(" "),
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
    let mut env = build_lab_offload_env(&lab_metadata);
    forward_env_if_present(&mut env, PREVIEW_METADATA_ENV);
    forward_env_if_present(&mut env, PREVIEW_PUBLIC_URL_ENV);
    forward_release_ci_env(&mut env);
    let tunnel_secret_env = hydrate_tunnel_secret_env(&remapped_args, &mut env)?;
    lab_metadata["tunnel_secret_env"] = tunnel_secret_env;
    env = build_lab_offload_env(&lab_metadata);
    forward_env_if_present(&mut env, PREVIEW_METADATA_ENV);
    forward_env_if_present(&mut env, PREVIEW_PUBLIC_URL_ENV);
    forward_release_ci_env(&mut env);
    hydrate_tunnel_secret_env(&remapped_args, &mut env)?;

    let (exec_output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(runner_workspace_root.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env,
            secret_env_names: Vec::new(),
            capture_patch: request.capture_patch,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: contract.required_extensions,
            require_paths: Vec::new(),
        },
    )?;
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

    Ok(LabOffloadOutcome::Offloaded {
        plan,
        stdout: exec_output.stdout,
        stderr,
        exit_code,
    })
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

    let runner_workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
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
            runner_workspace_root,
            runner.settings.homeboy_path.as_deref().unwrap_or("homeboy"),
            &runner_status,
        );
    }

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
        command_prefix.argv.join(" "),
        runner_id,
    );
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
    let remapped_args =
        inline_agent_task_prompt_files_in_args(&remapped_args, Path::new(&synced.local_path))?;
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

    let mut command = command_prefix.argv.clone();
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
    let tunnel_secret_env = hydrate_tunnel_secret_env(&changed_since_preflight.args, &mut env)?;
    lab_metadata["agent_task_secret_env"] = agent_task_secret_env;
    lab_metadata["trace_secret_env"] = trace_secret_env;
    lab_metadata["tunnel_secret_env"] = tunnel_secret_env;
    lab_metadata["rig_component_path_env"] = rig_component_path_env;
    lab_metadata["settings_env"] = settings_env_diagnostics(&remapped_args, &env);
    lab_metadata["runner_homeboy"] = runner_homeboy.clone();
    lab_metadata["source_checkout"] = source_checkout.clone();
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
    hydrate_tunnel_secret_env(&changed_since_preflight.args, &mut env)?;
    preflight_agent_task_provider_on_runner(
        runner_id,
        &command_prefix.argv,
        &remote_cwd,
        &remapped_args,
        env.clone(),
        source_snapshot.clone(),
        contract.required_extensions.clone(),
        capability_preflight.clone(),
        &runner_homeboy,
    )?;
    if is_agent_task_offload_command(&remapped_args) {
        preflight_agent_task_provider_registry(
            runner_id,
            &remote_cwd,
            &command_prefix.argv,
            &env,
            &runner_homeboy,
        )?;
    }
    let exec_result = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.clone()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env,
            secret_env_names: Vec::new(),
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
        if apply_output.is_none() {
            return Err(missing_mutation_patch_error(
                request.normalized_args,
                request.mutation_flag,
                &exec_output,
            ));
        }
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
                        run_id,
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
                    run_id,
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

fn lab_runner_homeboy_metadata(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> serde_json::Value {
    let refresh_commands = vec![
        format!("homeboy runner disconnect {}", shell::quote_arg(runner_id)),
        format!("homeboy runner connect {}", shell::quote_arg(runner_id)),
    ];
    serde_json::json!({
        "schema": "homeboy/lab-runner-homeboy/v1",
        "runner_id": runner_id,
        "configured_executable": configured_executable,
        "active_daemon_version": status.session.as_ref().map(|session| session.homeboy_version.clone()),
        "active_daemon_build_identity": status.session.as_ref().and_then(|session| session.homeboy_build_identity.clone()),
        "stale_daemon": status.stale_daemon,
        "refresh_commands": refresh_commands,
        "upgrade_command": format!("homeboy upgrade --force --upgrade-runner {}", shell::quote_arg(runner_id)),
    })
}

fn lab_source_checkout_metadata(source_path: &Path) -> serde_json::Value {
    let git_branch =
        super::super::workspace::git_output(source_path, &["branch", "--show-current"])
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                super::super::workspace::git_output(
                    source_path,
                    &["rev-parse", "--abbrev-ref", "HEAD"],
                )
                .ok()
            });
    let git_sha = super::super::workspace::git_output(source_path, &["rev-parse", "HEAD"])
        .ok()
        .filter(|value| !value.is_empty());
    let git_remote =
        super::super::workspace::git_output(source_path, &["config", "--get", "remote.origin.url"])
            .ok()
            .filter(|value| !value.is_empty());
    let dirty = super::super::workspace::git_output(source_path, &["status", "--porcelain=v1"])
        .ok()
        .map(|status| !status.is_empty());

    serde_json::json!({
        "schema": "homeboy/lab-source-checkout/v1",
        "local_path": source_path.display().to_string(),
        "git_branch": git_branch,
        "git_sha": git_sha,
        "git_remote": git_remote,
        "dirty": dirty,
    })
}

fn source_checkout_ref_display(metadata: &serde_json::Value) -> String {
    let branch = metadata
        .get("git_branch")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let sha = metadata
        .get("git_sha")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(12).collect::<String>());
    let dirty = metadata
        .get("dirty")
        .and_then(|value| value.as_bool())
        .map(|value| if value { " dirty" } else { " clean" })
        .unwrap_or("");

    match (branch, sha) {
        (Some(branch), Some(sha)) => format!("{branch}@{sha}{dirty}"),
        (Some(branch), None) => format!("{branch}{dirty}"),
        (None, Some(sha)) => format!("{sha}{dirty}"),
        (None, None) => format!("unknown ref{dirty}"),
    }
}

fn stale_runner_homeboy_error(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> Error {
    let refresh_commands = runner_homeboy_refresh_commands(runner_id, status);
    let active_daemon = status
        .session
        .as_ref()
        .map(runner_session_homeboy_display)
        .unwrap_or_else(|| "<not connected>".to_string());
    let current_homeboy = status.stale_daemon.as_ref().map_or_else(
        || "configured runner executable".to_string(),
        runner_stale_daemon_current_display,
    );
    let drift_message = status
        .stale_daemon
        .as_ref()
        .map(|warning| warning.message.clone())
        .unwrap_or_else(|| {
            "connected runner daemon was started by a different Homeboy runtime".to_string()
        });
    let refresh = refresh_commands.join(" && ");
    Error::validation_invalid_argument(
        "runner",
        format!(
            "Lab offload refused runner `{runner_id}` because its active daemon Homeboy/runtime differs from the configured runner executable `{configured_executable}`. Active daemon: {active_daemon}; configured runtime: {current_homeboy}. {drift_message} Stale runner runtimes can return malformed or misleading provider output; reconnect the runner before retrying."
        ),
        Some(runner_id.to_string()),
        Some(vec![
            format!("Reconnect runner `{runner_id}` before retrying Lab offload: {refresh}"),
            format!("If the runner binary itself is stale, upgrade it with `homeboy upgrade --force --upgrade-runner {}`.", shell::quote_arg(runner_id)),
            "Use --force-hot --allow-local-hot only if you intentionally want to bypass Lab offload and run locally.".to_string(),
        ]),
    )
}

fn runner_homeboy_refresh_commands(runner_id: &str, status: &RunnerStatusReport) -> Vec<String> {
    let commands = status
        .stale_daemon
        .as_ref()
        .map(|warning| warning.recovery_commands.clone())
        .unwrap_or_default();
    if !commands.is_empty() && !runner_id.contains(char::is_whitespace) {
        return commands;
    }
    vec![
        format!("homeboy runner disconnect {}", shell::quote_arg(runner_id)),
        format!("homeboy runner connect {}", shell::quote_arg(runner_id)),
    ]
}

fn runner_session_homeboy_display(session: &super::super::RunnerSession) -> String {
    session
        .homeboy_build_identity
        .as_deref()
        .unwrap_or(&session.homeboy_version)
        .to_string()
}

fn runner_stale_daemon_current_display(warning: &super::super::RunnerStaleDaemonWarning) -> String {
    warning
        .current_homeboy_build_identity
        .as_deref()
        .unwrap_or(&warning.current_homeboy_version)
        .to_string()
}

fn runner_homeboy_daemon_display(metadata: &serde_json::Value) -> String {
    metadata
        .get("active_daemon_build_identity")
        .and_then(|value| value.as_str())
        .or_else(|| {
            metadata
                .get("active_daemon_version")
                .and_then(|value| value.as_str())
        })
        .unwrap_or("<not connected>")
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTaskProviderSelection {
    backend: String,
    selector: Option<String>,
}

fn preflight_agent_task_provider_on_runner(
    runner_id: &str,
    command_prefix: &[String],
    remote_cwd: &str,
    args: &[String],
    env: std::collections::HashMap<String, String>,
    source_snapshot: SourceSnapshot,
    required_extensions: Vec<String>,
    capability_preflight: Option<RunnerCapabilityPreflight>,
    runner_homeboy: &serde_json::Value,
) -> Result<()> {
    let Some(selection) = agent_task_provider_selection_from_args(args) else {
        return Ok(());
    };

    let mut command = command_prefix.to_vec();
    command.extend(["agent-task".to_string(), "providers".to_string()]);
    let (output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command: command.clone(),
            env,
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: Some(source_snapshot),
            capability_preflight,
            required_extensions,
            require_paths: Vec::new(),
        },
    )?;

    let local_providers = ExtensionProviderAgentTaskExecutor::discover()
        .providers()
        .to_vec();
    let local_available = provider_available(
        &local_providers,
        &selection.backend,
        selection.selector.as_deref(),
    );

    if exit_code != 0 {
        return Err(agent_task_provider_selection_preflight_error(
            runner_id,
            &selection,
            local_available,
            None,
            runner_homeboy,
            Some(format!(
                "runner provider preflight command exited with {exit_code}: {}",
                first_non_empty_line(&output.stderr)
                    .or_else(|| first_non_empty_line(&output.stdout))
                    .unwrap_or_else(|| "no output".to_string())
            )),
            &command,
        ));
    }

    let runner_providers =
        parse_agent_task_providers_output(&output.stdout).map_err(|message| {
            agent_task_provider_selection_preflight_error(
                runner_id,
                &selection,
                local_available,
                None,
                runner_homeboy,
                Some(message),
                &command,
            )
        })?;
    let runner_available = provider_available(
        &runner_providers,
        &selection.backend,
        selection.selector.as_deref(),
    );

    if !runner_available {
        return Err(agent_task_provider_selection_preflight_error(
            runner_id,
            &selection,
            local_available,
            Some(false),
            runner_homeboy,
            None,
            &command,
        ));
    }

    Ok(())
}

fn agent_task_provider_selection_from_args(args: &[String]) -> Option<AgentTaskProviderSelection> {
    let action_index =
        super::args_util::subcommand_index(args, "agent-task").and_then(|index| {
            args.get(index + 1)
                .filter(|arg| matches!(arg.as_str(), "dispatch" | "cook"))
                .map(|_| index + 1)
        })?;

    let mut backend = None;
    let mut selector = None;
    let mut iter = args.iter().skip(action_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--backend" => backend = iter.next().cloned(),
            "--selector" => selector = iter.next().cloned(),
            _ => {
                if let Some(value) = arg.strip_prefix("--backend=") {
                    backend = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--selector=") {
                    selector = Some(value.to_string());
                }
            }
        }
    }

    backend
        .filter(|backend| !backend.trim().is_empty())
        .map(|backend| AgentTaskProviderSelection { backend, selector })
}

fn parse_agent_task_providers_output(
    output: &str,
) -> std::result::Result<Vec<AgentTaskExecutorProvider>, String> {
    let value = parse_json_from_mixed_output(output)
        .ok_or_else(|| "runner provider preflight did not return JSON".to_string())?;
    let providers = value
        .get("providers")
        .or_else(|| value.get("data").and_then(|data| data.get("providers")))
        .cloned()
        .ok_or_else(|| "runner provider preflight JSON did not include providers".to_string())?;

    serde_json::from_value::<Vec<AgentTaskExecutorProvider>>(providers).map_err(|error| {
        format!("runner provider preflight returned invalid providers JSON: {error}")
    })
}

fn parse_json_from_mixed_output(output: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        return Some(value);
    }
    for (index, _) in output.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&output[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            return Some(value);
        }
    }
    None
}

fn provider_available(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    providers.iter().any(|provider| {
        provider.backend == backend && selector.is_none_or(|selector| provider.id == selector)
    })
}

fn agent_task_provider_selection_preflight_error(
    runner_id: &str,
    selection: &AgentTaskProviderSelection,
    local_available: bool,
    runner_available: Option<bool>,
    runner_homeboy: &serde_json::Value,
    reason: Option<String>,
    command: &[String],
) -> Error {
    let selector = selection.selector.as_deref().unwrap_or("<default>");
    let runner_available = runner_available.unwrap_or(false);
    let reason = reason.unwrap_or_else(|| {
        format!(
            "runner provider availability is {runner_available} while local provider availability is {local_available}"
        )
    });
    Error::validation_invalid_argument(
        "backend",
        format!(
            "Lab runner `{runner_id}` cannot execute agent-task backend `{}` selector `{selector}` before dispatch: {reason}. No task cells were queued. This points to extension/runtime sync drift between the controller and selected runner, not a task failure.",
            selection.backend
        ),
        Some(selection.backend.clone()),
        Some(vec![
            format!(
                "Local provider availability: {local_available}; runner provider availability: {runner_available}."
            ),
            format!(
                "Refresh runner `{runner_id}` with `{}`.",
                runner_homeboy["refresh_commands"]
                    .as_array()
                    .map(|commands| commands
                        .iter()
                        .filter_map(|command| command.as_str())
                        .collect::<Vec<_>>()
                        .join(" && "))
                    .filter(|command| !command.is_empty())
                    .unwrap_or_else(|| format!("homeboy runner disconnect {runner_id} && homeboy runner connect {runner_id}"))
            ),
            format!(
                "Upgrade or sync the runner runtime/extensions with `{}`.",
                runner_homeboy["upgrade_command"]
                    .as_str()
                    .unwrap_or("homeboy upgrade --force --upgrade-runner <runner>")
            ),
            format!("Preflight command: `{}`.", command.join(" ")),
        ]),
    )
}

fn first_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
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
            serde_json::json!({
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
    run_id: Option<&str>,
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
            "status": "followup_required",
            "runner_id": runner_id,
            "job_id": job_id,
            "durable_run_id": run_id,
            "reason": reason,
            "source": err.details,
        }),
    );
    for hint in lab_offload_handoff_hints(
        runner_id,
        None,
        job_id,
        None,
        DaemonJobHandoffState::InFlight,
    ) {
        disconnected = disconnected.with_hint(hint);
    }
    disconnected.retryable = Some(false);
    disconnected
}

fn in_flight_daemon_disconnect_outcome(
    plan: HomeboyPlan,
    runner_id: &str,
    job_id: &str,
    run_id: &str,
    reason: &str,
    err: &Error,
) -> LabOffloadOutcome {
    let plan = with_step(
        plan,
        PlanStep::builder("lab.exec.detached", "lab.exec.detached", PlanStepStatus::PartialSuccess)
            .skip_reason(format!(
                "controller disconnected after durable run `{run_id}` dispatched to runner job `{job_id}`"
            ))
            .build(),
    );
    let error = in_flight_daemon_disconnect_error(runner_id, job_id, Some(run_id), reason, err);
    let details = serde_json::json!({
        "status": "dispatched_detached",
        "followup_required": true,
        "durable_run_id": run_id,
        "runner_id": runner_id,
        "job_id": job_id,
        "reason": reason,
        "message": error.message,
        "retrieval_commands": {
            "status": format!("homeboy agent-task status {run_id}"),
            "logs": format!("homeboy agent-task logs {run_id}"),
            "artifacts": format!("homeboy agent-task artifacts {run_id}"),
            "runner_job_logs": format!("homeboy runner job logs {runner_id} {job_id} --follow")
        }
    });
    let stdout = serde_json::to_string_pretty(&serde_json::json!({
        "success": true,
        "data": details,
    }))
    .unwrap_or_else(|_| {
        format!(
            "Lab offload detached after dispatch. Durable run `{run_id}` continues remotely; inspect with `homeboy agent-task status {run_id}`."
        )
    });
    let mut stderr = format!(
        "Lab offload detached after dispatch: durable agent-task run `{run_id}` continues remotely on runner `{runner_id}` daemon job `{job_id}`.\n"
    );
    stderr.push_str(&format!("Reason: {reason}\n"));
    stderr.push_str(&format!("Next: homeboy agent-task status {run_id}\n"));
    stderr.push_str(&format!("Next: homeboy agent-task logs {run_id}\n"));
    stderr.push_str(&format!("Next: homeboy agent-task artifacts {run_id}\n"));
    stderr.push_str(&format!(
        "Runner job: homeboy runner job logs {runner_id} {job_id} --follow\n"
    ));

    LabOffloadOutcome::Offloaded {
        plan,
        stdout: format!("{stdout}\n"),
        stderr,
        exit_code: 0,
    }
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

/// Build an actionable diagnostic when a Lab offload write/fix command
/// finished cleanly but the runner returned no source-tree patch.
fn missing_mutation_patch_error(
    normalized_args: &[String],
    mutation_flag: Option<&str>,
    exec_output: &super::super::RunnerExecOutput,
) -> Error {
    let flag_label = mutation_flag.unwrap_or("write");
    let original_command = normalized_args.join(" ");
    let remote_command = exec_output.argv.join(" ");
    let patch_artifact_id = exec_output
        .patch
        .as_ref()
        .and_then(|patch| patch.get("patch_artifact_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let patch_artifact_path = exec_output
        .patch
        .as_ref()
        .and_then(|patch| patch.get("patch_artifact_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty());
    let mut error = Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Lab offload write command completed on runner `{}` but returned no source-tree patch to apply for `{flag_label}`",
            exec_output.runner_id
        ),
        serde_json::json!({
            "field": "lab_offload_patch",
            "problem": "missing required source-tree mutation patch",
            "runner_id": exec_output.runner_id,
            "job_id": exec_output.job_id,
            "mirror_run_id": exec_output.mirror_run_id,
            "remote_workspace": exec_output.remote_cwd,
            "remote_command": remote_command,
            "original_command": original_command,
            "mutation_flag": mutation_flag,
            "patch_artifact_id": patch_artifact_id,
            "patch_artifact_path": patch_artifact_path,
            "patch": exec_output.patch,
        }),
    );

    if let Some(run_id) = exec_output.mirror_run_id.as_deref() {
        error = error
            .with_hint(format!("Inspect the Lab run with `homeboy runs show {run_id}`."))
            .with_hint(format!(
                "List mirrored Lab artifacts with `homeboy runs artifacts {run_id}` and verify the runner produced a lint/refactor patch artifact."
            ));
    } else if let Some(job_id) = exec_output.job_id.as_deref() {
        error = error.with_hint(format!(
            "Runner daemon job `{job_id}` finished without a patch artifact; inspect runner job evidence before retrying."
        ));
    }

    if !original_command.is_empty() {
        error = error.with_hint(format!(
            "After runner patch capture is available, retry the intended Homeboy write path: `{original_command}`."
        ));
    }

    error
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

fn is_agent_task_offload_command(args: &[String]) -> bool {
    args.windows(2).any(|window| {
        window[0] == "agent-task" && matches!(window[1].as_str(), "cook" | "dispatch" | "run-plan")
    })
}

fn preflight_agent_task_provider_registry(
    runner_id: &str,
    remote_cwd: &str,
    command_prefix: &[String],
    env: &std::collections::HashMap<String, String>,
    runner_homeboy: &serde_json::Value,
) -> Result<()> {
    let local_executor = ExtensionProviderAgentTaskExecutor::discover();
    let local_providers = provider_fingerprints(local_executor.providers());
    let mut command = command_prefix.to_vec();
    command.extend(["agent-task".to_string(), "providers".to_string()]);
    let (output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command: command.clone(),
            env: env.clone(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight: None,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
        },
    )?;
    if exit_code != 0 {
        return Err(agent_task_provider_preflight_error(
            runner_id,
            remote_cwd,
            &command,
            runner_homeboy,
            format!("runner provider registry probe exited with {exit_code}"),
            local_providers,
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            Some(output.stderr),
        ));
    }
    let remote_providers =
        parse_agent_task_provider_fingerprints(&output.stdout).map_err(|err| {
            agent_task_provider_preflight_error(
                runner_id,
                remote_cwd,
                &command,
                runner_homeboy,
                err,
                local_providers.clone(),
                BTreeSet::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                Some(output.stdout.clone()),
            )
        })?;
    if local_providers != remote_providers {
        let missing_on_runner = local_providers
            .difference(&remote_providers)
            .cloned()
            .collect::<BTreeSet<_>>();
        let extra_on_runner = remote_providers
            .difference(&local_providers)
            .cloned()
            .collect::<BTreeSet<_>>();
        return Err(agent_task_provider_preflight_error(
            runner_id,
            remote_cwd,
            &command,
            runner_homeboy,
            "Lab runner agent-task provider registry differs from the controller".to_string(),
            local_providers,
            remote_providers,
            missing_on_runner,
            extra_on_runner,
            None,
        ));
    }
    Ok(())
}

fn parse_agent_task_provider_fingerprints(
    stdout: &str,
) -> std::result::Result<BTreeSet<String>, String> {
    let response: serde_json::Value = serde_json::from_str(stdout)
        .map_err(|err| format!("parse runner agent-task providers response: {err}"))?;
    let providers = response
        .get("data")
        .and_then(|data| data.get("providers"))
        .and_then(|providers| providers.as_array())
        .ok_or_else(|| {
            "runner agent-task providers response did not include data.providers".to_string()
        })?;
    let mut fingerprints = BTreeSet::new();
    for provider in providers {
        let id = provider
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "runner agent-task provider entry is missing id".to_string())?;
        let backend = provider
            .get("backend")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "runner agent-task provider entry is missing backend".to_string())?;
        let command = provider
            .get("command")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let default_backend = provider
            .get("default_backend")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        fingerprints.insert(format!(
            "{id}|backend={backend}|command={command}|default={default_backend}"
        ));
    }
    Ok(fingerprints)
}

fn provider_fingerprints(providers: &[AgentTaskExecutorProvider]) -> BTreeSet<String> {
    providers
        .iter()
        .map(|provider| {
            format!(
                "{}|backend={}|command={}|default={}",
                provider.id, provider.backend, provider.command, provider.default_backend
            )
        })
        .collect()
}

fn agent_task_provider_preflight_error(
    runner_id: &str,
    remote_cwd: &str,
    command: &[String],
    runner_homeboy: &serde_json::Value,
    message: String,
    local_providers: BTreeSet<String>,
    remote_providers: BTreeSet<String>,
    missing_on_runner: BTreeSet<String>,
    extra_on_runner: BTreeSet<String>,
    raw_output: Option<String>,
) -> Error {
    let mut details = serde_json::json!({
        "field": "runner_provider_registry",
        "problem": message,
        "id": runner_id,
        "runner_id": runner_id,
        "remote_workspace": remote_cwd,
        "probe_command": command,
        "runner_homeboy": runner_homeboy,
        "local_providers": local_providers,
        "remote_providers": remote_providers,
        "missing_on_runner": missing_on_runner,
        "extra_on_runner": extra_on_runner,
    });
    if let Some(raw_output) = raw_output {
        details["raw_output"] = serde_json::json!(raw_output);
    }

    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!("Invalid argument 'runner_provider_registry': {message}"),
        details,
    )
    .with_hint(format!(
        "Refresh runner `{runner_id}` so its Homeboy/runtime provider registry matches the controller before retrying Lab agent-task offload."
    ))
    .with_hint(format!(
        "Inspect the runner registry with `homeboy runner exec {} -- {}`.",
        shell::quote_arg(runner_id),
        command
            .iter()
            .map(|arg| shell::quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")
    ))
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
        RunnerExecMode, RunnerExecOutput, RunnerRequiredTool, RunnerSession, RunnerSessionState,
        RunnerStaleDaemonWarning, RunnerTunnelMode, RunnerWorkspaceSyncOutput,
    };

    pub(super) fn portable_lab_command(label: &'static str) -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: label,
            portable: true,
            default_lab_offload: true,
            unsupported_reason: None,
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
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
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
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
    fn required_git_checkout_preflight_rejects_non_git_source_before_offload() {
        let dir = tempfile::tempdir().expect("temp dir");

        let err = preflight_patch_provider_git_checkout(dir.path())
            .expect_err("non-git source should fail");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("requires --cwd to be a git checkout"));
        assert!(err.details["tried"]
            .as_array()
            .expect("tried hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("Data Machine Code worktree"))));
    }

    #[test]
    fn required_git_checkout_preflight_rejects_checkout_without_origin() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .status()
            .expect("init git repo");

        let err = preflight_patch_provider_git_checkout(dir.path())
            .expect_err("checkout without origin should fail");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("remote.origin.url"));
        assert!(err.details["tried"]
            .as_array()
            .expect("tried hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("Set remote.origin.url"))));
    }

    #[test]
    fn required_git_checkout_preflight_rejects_dirty_checkout() {
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
        std::fs::write(dir.path().join("dirty.txt"), "dirty").expect("write dirty file");

        let err = preflight_patch_provider_git_checkout(dir.path())
            .expect_err("dirty checkout should fail");

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("clean git checkout"));
        assert!(err.details["tried"]
            .as_array()
            .expect("tried hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("Commit or stash"))));
    }

    #[test]
    fn required_git_checkout_preflight_accepts_clean_checkout_with_origin() {
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

        preflight_patch_provider_git_checkout(dir.path()).expect("clean checkout should pass");
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
            None,
            "runner daemon health check failed",
            &source,
        );

        assert_eq!(err.code, ErrorCode::InternalUnexpected);
        assert_eq!(err.retryable, Some(false));
        assert_eq!(err.details["runner_id"], "homeboy-lab");
        assert_eq!(err.details["job_id"], "job-123");
        assert_eq!(err.details["status"], "followup_required");
        assert!(err.message.contains("still in flight"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("homeboy runner exec homeboy-lab -- homeboy runs list --status running")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("homeboy runner exec homeboy-lab")));
    }

    #[test]
    fn in_flight_daemon_disconnect_outcome_marks_durable_run_detached() {
        let source = Error::new(
            ErrorCode::InternalUnexpected,
            "query runner daemon: error sending request for url (http://127.0.0.1:63203/jobs/job-123)",
            serde_json::json!({
                "runner_id": "homeboy-lab",
                "job_id": "job-123",
            }),
        );

        let outcome = in_flight_daemon_disconnect_outcome(
            base_lab_plan(Some(&portable_lab_command("agent-task cook"))),
            "homeboy-lab",
            "job-123",
            "run-123",
            "runner daemon health check failed",
            &source,
        );

        let LabOffloadOutcome::Offloaded {
            plan,
            stdout,
            stderr,
            exit_code,
        } = outcome
        else {
            panic!("expected detached offloaded outcome");
        };

        assert_eq!(exit_code, 0);
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");
        assert_eq!(json["success"], serde_json::json!(true));
        assert_eq!(json["data"]["status"], "dispatched_detached");
        assert_eq!(json["data"]["followup_required"], true);
        assert_eq!(json["data"]["durable_run_id"], "run-123");
        assert_eq!(json["data"]["runner_id"], "homeboy-lab");
        assert_eq!(json["data"]["job_id"], "job-123");
        assert_eq!(
            json["data"]["retrieval_commands"]["status"],
            "homeboy agent-task status run-123"
        );
        assert!(stderr.contains("durable agent-task run `run-123` continues remotely"));
        assert!(stderr.contains("homeboy agent-task logs run-123"));
        assert!(plan
            .steps
            .iter()
            .any(|step| step.id == "lab.exec.detached"
                && step.status == PlanStepStatus::PartialSuccess));
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

    fn stale_reverse_status(runner_id: &str) -> RunnerStatusReport {
        let mut status = reverse_status(runner_id);
        status.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            runner_id,
            "homeboy 0.228.0".to_string(),
            "homeboy 0.229.11".to_string(),
            Some("homeboy 0.228.0+old".to_string()),
            Some("homeboy 0.229.11+new".to_string()),
        ));
        status
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
            validation_dependencies: Vec::new(),
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
            validation_dependencies: Vec::new(),
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
    fn agent_task_provider_registry_probe_only_targets_dispatch_commands() {
        assert!(is_agent_task_offload_command(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ]));
        assert!(is_agent_task_offload_command(&[
            "cargo".to_string(),
            "run".to_string(),
            "--".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
        ]));
        assert!(!is_agent_task_offload_command(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "providers".to_string(),
        ]));
    }

    #[test]
    fn parses_agent_task_provider_registry_fingerprints_from_cli_envelope() {
        let stdout = serde_json::json!({
            "success": true,
            "data": {
                "schema": "homeboy/agent-task-providers/v1",
                "providers": [
                    {
                        "id": "claude-code",
                        "backend": "claude-code",
                        "command": "homeboy agent-task provider claude-code",
                        "default_backend": true
                    },
                    {
                        "id": "codebox",
                        "backend": "codebox",
                        "command": "homeboy agent-task provider codebox"
                    }
                ]
            }
        })
        .to_string();

        let fingerprints =
            parse_agent_task_provider_fingerprints(&stdout).expect("provider fingerprints");

        assert!(fingerprints.contains(
            "claude-code|backend=claude-code|command=homeboy agent-task provider claude-code|default=true"
        ));
        assert!(fingerprints.contains(
            "codebox|backend=codebox|command=homeboy agent-task provider codebox|default=false"
        ));
    }

    #[test]
    fn provider_registry_drift_error_reports_missing_and_extra_entries() {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "providers".to_string(),
        ];
        let local =
            BTreeSet::from(["local|backend=local|command=local run|default=true".to_string()]);
        let remote =
            BTreeSet::from(["remote|backend=remote|command=remote run|default=true".to_string()]);
        let missing = local.clone();
        let extra = remote.clone();

        let err = agent_task_provider_preflight_error(
            "lab-1",
            "/srv/homeboy/app",
            &command,
            &serde_json::json!({ "binary": "homeboy" }),
            "Lab runner agent-task provider registry differs from the controller".to_string(),
            local,
            remote,
            missing,
            extra,
            None,
        );

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(err.details["field"], "runner_provider_registry");
        assert_eq!(err.details["runner_id"], "lab-1");
        assert_eq!(
            err.details["missing_on_runner"][0],
            "local|backend=local|command=local run|default=true"
        );
        assert_eq!(
            err.details["extra_on_runner"][0],
            "remote|backend=remote|command=remote run|default=true"
        );
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Refresh runner `lab-1`")));
    }

    #[test]
    fn lab_runner_homeboy_metadata_names_binary_and_refresh_path() {
        let status = reverse_status("homeboy lab");
        let metadata = lab_runner_homeboy_metadata(
            "homeboy lab",
            "/tmp/_lab_workspaces/homeboy/target/debug/homeboy",
            &status,
        );

        assert_eq!(metadata["schema"], "homeboy/lab-runner-homeboy/v1");
        assert_eq!(metadata["runner_id"], "homeboy lab");
        assert_eq!(
            metadata["configured_executable"],
            "/tmp/_lab_workspaces/homeboy/target/debug/homeboy"
        );
        assert_eq!(metadata["active_daemon_version"], "homeboy 0.0.0");
        assert_eq!(
            metadata["active_daemon_build_identity"],
            "homeboy 0.0.0+test"
        );
        assert_eq!(
            metadata["refresh_commands"],
            serde_json::json!([
                "homeboy runner disconnect 'homeboy lab'",
                "homeboy runner connect 'homeboy lab'"
            ])
        );
        assert_eq!(
            metadata["upgrade_command"],
            "homeboy upgrade --force --upgrade-runner 'homeboy lab'"
        );
    }

    #[test]
    fn source_checkout_ref_display_includes_branch_sha_and_dirty_state() {
        let metadata = serde_json::json!({
            "git_branch": "fix/lab-source-ref-preflight",
            "git_sha": "1234567890abcdef",
            "dirty": true,
        });

        assert_eq!(
            source_checkout_ref_display(&metadata),
            "fix/lab-source-ref-preflight@1234567890ab dirty"
        );
    }

    #[test]
    fn source_checkout_ref_display_handles_missing_git_ref() {
        let metadata = serde_json::json!({
            "local_path": "/tmp/source",
            "dirty": null,
        });

        assert_eq!(source_checkout_ref_display(&metadata), "unknown ref");
    }

    #[test]
    fn stale_runner_homeboy_error_blocks_offload_with_reconnect_guidance() {
        let status = stale_reverse_status("homeboy lab");

        let err = stale_runner_homeboy_error(
            "homeboy lab",
            "/home/chubes/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy",
            &status,
        );

        assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(err.details["field"], "runner");
        assert_eq!(err.details["id"], "homeboy lab");
        assert!(err
            .message
            .contains("Lab offload refused runner `homeboy lab`"));
        assert!(err
            .message
            .contains("/home/chubes/Developer/_lab_workspaces/homeboy-post-4583-proof"));
        assert!(err.message.contains("Active daemon: homeboy 0.0.0+test"));
        assert!(err
            .message
            .contains("configured runtime: homeboy 0.229.11+new"));
        assert!(err
            .message
            .contains("malformed or misleading provider output"));
        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried
            .iter()
            .any(|hint| hint.as_str().is_some_and(|hint| hint.contains(
                "homeboy runner disconnect 'homeboy lab' && homeboy runner connect 'homeboy lab'"
            ))));
        assert!(tried.iter().any(|hint| hint.as_str().is_some_and(
            |hint| hint.contains("homeboy upgrade --force --upgrade-runner 'homeboy lab'")
        )));
    }

    #[test]
    fn runner_homeboy_metadata_carries_stale_daemon_details() {
        let status = stale_reverse_status("lab");

        let metadata = lab_runner_homeboy_metadata("lab", "homeboy", &status);

        assert_eq!(
            metadata["stale_daemon"]["session_homeboy_version"],
            "homeboy 0.228.0"
        );
        assert_eq!(
            metadata["stale_daemon"]["current_homeboy_version"],
            "homeboy 0.229.11"
        );
        assert_eq!(
            metadata["stale_daemon"]["session_homeboy_build_identity"],
            "homeboy 0.228.0+old"
        );
        assert_eq!(
            metadata["stale_daemon"]["current_homeboy_build_identity"],
            "homeboy 0.229.11+new"
        );
        assert_eq!(
            metadata["refresh_commands"],
            serde_json::json!([
                "homeboy runner disconnect lab",
                "homeboy runner connect lab"
            ])
        );
    }

    #[test]
    fn runner_homeboy_daemon_display_prefers_build_identity() {
        let metadata = serde_json::json!({
            "active_daemon_version": "homeboy 0.1.0",
            "active_daemon_build_identity": "homeboy 0.1.0+abc123",
        });

        assert_eq!(
            runner_homeboy_daemon_display(&metadata),
            "homeboy 0.1.0+abc123"
        );
    }

    #[test]
    fn agent_task_provider_selection_reads_cook_backend_and_selector() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--backend".to_string(),
            "wordpress".to_string(),
            "--selector=wp-codebox".to_string(),
            "--prompt".to_string(),
            "fix it".to_string(),
        ];

        let selection = agent_task_provider_selection_from_args(&args).expect("selection");

        assert_eq!(selection.backend, "wordpress");
        assert_eq!(selection.selector.as_deref(), Some("wp-codebox"));
    }

    #[test]
    fn agent_task_provider_selection_ignores_non_dispatch_commands() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "providers".to_string(),
            "--backend".to_string(),
            "wordpress".to_string(),
        ];

        assert!(agent_task_provider_selection_from_args(&args).is_none());
    }

    #[test]
    fn runner_provider_output_parser_accepts_cli_envelope_with_chatter() {
        let stdout = concat!(
            "Preparing runtime...\n",
            "{\"success\":true,\"data\":{\"providers\":[{\"schema\":\"homeboy/agent-task-executor-provider/v1\",\"id\":\"wp-codebox\",\"backend\":\"wordpress\",\"default_backend\":true,\"command\":\"wp-codebox agent\",\"request_schema\":\"homeboy/agent-task-request/v1\",\"outcome_schema\":\"homeboy/agent-task-outcome/v1\"}]}}\n"
        );

        let providers = parse_agent_task_providers_output(stdout).expect("providers parse");

        assert!(provider_available(&providers, "wordpress", None));
        assert!(provider_available(
            &providers,
            "wordpress",
            Some("wp-codebox")
        ));
        assert!(!provider_available(
            &providers,
            "wordpress",
            Some("missing")
        ));
    }

    #[test]
    fn provider_preflight_error_reports_local_runner_drift_and_no_cells_queued() {
        let selection = AgentTaskProviderSelection {
            backend: "wordpress".to_string(),
            selector: None,
        };
        let runner_homeboy = serde_json::json!({
            "refresh_commands": [
                "homeboy runner disconnect homeboy-lab",
                "homeboy runner connect homeboy-lab"
            ],
            "upgrade_command": "homeboy upgrade --force --upgrade-runner homeboy-lab"
        });

        let err = agent_task_provider_selection_preflight_error(
            "homeboy-lab",
            &selection,
            true,
            Some(false),
            &runner_homeboy,
            None,
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "providers".to_string(),
            ],
        );

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("homeboy-lab"));
        assert!(err.message.contains("backend `wordpress`"));
        assert!(err.message.contains("No task cells were queued"));
        assert!(err.message.contains("extension/runtime sync drift"));
        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Local provider availability: true"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("runner provider availability: false"))));
        assert!(tried.iter().any(|hint| hint.as_str().is_some_and(
            |hint| hint.contains("homeboy upgrade --force --upgrade-runner homeboy-lab")
        )));
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
    fn lab_runner_selection_rejects_allow_local_hot_without_force_hot() {
        let command = portable_lab_command("rig check");

        let err = resolve_lab_runner_selection_from_default(
            &command,
            None,
            false,
            true,
            false,
            Some("lab-default".to_string()),
        )
        .expect_err("allow-local-hot alone must not silently auto-offload");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("--allow-local-hot only permits"));
        assert!(err.message.contains("--force-hot"));
        assert!(err.message.contains("automatic Lab offload"));
        let tried = err.details["tried"].as_array().expect("tried");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("--force-hot --allow-local-hot"))));
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
    fn missing_mutation_patch_error_points_to_runner_evidence_and_retry() {
        let exec_output = RunnerExecOutput {
            command: "runner.exec",
            runner_id: "lab-default".to_string(),
            mode: RunnerExecMode::Daemon,
            argv: vec![
                "homeboy".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "lint".to_string(),
                "--write".to_string(),
                "data-machine-code".to_string(),
            ],
            remote_cwd: "/srv/homeboy/_lab_workspaces/data-machine-code".to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            job_id: Some("job-123".to_string()),
            job_events: None,
            mirror_run_id: Some("runner-exec-lab-default-job-123".to_string()),
            patch: None,
            metrics: None,
            capture: None,
            diagnostics: None,
        };

        let err = missing_mutation_patch_error(
            &[
                "homeboy".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "lint".to_string(),
                "--write".to_string(),
                "data-machine-code".to_string(),
            ],
            Some("--write"),
            &exec_output,
        );

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("returned no source-tree patch"));
        assert_eq!(err.details["runner_id"], "lab-default");
        assert_eq!(err.details["job_id"], "job-123");
        assert_eq!(
            err.details["mirror_run_id"],
            "runner-exec-lab-default-job-123"
        );
        let hints = err
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>();
        assert!(hints
            .iter()
            .any(|hint| hint.contains("homeboy runs show runner-exec-lab-default-job-123")));
        assert!(hints
            .iter()
            .any(|hint| hint.contains("homeboy runs artifacts runner-exec-lab-default-job-123")));
        assert!(hints
            .iter()
            .any(|hint| hint.contains("homeboy refactor --from lint --write data-machine-code")));
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
            mutation_flag: None,
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

    #[test]
    fn unsupported_runner_error_guides_tunnel_service_inspection() {
        let outcome = execute_lab_offload(LabOffloadRequest {
            command: None,
            normalized_args: &[
                "homeboy".to_string(),
                "tunnel".to_string(),
                "service".to_string(),
                "status".to_string(),
                "wpcom-ai-manual-held".to_string(),
            ],
            explicit_runner: Some("homeboy-lab"),
            force_hot: false,
            allow_local_hot: false,
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            capture_patch: false,
            mutation_flag: None,
        });

        let Err(err) = outcome else {
            panic!("unsupported --runner command should fail");
        };
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("homeboy runner exec homeboy-lab"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("tunnel service status"))));
    }
}
