use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::core::observation::{PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV};
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanStepStatus, PlanValues};
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, Result};

use super::{
    evaluate_lab_runner_capabilities_for_runner, exec, lab_offload_changed_since_ref,
    lab_offload_metadata, lab_offload_metadata_with_workspace_mapping, lab_runner_capability_plan,
    load, preflight_lab_offload_changed_since, prepare_git_lab_offload_changed_since,
    rig_materialization, status, sync_workspace, LabRunnerCapabilityContract,
    LabRunnerGateDecision, LabRunnerGateMode, RunnerCapabilityPreflight, RunnerConnectReport,
    RunnerExecOptions, RunnerRequiredTool, RunnerStatusReport, RunnerTunnelMode,
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};

use super::daemon_health::runner_daemon_health_failure;
use super::lab_apply::apply_lab_offload_patch;
use super::lab_command::lab_offload_command_prefix;
use super::lab_env::{build_lab_offload_env, forward_env_if_present};
use super::lab_workspaces::{
    lab_extra_workspaces, lab_workspace_mapping_metadata, sync_extra_lab_workspaces,
    workspace_mapping_entry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerSelectionSource {
    Explicit,
    Default,
}

impl LabRunnerSelectionSource {
    fn metadata_value(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Default => "automatic",
        }
    }

    fn gate_mode(self) -> LabRunnerGateMode {
        match self {
            Self::Explicit => LabRunnerGateMode::Explicit,
            Self::Default => LabRunnerGateMode::Automatic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LabRunnerSelection {
    runner_id: String,
    source: LabRunnerSelectionSource,
    mode: RunnerTunnelMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LabRunnerPreparation {
    Ready,
    FallBackLocal { reason: String },
}

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

fn base_lab_plan(command: Option<&LabOffloadCommand>) -> HomeboyPlan {
    let description = command
        .map(|contract| contract.hot_label)
        .unwrap_or("command");
    HomeboyPlan::builder_for_description(PlanKind::LabOffload, description)
        .mode("lab_offload")
        .build()
}

fn with_step(mut plan: HomeboyPlan, step: PlanStep) -> HomeboyPlan {
    plan.steps.push(step);
    plan
}

fn disabled_select_runner_plan(plan: HomeboyPlan, reason: &'static str) -> HomeboyPlan {
    with_step(
        plan,
        PlanStep::disabled_with_reason("lab.select_runner", "lab.select_runner", reason).build(),
    )
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

fn prepare_lab_runner_for_offload(selection: &LabRunnerSelection) -> Result<LabRunnerPreparation> {
    let runner = load(&selection.runner_id)?;
    if runner.kind != super::RunnerKind::Ssh {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote direct SSH or reverse-connected runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner before using Lab offload.".to_string(),
            ]),
        ));
    }

    prepare_lab_runner_for_offload_with(selection, status, |runner_id| {
        connect_runner_for_offload(runner_id, selection.source)
    })
}

fn connect_runner_for_offload(
    runner_id: &str,
    source: LabRunnerSelectionSource,
) -> Result<(RunnerConnectReport, i32)> {
    let timeout = lab_connect_timeout(source);
    let (stdout, stderr, exit_code, timed_out) = run_runner_connect_command(runner_id, timeout)?;
    let status = status(runner_id)?;

    if status.connected {
        if let Some(session) = status.session {
            return Ok((
                RunnerConnectReport {
                    runner_id: runner_id.to_string(),
                    mode: Some(session.mode),
                    role: Some(session.role),
                    connected: true,
                    recorded: None,
                    local_url: session.local_url,
                    broker_url: session.broker_url,
                    controller_id: session.controller_id,
                    remote_daemon_address: session.remote_daemon_address,
                    tunnel_pid: session.tunnel_pid,
                    remote_daemon_pid: session.remote_daemon_pid,
                    homeboy_version: Some(session.homeboy_version),
                    session_path: Some(status.session_path),
                    failure_kind: None,
                    failure_message: None,
                },
                0,
            ));
        }
    }

    let reason = if timed_out {
        format!("runner connect timed out after {}s", timeout.as_secs())
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        if detail.is_empty() {
            format!("runner connect exited with code {exit_code}")
        } else {
            format!("runner connect exited with code {exit_code}: {detail}")
        }
    };

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
            session_path: Some(status.session_path),
            failure_kind: Some(super::RunnerFailureKind::SshFailure),
            failure_message: Some(reason),
        },
        exit_code,
    ))
}

fn lab_connect_timeout(source: LabRunnerSelectionSource) -> Duration {
    match source {
        LabRunnerSelectionSource::Explicit => Duration::from_secs(30),
        LabRunnerSelectionSource::Default => Duration::from_secs(3),
    }
}

fn run_runner_connect_command(
    runner_id: &str,
    timeout: Duration,
) -> Result<(String, String, i32, bool)> {
    let exe = std::env::current_exe().map_err(|err| {
        Error::internal_io(err.to_string(), Some("resolve homeboy executable".into()))
    })?;
    let mut child = std::process::Command::new(exe)
        .args(["runner", "connect", runner_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error::internal_io(err.to_string(), Some("start runner connect".into())))?;
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            Error::internal_io(err.to_string(), Some("wait runner connect".into()))
        })? {
            let mut stdout = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            return Ok((stdout, stderr, status.code().unwrap_or(-1), false));
        }

        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok((String::new(), String::new(), 124, true));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn prepare_lab_runner_for_offload_with(
    selection: &LabRunnerSelection,
    status_fn: impl Fn(&str) -> Result<RunnerStatusReport>,
    connect_fn: impl Fn(&str) -> Result<(RunnerConnectReport, i32)>,
) -> Result<LabRunnerPreparation> {
    let status = status_fn(&selection.runner_id)?;
    if status.connected {
        if let Some(reason) = connected_runner_not_ready_reason(&selection.runner_id, &status) {
            return automatic_fallback_or_explicit_error(
                selection,
                reason,
                format!(
                    "Lab offload runner `{}` is connected but is not ready for remote execution",
                    selection.runner_id
                ),
                format!(
                    "Run `homeboy runner connect {}` to refresh the runner daemon session.",
                    selection.runner_id
                ),
            );
        }
        eprintln!(
            "Lab offload: runner `{}` is connected via {} mode.",
            selection.runner_id,
            status_tunnel_mode(&status).label()
        );
        return Ok(LabRunnerPreparation::Ready);
    }

    if status_tunnel_mode(&status) == RunnerTunnelMode::Reverse {
        let reason = format!(
            "reverse-connected runner `{}` is not currently connected",
            selection.runner_id
        );
        return automatic_fallback_or_explicit_error(
            selection,
            reason,
            format!(
                "Lab offload requires reverse runner `{}` to have an active reverse session",
                selection.runner_id
            ),
            "Start the reverse runner session on the Lab machine before using --runner."
                .to_string(),
        );
    }

    eprintln!(
        "Lab offload: direct SSH runner `{}` is not connected; attempting connection.",
        selection.runner_id
    );
    let (report, _) = connect_fn(&selection.runner_id)?;
    if report.connected {
        return Ok(LabRunnerPreparation::Ready);
    }

    let reason = report
        .failure_message
        .unwrap_or_else(|| "runner connection did not become ready".to_string());

    automatic_fallback_or_explicit_error(
        selection,
        reason,
        format!(
            "Lab offload could not connect runner `{}` before execution",
            selection.runner_id
        ),
        format!(
            "Run `homeboy runner connect {}` for full diagnostics.",
            selection.runner_id
        ),
    )
}

fn automatic_fallback_or_explicit_error(
    selection: &LabRunnerSelection,
    reason: String,
    explicit_message: String,
    remediation: String,
) -> Result<LabRunnerPreparation> {
    match selection.source {
        LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
        LabRunnerSelectionSource::Explicit => Err(Error::validation_invalid_argument(
            "runner",
            format!("{explicit_message}: {reason}"),
            Some(selection.runner_id.clone()),
            Some(vec![
                remediation,
                "Use --force-hot to run the command locally instead of offloading.".to_string(),
            ]),
        )),
    }
}

fn connected_runner_not_ready_reason(
    runner_id: &str,
    status: &RunnerStatusReport,
) -> Option<String> {
    let session = status.session.as_ref()?;
    match session.mode {
        RunnerTunnelMode::DirectSsh if session.local_url.as_deref().unwrap_or("").is_empty() => {
            Some(format!(
                "direct SSH runner `{runner_id}` has no local daemon URL; reconnect it with `homeboy runner connect {runner_id}`"
            ))
        }
        RunnerTunnelMode::Reverse if session.broker_url.as_deref().unwrap_or("").is_empty() => {
            Some(format!(
                "reverse-connected runner `{runner_id}` has no broker URL; restart the reverse runner session before retrying"
            ))
        }
        _ => None,
    }
}

fn resolve_lab_runner_selection(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    force_hot: bool,
) -> Result<Option<LabRunnerSelection>> {
    let default_runner = if explicit_runner.is_none() && !force_hot && command.portable {
        super::resolve_default_lab_runner()?
    } else {
        None
    };

    resolve_lab_runner_selection_from_default(command, explicit_runner, force_hot, default_runner)
}

fn resolve_lab_runner_selection_from_default(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    force_hot: bool,
    default_runner: Option<String>,
) -> Result<Option<LabRunnerSelection>> {
    if let Some(runner_id) = explicit_runner {
        if !command.portable {
            let message = command.unsupported_reason.map_or_else(
                || "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, trace, and refactor source runs".to_string(),
                |reason| format!("--runner is unavailable for this hot command. {reason}"),
            );
            return Err(Error::validation_invalid_argument(
                "runner",
                message,
                Some(runner_id.to_string()),
                Some(vec!["Current Lab offload support: audit, bench run, full lint, full test, trace, and refactor source runs.".to_string()]),
            ));
        }

        return Ok(Some(LabRunnerSelection {
            runner_id: runner_id.to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: runner_status_tunnel_mode(runner_id),
        }));
    }

    if force_hot || !command.portable {
        return Ok(None);
    }

    default_runner
        .map(|runner_id| {
            Ok(LabRunnerSelection {
                mode: runner_status_tunnel_mode(&runner_id),
                runner_id,
                source: LabRunnerSelectionSource::Default,
            })
        })
        .transpose()
}

fn runner_status_tunnel_mode(runner_id: &str) -> RunnerTunnelMode {
    status(runner_id).map_or(RunnerTunnelMode::DirectSsh, |status| {
        status_tunnel_mode(&status)
    })
}

fn status_tunnel_mode(status: &RunnerStatusReport) -> RunnerTunnelMode {
    status
        .session
        .as_ref()
        .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone())
}

fn lab_runner_capability_contract(
    command: &LabOffloadCommand,
    source_path: &Path,
    command_prefix_required_tools: &[RunnerRequiredTool],
) -> Option<LabRunnerCapabilityContract> {
    if !command.portable {
        return None;
    }

    let mut required_tools = Vec::new();

    for tool in command_prefix_required_tools {
        push_unique(&mut required_tools, *tool);
    }

    if source_path.join(concat!("package", ".json")).is_file() {
        push_node_package_tool(&mut required_tools, RunnerRequiredTool::Npm);
    }

    if source_path.join("pnpm-lock.yaml").is_file() {
        push_node_package_tool(&mut required_tools, RunnerRequiredTool::Pnpm);
    }

    if source_path.join(concat!("com", "poser", ".json")).is_file() {
        push_unique(&mut required_tools, RunnerRequiredTool::Php);
        push_unique(&mut required_tools, RunnerRequiredTool::Composer);
    }

    if has_docker_signal(source_path) {
        push_unique(&mut required_tools, RunnerRequiredTool::Docker);
    }

    Some(LabRunnerCapabilityContract {
        command: command.hot_label,
        required_tools,
        requires_playwright: command.requires_playwright,
    })
}

fn lab_offload_source_path(args: &[String]) -> Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" {
            let value = iter.next().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "path",
                    "--path requires a value before Lab offload can sync the workspace",
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
    }

    std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("read cwd".to_string())))
}

fn rewrite_lab_offload_args(args: &[String], remote_path: &str) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    let has_force_hot = args.iter().any(|arg| arg == "--force-hot");
    while let Some(arg) = iter.next() {
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--path" {
            stripped.push(arg.clone());
            let _ = iter.next();
            stripped.push(remote_path.to_string());
            continue;
        }
        if arg.starts_with("--path=") {
            stripped.push(format!("--path={remote_path}"));
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--output" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn push_node_package_tool(
    required_tools: &mut Vec<RunnerRequiredTool>,
    package_tool: RunnerRequiredTool,
) {
    push_unique(required_tools, RunnerRequiredTool::Node);
    push_unique(required_tools, package_tool);
}

fn has_docker_signal(source_path: &Path) -> bool {
    [
        "Dockerfile",
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ]
    .iter()
    .any(|name| source_path.join(name).is_file())
}

#[cfg(test)]
mod preparation_tests;

#[cfg(test)]
mod tests {
    use super::super::lab_workspaces::LAB_WORKSPACE_MAPPING_SCHEMA;
    use super::*;
    use crate::core::observation::LAB_OFFLOAD_METADATA_ENV;
    use crate::core::runner::RunnerWorkspaceSyncOutput;

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
    fn rewrites_lab_offload_path_and_strips_runner_and_output_flags() {
        let args = vec![
            "homeboy".to_string(),
            "audit".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--runner".to_string(),
            "lab".to_string(),
            "--json-summary".to_string(),
            "--output".to_string(),
            "/tmp/local.json".to_string(),
            "--runner=other".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--json-summary".to_string(),
            ]
        );
    }

    #[test]
    fn leaves_passthrough_path_args_untouched() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path=/Users/chubes/Developer/project".to_string(),
            "--".to_string(),
            "--path".to_string(),
            "test-fixture".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "test".to_string(),
                "--path=/home/chubes/Developer/project".to_string(),
                "--".to_string(),
                "--path".to_string(),
                "test-fixture".to_string(),
            ]
        );
    }

    #[test]
    fn rewrite_lab_offload_args_does_not_duplicate_force_hot() {
        let args = vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "audit".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
            ]
        );
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
    fn detects_lab_offload_source_path_from_path_flag() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
        ];

        assert_eq!(
            lab_offload_source_path(&args).expect("path"),
            std::path::PathBuf::from("/Users/chubes/Developer/project")
        );
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
