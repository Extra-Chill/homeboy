use std::path::Path;

use crate::core::observation::{PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV};
use crate::core::plan::{HomeboyPlan, PlanStep, PlanStepStatus, PlanValues};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, Result};

use super::{
    evaluate_lab_runner_capabilities_for_runner, exec, lab_offload_changed_since_ref,
    lab_offload_metadata, lab_offload_metadata_with_workspace_mapping, lab_runner_capability_plan,
    load, preflight_lab_offload_changed_since, prepare_git_lab_offload_changed_since,
    rig_materialization, status, sync_workspace, LabRunnerGateDecision, RunnerCapabilityPreflight,
    RunnerExecOptions, RunnerStatusReport, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

use super::daemon_health::runner_daemon_health_failure;
use super::lab_apply::apply_lab_offload_patch;
use super::lab_args::{lab_offload_source_path, rewrite_lab_offload_args};
use super::lab_capabilities::lab_runner_capability_contract;
use super::lab_command::lab_offload_command_prefix;
use super::lab_env::{build_lab_offload_env, forward_env_if_present};
use super::lab_plan::{base_lab_plan, disabled_select_runner_plan, with_step};
pub use super::lab_selection::LabRunnerSelectionSource;
use super::lab_selection::{
    prepare_lab_runner_for_offload, resolve_lab_runner_selection, status_tunnel_mode,
    LabRunnerPreparation, LabRunnerSelection,
};
use super::lab_workspaces::{
    lab_extra_workspaces, lab_workspace_mapping_metadata, sync_extra_lab_workspaces,
    workspace_mapping_entry,
};

pub struct LabOffloadRequest<'a> {
    pub command: Option<LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabOffloadWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
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

pub fn execute_lab_offload(request: LabOffloadRequest<'_>) -> Result<LabOffloadOutcome> {
    let unsupported_runner_error = |runner_id: &str, message: String| {
        Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            Some(vec!["Current Lab offload support: audit, bench run, full lint, full test, trace, and refactor source runs.".to_string()]),
        )
    };
    let mut plan = base_lab_plan(request.command.as_ref());
    let Some(contract) = request.command.clone() else {
        if let Some(runner_id) = request.explicit_runner {
            return Err(unsupported_runner_error(
                runner_id,
                "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, trace, and refactor source runs".to_string(),
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
                || "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, trace, and refactor source runs".to_string(),
                |reason| format!("--runner is unavailable for this hot command. {reason}"),
            );
            return Err(unsupported_runner_error(runner_id, message));
        }
        return Ok(LabOffloadOutcome::RunLocal {
            plan: disabled_select_runner_plan(plan, "command is local-only"),
            metadata: None,
            messages: Vec::new(),
        });
    }

    let selection =
        resolve_lab_runner_selection(&contract, request.explicit_runner, request.force_hot)?;
    let Some(selection) = selection else {
        let reason = if request.force_hot {
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
    messages: Vec<String>,
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

    let source_path = lab_offload_source_path(request.normalized_args)?;
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

        match decision {
            LabRunnerGateDecision::Eligible => {
                plan = with_step(
                    plan,
                    PlanStep::ready("lab.capability_preflight", "lab.capability_preflight")
                        .inputs(PlanValues::new().string("command", capability_plan.command))
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
                        .build(),
                    );
                    return Ok(automatic_capability_fallback(
                        plan,
                        runner_id,
                        &runner_status,
                        reason,
                    ));
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
    let sync_mode = match contract.workspace_mode_policy {
        LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
            if lab_offload_changed_since_ref(request.normalized_args).is_some() {
                RunnerWorkspaceSyncMode::Git
            } else {
                RunnerWorkspaceSyncMode::Snapshot
            }
        }
    };
    let changed_since_preflight = if sync_mode == RunnerWorkspaceSyncMode::Git {
        prepare_git_lab_offload_changed_since(request.normalized_args, &source_path)?
    } else {
        preflight_lab_offload_changed_since(request.normalized_args, sync_mode)?
    };
    let extra_workspaces = lab_extra_workspaces(&source_path)?;
    let synced = sync_workspace(
        runner_id,
        RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            mode: sync_mode,
            changed_since_base: changed_since_preflight.resolved_base.clone(),
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
    )?;
    if synced_rigs > 0 {
        plan = with_step(
            plan,
            PlanStep::ready("lab.sync_rigs", "lab.sync_rigs")
                .inputs(PlanValues::new().json("count", synced_rigs))
                .build(),
        );
    }

    let mut command = command_prefix.argv;
    command.extend(
        rewrite_lab_offload_args(&changed_since_preflight.args, &remote_cwd)
            .into_iter()
            .skip(1),
    );
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
    let lab_metadata = lab_offload_metadata_with_workspace_mapping(
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
        },
    );
    let (exec_output, exit_code) = match exec_result {
        Ok(output) => output,
        Err(err) => {
            if let Some(reason) = runner_daemon_health_failure(&err) {
                plan = with_step(
                    plan,
                    PlanStep::builder("lab.exec", "lab.exec", PlanStepStatus::Failed)
                        .skip_reason(reason.clone())
                        .build(),
                );
                return match selection.source {
                    LabRunnerSelectionSource::Default => Ok(LabOffloadOutcome::RunLocal {
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
                        messages: vec![format!("Lab offload: {reason}; running locally.")],
                    }),
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
        }
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

#[cfg(test)]
#[path = "lab_arg_tests.rs"]
mod lab_arg_tests;

#[cfg(test)]
mod tests {
    use super::super::lab_selection::{
        prepare_lab_runner_for_offload_with, resolve_lab_runner_selection_from_default,
    };
    use super::super::lab_workspaces::LAB_WORKSPACE_MAPPING_SCHEMA;
    use super::*;
    use crate::core::observation::LAB_OFFLOAD_METADATA_ENV;
    use crate::core::plan::PlanKind;
    use crate::core::runner::{
        RunnerConnectReport, RunnerRequiredTool, RunnerTunnelMode, RunnerWorkspaceSyncOutput,
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
    fn lab_runner_selection_keeps_explicit_runner_precedence() {
        let command = portable_lab_command("test");
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
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

        assert!(
            resolve_lab_runner_selection_from_default(&command, None, false, None)
                .expect("selection")
                .is_none()
        );
    }

    #[test]
    fn lab_runner_selection_force_hot_is_local_escape_hatch() {
        let command = portable_lab_command("test");

        assert!(resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            Some("lab-default".to_string())
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
            Some("lab-default".to_string()),
        )
        .expect_err("rig up rejects explicit runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("single-workspace Lab snapshot"));
    }

    #[test]
    fn lab_runner_preparation_falls_back_for_unreachable_default_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: RunnerTunnelMode::DirectSsh,
        };

        let prepared = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: false,
                    state: super::super::RunnerSessionState::Disconnected,
                    session: None,
                    stale_daemon: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |runner_id| {
                Ok((
                    RunnerConnectReport {
                        runner_id: runner_id.to_string(),
                        mode: None,
                        role: None,
                        connected: false,
                        recorded: None,
                        local_url: None,
                        broker_url: None,
                        controller_id: None,
                        remote_daemon_address: None,
                        tunnel_pid: None,
                        remote_daemon_pid: None,
                        homeboy_version: None,
                        session_path: Some("/tmp/lab.json".to_string()),
                        failure_kind: Some(super::super::RunnerFailureKind::SshFailure),
                        failure_message: Some("SSH connectivity check failed".to_string()),
                    },
                    20,
                ))
            },
        )
        .expect("prepared");

        assert_eq!(
            prepared,
            LabRunnerPreparation::FallBackLocal {
                reason: "SSH connectivity check failed".to_string()
            }
        );
    }

    #[test]
    fn lab_runner_preparation_uses_already_connected_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: RunnerTunnelMode::DirectSsh,
        };

        let prepared = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: true,
                    state: super::super::RunnerSessionState::Connected,
                    session: None,
                    stale_daemon: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |_| panic!("connected runner should not reconnect"),
        )
        .expect("prepared");

        assert_eq!(prepared, LabRunnerPreparation::Ready);
    }

    #[test]
    fn lab_runner_preparation_connects_disconnected_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: RunnerTunnelMode::DirectSsh,
        };

        let prepared = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: false,
                    state: super::super::RunnerSessionState::Disconnected,
                    session: None,
                    stale_daemon: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |runner_id| {
                Ok((
                    RunnerConnectReport {
                        runner_id: runner_id.to_string(),
                        mode: Some(RunnerTunnelMode::DirectSsh),
                        role: Some(super::super::RunnerSessionRole::Controller),
                        connected: true,
                        recorded: None,
                        local_url: Some("http://127.0.0.1:1234".to_string()),
                        broker_url: None,
                        controller_id: None,
                        remote_daemon_address: Some("127.0.0.1:5678".to_string()),
                        tunnel_pid: None,
                        remote_daemon_pid: Some(42),
                        homeboy_version: Some("homeboy 0.0.0".to_string()),
                        session_path: Some("/tmp/lab.json".to_string()),
                        failure_kind: None,
                        failure_message: None,
                    },
                    0,
                ))
            },
        )
        .expect("prepared");

        assert_eq!(prepared, LabRunnerPreparation::Ready);
    }

    #[test]
    fn lab_runner_preparation_errors_for_unreachable_explicit_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::DirectSsh,
        };

        let err = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: false,
                    state: super::super::RunnerSessionState::Disconnected,
                    session: None,
                    stale_daemon: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |runner_id| {
                Ok((
                    RunnerConnectReport {
                        runner_id: runner_id.to_string(),
                        mode: None,
                        role: None,
                        connected: false,
                        recorded: None,
                        local_url: None,
                        broker_url: None,
                        controller_id: None,
                        remote_daemon_address: None,
                        tunnel_pid: None,
                        remote_daemon_pid: None,
                        homeboy_version: None,
                        session_path: Some("/tmp/lab.json".to_string()),
                        failure_kind: Some(super::super::RunnerFailureKind::SshFailure),
                        failure_message: Some("SSH connectivity check failed".to_string()),
                    },
                    20,
                ))
            },
        )
        .expect_err("explicit runner should error");

        assert!(err.message.contains("could not connect runner"));
    }

    #[test]
    fn plan_records_skipped_auto_offload() {
        let outcome = execute_lab_offload(LabOffloadRequest {
            command: Some(portable_lab_command("test")),
            normalized_args: &["homeboy".to_string(), "test".to_string()],
            explicit_runner: None,
            force_hot: true,
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
