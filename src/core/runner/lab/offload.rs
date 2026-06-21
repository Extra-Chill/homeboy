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

use crate::command_contract::lab_runner_support_summary;
use crate::core::agent_task_lifecycle;
use crate::core::engine::shell;
use crate::core::plan::{HomeboyPlan, PlanStep, PlanStepStatus, PlanValues};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, ErrorCode, Result};

use super::super::command_path::preflight_remote_argv_path_translation;
use super::super::daemon_health::runner_daemon_health_failure;
use super::super::execution::{lab_offload_handoff_hints, DaemonJobHandoffState};
use super::super::lab_apply::apply_lab_offload_patch;
use super::super::lab_args::{
    inject_agent_task_default_provider_config_in_args, inline_agent_task_prompt_files_in_args,
    lab_offload_source_path, remap_agent_task_plan_in_args, remap_path_settings_in_args,
    remap_provider_config_in_args, rewrite_lab_offload_args,
    rewrite_runner_resident_lab_offload_args, LabPathRemap,
};
use super::super::lab_capabilities::lab_runner_capability_contract;
use super::super::lab_command::lab_offload_command_prefix;
use super::super::lab_env::{
    build_lab_offload_env_with_passthroughs, forward_rig_component_path_env,
    misplaced_runner_exec_wait_timeout_warning, settings_env_diagnostics,
};
use super::super::lab_plan::{base_lab_plan, disabled_select_runner_plan, with_step};
use super::super::lab_selection::{
    prepare_lab_runner_for_offload, release_gate_local_hot_denied_error,
    resolve_lab_runner_selection, status_tunnel_mode, LabRunnerPreparation, LabRunnerSelection,
    LabRunnerSelectionSource,
};
use super::super::lab_workspaces::{
    agent_task_plan_extra_workspaces, lab_extra_workspaces, lab_workspace_mapping_metadata,
    path_setting_extra_workspaces, preflight_provider_config_source_cli_dependencies,
    provider_config_extra_workspaces, rig_component_path_env_extra_workspaces,
    sync_extra_lab_workspaces, workspace_mapping_entries_for_git_dependency,
    workspace_mapping_entry, LabWorkspaceMappingEntry,
};
use super::super::offload_changed_since::LabOffloadChangedSincePreflight;
use super::super::{
    evaluate_lab_runner_capabilities_for_runner, exec, lab_offload_metadata,
    lab_offload_metadata_with_workspace_mapping, load, preflight_lab_offload_changed_since,
    prepare_git_lab_offload_changed_since, prepare_lab_runner_capability, rig_materialization,
    status, sync_workspace, LabRunnerGateDecision, RunnerCapabilityPreflight, RunnerExecOptions,
    RunnerStatusReport, RunnerTunnelMode, RunnerWorkspaceApplyOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};

use super::agent_task_bridge::{
    agent_task_dispatch_run_isolation_token, ensure_agent_task_dispatch_run_id_with,
    lab_pre_dispatch_failure_message, materialize_inline_agent_task_plan_arg,
    materialize_inline_agent_task_tasks_arg, mirror_agent_task_run_plan_lifecycle,
    parse_offloaded_dispatch_envelope_from_outputs,
};
use super::evidence::terminal_lab_run_evidence;
use super::provider_preflight::preflight_agent_task_provider_on_runner;
use super::secrets::{
    hydrate_agent_task_secret_env, hydrate_trace_secret_env, hydrate_tunnel_secret_env,
    preflight_agent_task_runner_secret_env,
};
use super::trace_fetch_refs::lab_offload_git_fetch_refs;
use super::workspace_plan::{lab_workspace_sync_mode, preflight_required_git_checkout_workspace};
#[cfg(test)]
use super::workspace_plan::{
    lab_workspace_sync_mode_with_source_policy, preflight_patch_provider_git_checkout,
};

/// Local-execution escape hatches shared across the Lab routing and offload
/// request layers. Grouping these two flags keeps the policy shape identical
/// wherever a Lab command is allowed to stay local instead of offloading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LabLocalExecutionPolicy {
    /// Permit a `--force-hot` portable command to stay local even when a
    /// default Lab runner exists.
    pub allow_local_hot: bool,
    /// Permit a selected Lab runner to fall back to local execution after
    /// offload preflight fails.
    pub allow_local_fallback: bool,
    /// Fail instead of returning a local execution outcome.
    pub deny_local_execution: bool,
}

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub local_policy: LabLocalExecutionPolicy,
    pub allow_dirty_lab_workspace: bool,
    pub capture_patch: bool,
    /// Human-readable flag (e.g. `--write`, `--fix`) that requested the
    /// source-tree mutation. Used to render actionable diagnostics when the
    /// remote runner finishes cleanly but returns no patch to apply.
    pub mutation_flag: Option<&'a str>,
    pub detach_after_handoff: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabOffloadCommand {
    pub hot_label: &'static str,
    pub portable: bool,
    pub unsupported_reason: Option<&'static str>,
    pub source_path_mode: LabOffloadSourcePathMode,
    pub workspace_mode_policy: LabOffloadWorkspaceModePolicy,
    pub required_extensions: Vec<String>,
    pub requires_playwright: bool,
    /// Routing-policy flags shared across the Lab command layers
    /// (`default_lab_offload`, `infer_source_path_tools`, `release_gate`,
    /// `requires_extension_parity`).
    pub routing_policy: crate::command_contract::LabRoutingPolicy,
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

/// Record a freshly synced, remapped agent-task workspace entry: append it to
/// the workspace mapping and emit the matching Lab plan step. Shared by the
/// inline tasks-arg and plan-arg materialization in `run_lab_offload_inner`.
fn record_synced_remapped_workspace_entry(
    plan: HomeboyPlan,
    workspace_mapping: &mut Vec<super::super::lab_workspaces::LabWorkspaceMappingEntry>,
    entry: Option<super::super::lab_workspaces::LabWorkspaceMappingEntry>,
    step_id: &str,
) -> HomeboyPlan {
    let Some(entry) = entry else {
        return plan;
    };
    workspace_mapping.push(entry.clone());
    with_step(
        plan,
        PlanStep::ready(step_id, step_id)
            .inputs(PlanValues::new().json("workspace", &entry))
            .build(),
    )
}

/// Build the `RunLocal` outcome used whenever automatic Lab offload is skipped
/// for a portability/default-runner reason. Centralizes the repeated
/// "automatic / skipped" metadata shape used by `execute_lab_offload`.
fn skipped_automatic_run_local(plan: HomeboyPlan, reason: &str) -> LabOffloadOutcome {
    LabOffloadOutcome::RunLocal {
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
    }
}

fn local_execution_denied_error(reason: &str, runner_id: Option<&str>) -> Error {
    let mut hints = vec![
        "Use --runner <runner-id> to offload to Lab.".to_string(),
        "Remove --lab-only only when local execution on this controller is intentional."
            .to_string(),
    ];
    if let Some(runner_id) = runner_id {
        hints.insert(
            0,
            format!("Reconnect or repair runner `{runner_id}` before retrying."),
        );
    }
    Error::validation_invalid_argument(
        "lab_only",
        format!("Lab-only execution refused local execution: {reason}"),
        runner_id.map(str::to_string),
        Some(hints),
    )
}

pub fn execute_lab_offload(request: LabOffloadRequest<'_>) -> Result<LabOffloadOutcome> {
    let unsupported_runner_error = |runner_id: &str, message: String| {
        Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            Some(unsupported_runner_hints(
                runner_id,
                request.normalized_args,
                lab_runner_support_summary().hint,
            )),
        )
    };
    let mut plan = base_lab_plan(request.command.as_ref());
    let Some(contract) = request.command.clone() else {
        if let Some(runner_id) = request.explicit_runner {
            return Err(unsupported_runner_error(
                runner_id,
                lab_runner_support_summary().unsupported_message,
            ));
        }
        if request.local_policy.deny_local_execution {
            return Err(local_execution_denied_error(
                "command has no Lab contract",
                None,
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
                || lab_runner_support_summary().unsupported_message,
                |reason| format!("--runner is unavailable for this local-only resource-pressure command. {reason}"),
            );
            return Err(unsupported_runner_error(runner_id, message));
        }
        let reason = contract
            .unsupported_reason
            .unwrap_or("command is local-only");
        if request.local_policy.deny_local_execution {
            return Err(local_execution_denied_error(reason, None));
        }
        plan = disabled_select_runner_plan(plan, reason);
        return Ok(skipped_automatic_run_local(plan, reason));
    }

    if request.explicit_runner.is_none() && !contract.routing_policy.default_lab_offload {
        if request.local_policy.deny_local_execution {
            return Err(local_execution_denied_error(
                "automatic Lab offload disabled",
                None,
            ));
        }
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
        request.local_policy.allow_local_hot,
    )?;
    let Some(selection) = selection else {
        let reason = if request.force_hot && request.local_policy.allow_local_hot {
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
        if request.local_policy.deny_local_execution {
            return Err(local_execution_denied_error(reason, None));
        }
        return Ok(skipped_automatic_run_local(plan, reason));
    };

    let mut messages = Vec::new();
    if matches!(selection.source, LabRunnerSelectionSource::Default) {
        // Make the auto-offload visible up front (#3815): the operator did not
        // ask for a runner explicitly, so spell out that this command is about
        // to leave the local machine and run remotely, on which runner, and how
        // to keep it local. Without this the first sign of remote execution is
        // a confusing remote-specific failure (e.g. a local `@file` that does
        // not exist on the runner).
        let auto_offload_signal = format!(
            "Lab offload: auto-selected default {} runner `{}`; this command will run REMOTELY on that runner, not on this machine. Pass `--force-hot --allow-local-hot` to run it locally instead.",
            selection.mode.label(),
            selection.runner_id
        );
        eprintln!("{auto_offload_signal}");
        messages.push(auto_offload_signal);
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
            // Release-gate routing safety (#4603): when a release gate's
            // default runner cannot be prepared for remote execution (e.g. a
            // stale daemon / version skew or a failed connection), silently
            // falling back to local execution produces a gate result that is
            // not faithful to the routing policy. Fail closed with a clear
            // diagnostic that surfaces the underlying runner reason, rather
            // than letting a stale launcher route the gate to the controller.
            // The operator-only override is `/release_gate/local_hot: allowed`.
            if contract.routing_policy.release_gate
                && matches!(selection.source, LabRunnerSelectionSource::Default)
                && !crate::core::defaults::resolve_release_gate_local_hot_policy().is_allowed()
            {
                return Err(release_gate_local_hot_denied_error(
                    format!(
                        "Release gate `{}` selected default Lab runner `{}` but could not prepare it for remote execution ({}); `/release_gate/local_hot` is `fail_closed`, so the gate will not silently fall back to local execution",
                        contract.hot_label, selection.runner_id, reason
                    ),
                    "release_gate",
                ));
            }
            if request.local_policy.deny_local_execution {
                return Err(local_execution_denied_error(
                    &reason,
                    Some(&selection.runner_id),
                ));
            }
            if !request.local_policy.allow_local_fallback {
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

fn unsupported_runner_hints(
    runner_id: &str,
    normalized_args: &[String],
    support_hint: String,
) -> Vec<String> {
    let mut hints = vec![support_hint];

    if let Some(commands) = review_lab_fallback_commands(runner_id, normalized_args) {
        hints.push(format!(
            "Scoped `homeboy review` cannot offload yet. Run these full-workspace Lab gates instead: {}; {}; {}.",
            commands.audit, commands.lint, commands.test
        ));
    }

    if let Some(service_command) = tunnel_service_command(normalized_args) {
        hints.push(format!(
            "`tunnel service {service_command} --runner {runner_id}` is not routed directly; inspect runner-side tunnel state with `homeboy runner exec {runner_id} --ssh --raw -- homeboy tunnel service {service_command} ...` until service inspection supports native --runner routing."
        ));
    }

    hints
}

struct ReviewLabFallbackCommands {
    audit: String,
    lint: String,
    test: String,
}

fn review_lab_fallback_commands(
    runner_id: &str,
    normalized_args: &[String],
) -> Option<ReviewLabFallbackCommands> {
    let review_index = normalized_args.iter().position(|arg| arg == "review")?;
    let review_args = &normalized_args[review_index + 1..];
    let mut component: Option<&str> = None;
    let mut path: Option<&str> = None;
    let mut extensions: Vec<&str> = Vec::new();
    let mut scoped = false;
    let mut i = 0;

    while i < review_args.len() {
        let arg = review_args[i].as_str();
        if arg == "--changed-only" || arg.starts_with("--changed-since=") {
            scoped = true;
        } else if arg == "--changed-since" {
            scoped = true;
            i += 1;
        } else if arg == "--path" {
            if let Some(value) = review_args.get(i + 1) {
                path = Some(value.as_str());
            }
            i += 1;
        } else if let Some(value) = arg.strip_prefix("--path=") {
            path = Some(value);
        } else if arg == "--extension" {
            if let Some(value) = review_args.get(i + 1) {
                extensions.push(value.as_str());
            }
            i += 1;
        } else if let Some(value) = arg.strip_prefix("--extension=") {
            extensions.push(value);
        } else if !arg.starts_with('-') && component.is_none() {
            component = Some(arg);
        }
        i += 1;
    }

    if !scoped {
        return None;
    }

    let mut common = vec!["--runner".to_string(), shell_arg(runner_id)];
    if let Some(path) = path {
        common.push("--path".to_string());
        common.push(shell_arg(path));
    }
    for extension in extensions {
        common.push("--extension".to_string());
        common.push(shell_arg(extension));
    }
    if path.is_none() {
        if let Some(component) = component {
            common.push(shell_arg(component));
        }
    }

    Some(ReviewLabFallbackCommands {
        audit: fallback_command("audit", &common),
        lint: fallback_command("lint", &common),
        test: fallback_command("test", &common),
    })
}

fn fallback_command(command: &str, args: &[String]) -> String {
    let suffix = if args.is_empty() {
        String::new()
    } else {
        format!(" {}", args.join(" "))
    };
    format!("`homeboy {command}{suffix}`")
}

fn shell_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tunnel_service_command(normalized_args: &[String]) -> Option<&str> {
    normalized_args.windows(3).find_map(|window| {
        let [first, second, third] = window else {
            return None;
        };
        if first == "tunnel" && second == "service" {
            match third.as_str() {
                "list" | "show" | "status" | "url" | "set" | "remove" => Some(third.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}

#[allow(clippy::too_many_arguments)]
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
    let mut env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    let tunnel_secret_env = hydrate_tunnel_secret_env(&remapped_args, &mut env)?;
    lab_metadata["tunnel_secret_env"] = tunnel_secret_env;
    env = build_lab_offload_env_with_passthroughs(&lab_metadata);
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
            detach_after_handoff: false,
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

struct LabOffloadWorkspaceStage {
    plan: HomeboyPlan,
    sync_mode: RunnerWorkspaceSyncMode,
    changed_since_preflight: LabOffloadChangedSincePreflight,
    synced: RunnerWorkspaceSyncOutput,
    remote_cwd: String,
    workspace_mapping: Vec<LabWorkspaceMappingEntry>,
    source_snapshot: SourceSnapshot,
    remapped_args: Vec<String>,
    agent_task_run_id: Option<String>,
    command: Vec<String>,
    remote_command: Vec<String>,
    synced_rigs: Vec<rig_materialization::LabOffloadRigSync>,
    rig_component_path_overrides: Vec<(String, String)>,
}

#[allow(clippy::too_many_arguments)]
fn prepare_lab_offload_workspace_stage(
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
    let path_remaps: Vec<LabPathRemap> = workspace_mapping
        .iter()
        .map(|entry| LabPathRemap {
            local: entry.local_path().to_string(),
            remote: entry.remote_path().to_string(),
        })
        .collect();
    preflight_provider_config_source_cli_dependencies(&offload_args, &synced.excludes)?;
    let remapped_args = rig_materialization::remap_bench_rig_default_component_to_primary_snapshot(
        &offload_args,
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
    plan = record_synced_remapped_workspace_entry(
        plan,
        &mut workspace_mapping,
        synced_remapped_tasks,
        "lab.sync_remapped_agent_task_tasks",
    );
    let (remapped_args, synced_remapped_plan) =
        materialize_inline_agent_task_plan_arg(runner_id, &remapped_args)?;
    plan = record_synced_remapped_workspace_entry(
        plan,
        &mut workspace_mapping,
        synced_remapped_plan,
        "lab.sync_remapped_agent_task_plan",
    );
    let (remapped_args, agent_task_run_id) =
        ensure_agent_task_dispatch_run_id_with(&remapped_args, run_isolation_token.as_deref())
            .map_or((remapped_args, None), |(args, run_id)| (args, Some(run_id)));

    let mut command = command_prefix_argv.to_vec();
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
        synced_rigs,
        rig_component_path_overrides,
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
                if request.local_policy.deny_local_execution {
                    return Err(local_execution_denied_error(&reason, Some(runner_id)));
                }
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
                        request.local_policy.allow_local_fallback,
                        request.local_policy.deny_local_execution,
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
    let workspace_stage = prepare_lab_offload_workspace_stage(
        &request,
        &contract,
        plan,
        runner_id,
        &source_path,
        homeboy_path,
        &command_prefix.argv,
        runner.workspace_root.as_deref(),
    )?;
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
        synced_rigs,
        rig_component_path_overrides,
    } = workspace_stage;
    plan = next_plan;

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
    let mut env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    let rig_component_path_env = forward_rig_component_path_env(&mut env, &workspace_mapping)?;
    apply_rig_component_path_overrides(&mut env, &rig_component_path_overrides);
    let agent_task_secret_env =
        hydrate_agent_task_secret_env(&changed_since_preflight.args, &mut env)?;
    let trace_secret_env = hydrate_trace_secret_env(&changed_since_preflight.args, &mut env)?;
    let tunnel_secret_env = hydrate_tunnel_secret_env(&changed_since_preflight.args, &mut env)?;
    lab_metadata["agent_task_secret_env"] = agent_task_secret_env;
    lab_metadata["trace_secret_env"] = trace_secret_env;
    lab_metadata["tunnel_secret_env"] = tunnel_secret_env;
    lab_metadata["rig_component_path_env"] = rig_component_path_env;
    lab_metadata["rig_component_path_overrides"] =
        rig_component_path_overrides_metadata(&rig_component_path_overrides);
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
    env = build_lab_offload_env_with_passthroughs(&lab_metadata);
    forward_rig_component_path_env(&mut env, &workspace_mapping)?;
    apply_rig_component_path_overrides(&mut env, &rig_component_path_overrides);
    hydrate_agent_task_secret_env(&changed_since_preflight.args, &mut env)?;
    hydrate_trace_secret_env(&changed_since_preflight.args, &mut env)?;
    hydrate_tunnel_secret_env(&changed_since_preflight.args, &mut env)?;
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
            detach_after_handoff: request.detach_after_handoff,
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
                        if request.local_policy.deny_local_execution {
                            Err(local_execution_denied_error(&reason, Some(runner_id)))
                        } else if !request.local_policy.allow_local_fallback {
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
    ensure_lab_offload_streams_not_truncated(&exec_output)?;
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
        if let Some(run_id) = agent_task_run_id.as_deref() {
            if let Some(envelope) = parse_offloaded_dispatch_envelope_from_outputs(
                &exec_output.stdout,
                &exec_output.stderr,
            )? {
                if let Some(record) = agent_task_lifecycle::record_remote_dispatch_failure(
                    agent_task_lifecycle::AgentTaskRemoteDispatchFailure {
                        identity: agent_task_lifecycle::RunDispatchIdentity { run_id, runner_id },
                        local_command: request.normalized_args.to_vec(),
                        remote_command: remote_command.clone(),
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
                    identity: agent_task_lifecycle::RunDispatchIdentity { run_id, runner_id },
                    local_command: request.normalized_args.to_vec(),
                    remote_command: remote_command.clone(),
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

fn ensure_lab_offload_streams_not_truncated(
    exec_output: &super::super::RunnerExecOutput,
) -> Result<()> {
    let Some(capture) = exec_output.capture.as_ref() else {
        return Ok(());
    };
    if !capture.stdout.truncated && !capture.stderr.truncated {
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

/// Insert generic `${components.<id>.path}` override env vars so a remote rig
/// check resolves component paths to the runner-side materialized checkout
/// instead of the controller path the rig spec declares (issue #3766/#3767).
fn apply_rig_component_path_overrides(
    env: &mut std::collections::HashMap<String, String>,
    overrides: &[(String, String)],
) {
    for (name, value) in overrides {
        if !value.trim().is_empty() {
            env.insert(name.clone(), value.clone());
        }
    }
}

/// Build diagnostics describing each rig component path override forwarded to
/// the runner, so bench artifacts show how `${components.<id>.path}` resolved.
fn rig_component_path_overrides_metadata(overrides: &[(String, String)]) -> serde_json::Value {
    let forwarded = overrides
        .iter()
        .map(|(name, runner_path)| {
            serde_json::json!({
                "env_name": name,
                "runner_path": runner_path,
                "forwarded_to_runner": true,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema": "homeboy/lab-offload-rig-component-path-override/v1",
        "overrides": forwarded,
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

fn lab_materialization_proof_metadata(
    source_snapshot: &SourceSnapshot,
    workspace_snapshot_identity: &str,
    remote_workspace: &str,
    runner_homeboy: &serde_json::Value,
    source_checkout: &serde_json::Value,
    workspace_mapping: &serde_json::Value,
    synced_rigs: &[rig_materialization::LabOffloadRigSync],
) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/lab-materialization-proof/v1",
        "remote_workspace": remote_workspace,
        "workload_hashes": {
            "source_snapshot_hash": source_snapshot.snapshot_hash,
            "workspace_snapshot_identity": workspace_snapshot_identity,
        },
        "source_snapshot": source_snapshot,
        "source_checkout": source_checkout,
        "runner_homeboy": runner_homeboy,
        "wp_codebox_version": passive_wp_codebox_version(),
        "workspace_mapping": workspace_mapping,
        "rigs": synced_rigs,
    })
}

fn passive_wp_codebox_version() -> Option<String> {
    ["HOMEBOY_WP_CODEBOX_VERSION", "WP_CODEBOX_VERSION"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

pub(super) fn runner_homeboy_daemon_display(metadata: &serde_json::Value) -> String {
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
        true,
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
    deny_local_execution: bool,
) -> Result<LabOffloadOutcome> {
    if deny_local_execution {
        return Err(local_execution_denied_error(
            &reason,
            Some(&selection.runner_id),
        ));
    }
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
    use crate::core::engine::command::{CaptureMetadata, CommandCaptureMetadata};
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
            unsupported_reason: None,
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            required_extensions: Vec::new(),
            requires_playwright: false,
            routing_policy: crate::command_contract::LabRoutingPolicy {
                default_lab_offload: true,
                infer_source_path_tools: true,
                release_gate: false,
                requires_extension_parity: true,
            },
        }
    }

    fn local_only_lab_command(reason: &'static str) -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: "rig up",
            portable: false,
            unsupported_reason: Some(reason),
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            required_extensions: Vec::new(),
            requires_playwright: false,
            routing_policy: crate::command_contract::LabRoutingPolicy::default(),
        }
    }

    #[test]
    fn scoped_review_runner_rejection_includes_full_gate_fallbacks() {
        let args = vec![
            "homeboy".to_string(),
            "review".to_string(),
            "homeboy".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
            "--extension".to_string(),
            "rust".to_string(),
        ];

        let hints = unsupported_runner_hints("homeboy-lab", &args, "support".to_string());

        assert!(hints.iter().any(|hint| hint.contains(
            "`homeboy audit --runner homeboy-lab --extension rust homeboy`; `homeboy lint --runner homeboy-lab --extension rust homeboy`; `homeboy test --runner homeboy-lab --extension rust homeboy`"
        )));
    }

    #[test]
    fn unscoped_review_runner_rejection_does_not_include_fallbacks() {
        let args = vec!["homeboy".to_string(), "review".to_string()];

        let hints = unsupported_runner_hints("homeboy-lab", &args, "support".to_string());

        assert_eq!(hints, vec!["support".to_string()]);
    }

    #[test]
    fn lab_git_workspace_sync_uses_snapshot_for_private_proxied_sources() {
        let source_policy =
            crate::core::runner::source_materialization::SourceMaterializationPolicy {
                private_proxied_source_hosts: vec!["github.example.com".to_string()],
            };
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
                "git@github.example.com:example-org/example-repo.git",
            ])
            .current_dir(dir.path())
            .status()
            .expect("add origin");

        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        let mode = lab_workspace_sync_mode_with_source_policy(
            LabOffloadWorkspaceModePolicy::Git,
            &args,
            dir.path(),
            &source_policy,
        )
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
                "git@github.example.com:example-org/conductor.git",
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
                .is_some_and(|hint| hint.contains("Homeboy worktree"))));
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
        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("clean task worktree"))));
        assert!(tried
            .iter()
            .any(|hint| hint.as_str().is_some_and(|hint| hint.contains("dirty.txt"))));
        assert!(!tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Commit or stash"))));
        assert!(!tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("--force-hot"))));
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
                worker_identity: Some("worker-1".to_string()),
                worker_pid: Some(1234),
                last_seen_at: Some(chrono::Utc::now().to_rfc3339()),
            }),
            stale_daemon: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            active_job_count: 0,
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
        command.routing_policy.infer_source_path_tools = false;

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
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/Users/user/Developer/app".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
            current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
                local_path: "/Users/user/Developer/app".to_string(),
                remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                materialized: true,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
            },
            workspace_lease: crate::core::runner::RunnerWorkspaceLease {
                runner_id: "lab".to_string(),
                local_path: "/Users/user/Developer/app".to_string(),
                remote_path: "/srv/homeboy/_lab_workspaces/app-abc".to_string(),
                sync_mode: "snapshot".to_string(),
                materialized: true,
                lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
            },
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
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/Users/user/Developer/dep".to_string(),
            remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
            current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
                local_path: "/Users/user/Developer/dep".to_string(),
                remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Git,
                materialized: true,
                source_commit: Some("abc123".to_string()),
                source_ref: Some("main".to_string()),
                source_dirty: Some(false),
            },
            workspace_lease: crate::core::runner::RunnerWorkspaceLease {
                runner_id: "lab".to_string(),
                local_path: "/Users/user/Developer/dep".to_string(),
                remote_path: "/srv/homeboy/_lab_workspaces/dep-def".to_string(),
                sync_mode: "git".to_string(),
                materialized: true,
                lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
                source_commit: Some("abc123".to_string()),
                source_ref: Some("main".to_string()),
                source_dirty: Some(false),
            },
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
            metadata["local_to_remote"]["/Users/user/Developer/dep"],
            "/srv/homeboy/_lab_workspaces/dep-def"
        );
    }

    #[test]
    fn lab_offload_env_contains_workspace_mapping_metadata() {
        let mapping = serde_json::json!({
            "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
            "local_to_remote": {
                "/Users/user/Developer/app": "/srv/homeboy/_lab_workspaces/app-abc"
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
    fn materialization_proof_records_hashes_source_and_runner_identity() {
        let source_snapshot = SourceSnapshot {
            runner_id: "lab".to_string(),
            local_path: Some("/Users/user/Developer/app".to_string()),
            remote_path: Some("/srv/homeboy/_lab_workspaces/app-abc".to_string()),
            workspace_root: Some("/Users/user/Developer/app".to_string()),
            git_branch: Some("main".to_string()),
            git_sha: Some("abc123".to_string()),
            dirty: false,
            sync_mode: "lab_offload".to_string(),
            snapshot_hash: "sha256:source".to_string(),
            synced_at: "2026-06-21T00:00:00Z".to_string(),
            sync_excludes: vec!["target/".to_string()],
        };
        let runner_homeboy = serde_json::json!({
            "schema": "homeboy/lab-runner-homeboy/v1",
            "active_daemon_version": "homeboy 0.1.0",
        });
        let source_checkout = serde_json::json!({
            "schema": "homeboy/lab-source-checkout/v1",
            "git_sha": "abc123",
        });
        let workspace_mapping = serde_json::json!({
            "schema": LAB_WORKSPACE_MAPPING_SCHEMA,
            "workspaces": [],
            "local_to_remote": {},
        });

        let proof = lab_materialization_proof_metadata(
            &source_snapshot,
            "snapshot:workspace",
            "/srv/homeboy/_lab_workspaces/app-abc",
            &runner_homeboy,
            &source_checkout,
            &workspace_mapping,
            &[],
        );

        assert_eq!(proof["schema"], "homeboy/lab-materialization-proof/v1");
        assert_eq!(
            proof["remote_workspace"],
            "/srv/homeboy/_lab_workspaces/app-abc"
        );
        assert_eq!(
            proof["workload_hashes"]["source_snapshot_hash"],
            "sha256:source"
        );
        assert_eq!(
            proof["workload_hashes"]["workspace_snapshot_identity"],
            "snapshot:workspace"
        );
        assert_eq!(proof["source_snapshot"]["git_sha"], "abc123");
        assert_eq!(
            proof["runner_homeboy"]["active_daemon_version"],
            "homeboy 0.1.0"
        );
        assert!(proof["wp_codebox_version"].is_null());
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
            "/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy",
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
            .contains("/home/user/Developer/_lab_workspaces/homeboy-post-4583-proof"));
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
    fn lab_runner_selection_keeps_explicit_runner_precedence() {
        let command = portable_lab_command("test");
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
            false,
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
        command.routing_policy.default_lab_offload = false;

        let selection = resolve_lab_runner_selection_from_default(
            &command,
            None,
            false,
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
        command.routing_policy.default_lab_offload = false;

        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
            false,
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
            &command, None, false, false, false, false, None
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
            &command, None, true, false, false, false, None
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

        let err = resolve_lab_runner_selection_from_default(
            &command, None, true, true, true, false, None,
        )
        .expect_err("bench local execution should be denied by config policy");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("/bench/local_execution"));
        assert!(err.message.contains("denied"));
        let tried = err.details["tried"].as_array().expect("tried");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("--runner <runner-id>"))));
    }

    fn release_gate_lab_command(label: &'static str) -> LabOffloadCommand {
        let mut command = portable_lab_command(label);
        command.routing_policy.release_gate = true;
        command
    }

    #[test]
    fn release_gate_force_hot_allow_local_hot_fails_closed_with_default_runner() {
        // #4605: --force-hot --allow-local-hot must not silently bypass Lab
        // routing for a release gate when a default runner is configured.
        let command = release_gate_lab_command("lint");

        let err = resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            true,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect_err("release gate force-local bypass must fail closed");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("Release gate `lint`"));
        assert!(err.message.contains("--force-hot --allow-local-hot"));
        assert!(err.message.contains("lab-default"));
        assert!(err.message.contains("/release_gate/local_hot"));
        let tried = err.details["tried"].as_array().expect("tried");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("/release_gate/local_hot"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("HOMEBOY_RELEASE_GATE_LOCAL_HOT"))));
    }

    #[test]
    fn release_gate_force_hot_allow_local_hot_allowed_by_policy() {
        // When the operator opts in via /release_gate/local_hot=allowed, the
        // bypass runs locally and is recorded (None selection → local run).
        let command = release_gate_lab_command("test");

        assert!(resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            true,
            false,
            true,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .is_none());
    }

    #[test]
    fn release_gate_force_hot_allow_local_hot_runs_local_without_default_runner() {
        // No default runner configured → nothing to route to, so the gate runs
        // locally even under fail_closed.
        let command = release_gate_lab_command("audit");

        assert!(resolve_lab_runner_selection_from_default(
            &command, None, true, true, false, false, None
        )
        .expect("selection")
        .is_none());
    }

    #[test]
    fn non_release_gate_command_keeps_allow_local_hot_bypass() {
        // Non-gate portable commands (e.g. agent-task) keep the existing
        // --force-hot --allow-local-hot bypass behavior.
        let command = portable_lab_command("agent-task dispatch/cook/loop/run-plan");

        assert!(resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            true,
            false,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .is_none());
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
    fn lab_offload_rejects_truncated_runner_stdout() {
        let exec_output = RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab-default".to_string(),
            dry_run: false,
            mode: RunnerExecMode::Daemon,
            argv: vec!["homeboy".to_string(), "agent-task".to_string()],
            remote_cwd: "/srv/homeboy/_lab_workspaces/sample-plugin-code".to_string(),
            exit_code: 0,
            stdout: "tail-only-json-fragment".to_string(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: Some("job-123".to_string()),
            job_events: None,
            mirror_run_id: Some("runner-exec-lab-default-job-123".to_string()),
            patch: None,
            artifacts: Vec::new(),
            metrics: None,
            capture: Some(CommandCaptureMetadata {
                stdout: CaptureMetadata {
                    bytes_seen: 4_500_000,
                    bytes_retained: 4 * 1024 * 1024,
                    byte_limit: 4 * 1024 * 1024,
                    truncated: true,
                },
                stderr: CaptureMetadata::default(),
            }),
            runner_result: None,
            handoff: None,
            diagnostics: None,
        };

        let err = ensure_lab_offload_streams_not_truncated(&exec_output)
            .expect_err("truncated stdout is rejected");

        assert_eq!(err.code.as_str(), "internal.unexpected");
        assert!(err.message.contains("output exceeded"));
        assert_eq!(err.details["runner_id"], "lab-default");
        assert_eq!(err.details["job_id"], "job-123");
        assert_eq!(err.details["capture"]["stdout"]["truncated"], true);
    }

    #[test]
    fn missing_mutation_patch_error_points_to_runner_evidence_and_retry() {
        let exec_output = RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab-default".to_string(),
            dry_run: false,
            mode: RunnerExecMode::Daemon,
            argv: vec![
                "homeboy".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "lint".to_string(),
                "--write".to_string(),
                "sample-plugin-code".to_string(),
            ],
            remote_cwd: "/srv/homeboy/_lab_workspaces/sample-plugin-code".to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: Some("job-123".to_string()),
            job_events: None,
            mirror_run_id: Some("runner-exec-lab-default-job-123".to_string()),
            patch: None,
            artifacts: Vec::new(),
            metrics: None,
            capture: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        };

        let err = missing_mutation_patch_error(
            &[
                "homeboy".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "lint".to_string(),
                "--write".to_string(),
                "sample-plugin-code".to_string(),
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
            .any(|hint| hint.contains("homeboy refactor --from lint --write sample-plugin-code")));
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
            false,
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
            local_policy: LabLocalExecutionPolicy {
                allow_local_hot: true,
                allow_local_fallback: false,
                deny_local_execution: false,
            },
            allow_dirty_lab_workspace: false,
            capture_patch: false,
            mutation_flag: None,
            detach_after_handoff: false,
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
    fn lab_only_refuses_local_execution_without_lab_contract() {
        let outcome = execute_lab_offload(LabOffloadRequest {
            command: None,
            normalized_args: &["homeboy".to_string(), "status".to_string()],
            explicit_runner: None,
            force_hot: false,
            local_policy: LabLocalExecutionPolicy {
                allow_local_hot: false,
                allow_local_fallback: false,
                deny_local_execution: true,
            },
            allow_dirty_lab_workspace: false,
            capture_patch: false,
            mutation_flag: None,
            detach_after_handoff: false,
        });

        let Err(err) = outcome else {
            panic!("lab-only should refuse local execution");
        };
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("Lab-only execution refused"));
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
            local_policy: LabLocalExecutionPolicy::default(),
            allow_dirty_lab_workspace: false,
            capture_patch: false,
            mutation_flag: None,
            detach_after_handoff: false,
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
