use std::path::Path;

use clap::Args;
use serde::Serialize;

use homeboy::core::build_identity::BuildIdentity;
use homeboy::core::runner_execution_envelope::{
    RunnerExecutionArtifactDeclaration, RunnerExecutionEnvelope, RunnerExecutionLifecyclePolicy,
    RunnerExecutionResultRefs, RUNNER_EXECUTION_ENVELOPE_SCHEMA,
};
use homeboy::core::runners;
use homeboy::core::runners::{RunnerSession, RunnerStaleDaemonWarning, RunnerWorkspaceSyncMode};
use homeboy::core::secret_env_plan::SecretEnvPlan;

#[derive(Args, Debug, Clone)]
pub struct RefreshPlanArgs {
    /// Runner ID that will execute the workload
    #[arg(long)]
    runner: String,

    /// Controller-side workspace or worktree to sync to the runner
    #[arg(long = "workspace")]
    workspace: String,

    /// Runner-side cwd for the eventual runner exec command
    #[arg(long = "runner-cwd")]
    runner_cwd: String,

    /// Stable run id to use for the produced evidence
    #[arg(long = "run-id")]
    run_id: String,

    /// Produced output directory or file. Relative paths are resolved from --runner-cwd.
    #[arg(long = "output", value_name = "PATH")]
    outputs: Vec<String>,

    /// Produced summary directory or file. Relative paths are resolved from --runner-cwd.
    #[arg(long = "summary", value_name = "PATH")]
    summaries: Vec<String>,

    /// Source path that must exist before the refresh is dispatched. Repeat for multiple paths.
    #[arg(long = "source", value_name = "PATH")]
    sources: Vec<String>,

    /// Fixture path that must exist before the refresh is dispatched. Repeat for multiple paths.
    #[arg(long = "fixture", value_name = "PATH")]
    fixtures: Vec<String>,

    /// Runner workspace sync mode to use in the planned sync command.
    #[arg(long = "sync-mode", default_value = "snapshot")]
    sync_mode: String,

    /// Command and arguments to run after the plan checks pass.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LabRefreshPlanOutput {
    pub variant: &'static str,
    pub runner: String,
    pub workspace: String,
    pub runner_cwd: String,
    pub run_id: String,
    pub execution_envelope: RunnerExecutionEnvelope,
    pub handoff: LabRefreshPlanHandoff,
    pub checks: Vec<LabRefreshPlanCheck>,
    pub evidence_paths: Vec<LabRefreshPlanEvidencePath>,
    pub next_commands: Vec<LabRefreshPlanCommand>,
    pub docs: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LabRefreshPlanHandoff {
    pub schema: &'static str,
    pub run_id: String,
    pub handoff_id: String,
    pub workload_id: String,
    pub execution_envelope_ref: LabRefreshPlanExecutionEnvelopeRef,
    pub execution_envelope: RunnerExecutionEnvelope,
    pub runner: LabRefreshPlanRunnerHandoff,
    pub homeboy_provenance: LabHomeboyProvenance,
    pub workspace: LabRefreshPlanWorkspaceHandoff,
    pub env_plan: LabRefreshPlanEnvPlan,
    pub secret_plan: SecretEnvPlan,
    pub runtime_refs: LabRefreshPlanRuntimeRefs,
    pub lifecycle: LabRefreshPlanLifecycle,
    pub artifact: LabRefreshPlanArtifactPlan,
    pub evidence: LabRefreshPlanEvidencePlan,
    pub result: LabRefreshPlanResultPlan,
    pub inspection: LabRefreshPlanInspectionPlan,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanExecutionEnvelopeRef {
    pub schema: &'static str,
    pub envelope_id: String,
    pub source_kind: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanRunnerHandoff {
    pub id: String,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabHomeboyProvenance {
    pub controller_cli: LabHomeboyBinaryIdentity,
    pub runner_configured_binary: LabHomeboyBinaryIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon: Option<LabHomeboyBinaryIdentity>,
    pub controller_plans_lab_payload: bool,
    pub runner_executes_lab_payload: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<LabHomeboyProvenanceDiagnostic>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabHomeboyBinaryIdentity {
    pub role: &'static str,
    pub owner: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    pub purpose: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabHomeboyProvenanceDiagnostic {
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
    pub action: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanWorkspaceHandoff {
    pub controller_path: String,
    pub runner_cwd: String,
    pub sync_mode: String,
    pub mapping: LabRefreshPlanWorkspaceMapping,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanWorkspaceMapping {
    pub schema: &'static str,
    pub ref_id: String,
    pub controller_path: String,
    pub runner_path: String,
    pub sync_mode: String,
    pub lease: LabRefreshPlanWorkspaceLease,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanWorkspaceLease {
    pub lease_id: String,
    pub owner_run_id: String,
    pub cleanup_policy: &'static str,
    pub ttl: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanEnvPlan {
    pub vars: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanRuntimeRefs {
    pub command: Vec<String>,
    pub docs: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanLifecycle {
    pub status: &'static str,
    pub next: Vec<&'static str>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanArtifactPlan {
    pub paths: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanEvidencePlan {
    pub paths: Vec<LabRefreshPlanEvidencePath>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanResultPlan {
    pub run_id: String,
    pub status: &'static str,
    pub refs: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanInspectionPlan {
    pub commands: Vec<LabRefreshPlanCommand>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanCheck {
    pub name: String,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanEvidencePath {
    pub kind: &'static str,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanCommand {
    pub label: &'static str,
    pub command: String,
    pub purpose: &'static str,
}

pub fn refresh_plan(args: RefreshPlanArgs) -> homeboy::core::Result<LabRefreshPlanOutput> {
    validate_sync_mode(&args.sync_mode)?;

    if args.command.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "refresh-plan requires a command after --",
            None,
            Some(vec![
                "Example: homeboy runner refresh-plan --runner lab --workspace . --runner-cwd /workspace/app --run-id run-1 --output artifacts/review -- npm test".to_string(),
            ]),
        ));
    }

    let mut checks = Vec::new();
    let homeboy_provenance = add_runner_check(&mut checks, &args.runner)?;
    add_path_check(&mut checks, "workspace", &args.workspace)?;
    for source in &args.sources {
        add_path_check(&mut checks, "source", source)?;
    }
    for fixture in &args.fixtures {
        add_path_check(&mut checks, "fixture", fixture)?;
    }

    let evidence_paths = evidence_paths(&args);
    let next_commands = next_commands(&args, &evidence_paths);
    let docs = vec![
        "docs/operations/artifact-loop-runner-matrix.md".to_string(),
        "docs/commands/runner.md".to_string(),
    ];
    let execution_envelope =
        execution_envelope_plan(&args, &evidence_paths, &docs, &homeboy_provenance);
    let handoff = handoff_plan(
        &args,
        &execution_envelope,
        &evidence_paths,
        &next_commands,
        &docs,
        homeboy_provenance,
    );

    Ok(LabRefreshPlanOutput {
        variant: "refresh_plan",
        runner: args.runner,
        workspace: args.workspace,
        runner_cwd: args.runner_cwd,
        run_id: args.run_id,
        execution_envelope,
        handoff,
        checks,
        evidence_paths,
        next_commands,
        docs,
    })
}

fn validate_sync_mode(sync_mode: &str) -> homeboy::core::Result<RunnerWorkspaceSyncMode> {
    match sync_mode {
        "snapshot" => Ok(RunnerWorkspaceSyncMode::Snapshot),
        "snapshot-git" => Ok(RunnerWorkspaceSyncMode::SnapshotGit),
        "git" => Ok(RunnerWorkspaceSyncMode::Git),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "sync_mode",
            format!("unsupported sync mode: {sync_mode}"),
            None,
            Some(vec!["Use one of: snapshot, snapshot-git, git".to_string()]),
        )),
    }
}

fn add_runner_check(
    checks: &mut Vec<LabRefreshPlanCheck>,
    runner_id: &str,
) -> homeboy::core::Result<LabHomeboyProvenance> {
    let runner = runners::load(runner_id)?;
    let configured_homeboy = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let workspace_root = runner
        .workspace_root
        .as_deref()
        .unwrap_or("runner has no default workspace_root");
    let status = runners::status(runner_id);
    let active_daemon_status_error = match &status {
        Ok(status) if status.session.is_none() => {
            Some("runner is not connected to an active daemon".to_string())
        }
        Err(error) => Some(error.to_string()),
        Ok(_) => None,
    };
    let homeboy_provenance = lab_homeboy_provenance(
        runner_id,
        Some(configured_homeboy.to_string()),
        status
            .as_ref()
            .ok()
            .and_then(|status| status.session.as_ref()),
        status
            .as_ref()
            .ok()
            .and_then(|status| status.stale_daemon.as_ref()),
        active_daemon_status_error,
    );

    checks.push(LabRefreshPlanCheck {
        name: "runner".to_string(),
        status: "ok",
        detail: format!(
            "configured runner `{runner_id}` uses Homeboy `{configured_homeboy}` with workspace root `{workspace_root}`"
        ),
    });
    checks.push(LabRefreshPlanCheck {
        name: "homeboy_provenance".to_string(),
        status: if homeboy_provenance
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
        {
            "error"
        } else if homeboy_provenance.diagnostics.is_empty() {
            "ok"
        } else {
            "warn"
        },
        detail: lab_homeboy_provenance_detail(&homeboy_provenance),
    });
    checks.push(LabRefreshPlanCheck {
        name: "runner_homeboy_capability".to_string(),
        status: "planned",
        detail: format!(
            "verify with `{}` before dispatching the refresh workload",
            shell_join(&[
                "homeboy",
                "runner",
                "doctor",
                runner_id,
                "--scope",
                "lab-offload",
            ])
        ),
    });

    Ok(homeboy_provenance)
}

fn lab_homeboy_provenance(
    runner_id: &str,
    configured_binary_path: Option<String>,
    active_daemon: Option<&RunnerSession>,
    stale_daemon: Option<&RunnerStaleDaemonWarning>,
    status_error: Option<String>,
) -> LabHomeboyProvenance {
    let controller_identity = homeboy::core::build_identity::current();
    lab_homeboy_provenance_from_parts(
        runner_id,
        controller_identity,
        std::env::current_exe()
            .ok()
            .map(|path| path.to_string_lossy().to_string()),
        configured_binary_path,
        active_daemon.map(active_daemon_identity),
        stale_daemon,
        status_error,
    )
}

fn lab_homeboy_provenance_from_parts(
    runner_id: &str,
    controller_identity: BuildIdentity,
    controller_path: Option<String>,
    configured_binary_path: Option<String>,
    active_daemon: Option<LabHomeboyBinaryIdentity>,
    stale_daemon: Option<&RunnerStaleDaemonWarning>,
    status_error: Option<String>,
) -> LabHomeboyProvenance {
    let controller_cli = LabHomeboyBinaryIdentity {
        role: "controller_cli",
        owner: "controller",
        path: controller_path,
        version: Some(controller_identity.version.clone()),
        build_identity: Some(controller_identity.display.clone()),
        dirty: controller_identity.git_dirty,
        purpose: "plans Lab argv, workspace remapping, handoff metadata, and evidence paths before runner dispatch",
    };
    let runner_configured_binary = LabHomeboyBinaryIdentity {
        role: "runner_configured_binary",
        owner: "runner",
        path: configured_binary_path,
        version: None,
        build_identity: None,
        dirty: None,
        purpose: "configured runner executable used for runner jobs and daemon refresh operations",
    };

    let mut diagnostics = Vec::new();
    if controller_identity.git_dirty == Some(true) {
        diagnostics.push(LabHomeboyProvenanceDiagnostic {
            severity: "warn",
            code: "controller_dirty",
            message: format!(
                "controller CLI is {}; this binary prepares the Lab argv/remap payload before runner dispatch",
                controller_identity.display
            ),
            action: "rebuild or select a clean controller Homeboy binary before treating runner refresh results as proof".to_string(),
        });
    }

    if let Some(active_daemon) = active_daemon.as_ref() {
        if active_daemon.version != Some(controller_identity.version.clone()) {
            diagnostics.push(LabHomeboyProvenanceDiagnostic {
                severity: "warn",
                code: "controller_daemon_version_drift",
                message: format!(
                    "controller CLI {} prepares the Lab payload, while runner `{runner_id}` active daemon is {} and executes the runner job",
                    controller_identity.display,
                    active_daemon
                        .build_identity
                        .as_deref()
                        .or(active_daemon.version.as_deref())
                        .unwrap_or("unknown")
                ),
                action: "rebuild the controller binary or refresh/reconnect the runner so both phases report the intended Homeboy identity".to_string(),
            });
        } else if active_daemon
            .build_identity
            .as_ref()
            .is_some_and(|identity| identity != &controller_identity.display)
        {
            diagnostics.push(LabHomeboyProvenanceDiagnostic {
                severity: "warn",
                code: "controller_daemon_build_drift",
                message: format!(
                    "controller CLI {} and runner `{runner_id}` active daemon {} report the same version but different build identities",
                    controller_identity.display,
                    active_daemon
                        .build_identity
                        .as_deref()
                        .unwrap_or("unknown")
                ),
                action: "rebuild the controller binary or refresh/reconnect the runner before relying on commit-specific Lab behavior".to_string(),
            });
        }
    } else if let Some(error) = status_error {
        diagnostics.push(LabHomeboyProvenanceDiagnostic {
            severity: "warn",
            code: "active_daemon_unknown",
            message: format!(
                "runner `{runner_id}` active daemon identity could not be read: {error}"
            ),
            action: format!(
                "run `{}` to verify or reconnect the runner daemon",
                shell_join(&[
                    "homeboy",
                    "runner",
                    "doctor",
                    runner_id,
                    "--scope",
                    "lab-offload"
                ])
            ),
        });
    }

    if let Some(stale_daemon) = stale_daemon {
        diagnostics.push(LabHomeboyProvenanceDiagnostic {
            severity: stale_daemon.severity,
            code: "runner_stale_daemon",
            message: stale_daemon.message.clone(),
            action: stale_daemon.refresh_command.clone(),
        });
    }

    LabHomeboyProvenance {
        controller_cli,
        runner_configured_binary,
        active_daemon,
        controller_plans_lab_payload: true,
        runner_executes_lab_payload: true,
        diagnostics,
    }
}

fn active_daemon_identity(session: &RunnerSession) -> LabHomeboyBinaryIdentity {
    LabHomeboyBinaryIdentity {
        role: "active_daemon",
        owner: "runner",
        path: session.remote_daemon_address.clone().or(session.local_url.clone()),
        version: Some(session.homeboy_version.clone()),
        build_identity: session.homeboy_build_identity.clone(),
        dirty: None,
        purpose: "active daemon control plane that accepts runner jobs and reports runner-side execution state",
    }
}

fn lab_homeboy_provenance_detail(provenance: &LabHomeboyProvenance) -> String {
    let daemon = provenance
        .active_daemon
        .as_ref()
        .and_then(|identity| {
            identity
                .build_identity
                .as_deref()
                .or(identity.version.as_deref())
        })
        .unwrap_or("active daemon unknown");
    let configured = provenance
        .runner_configured_binary
        .path
        .as_deref()
        .unwrap_or("homeboy");
    let diagnostic_count = provenance.diagnostics.len();
    format!(
        "controller `{}` plans Lab payload; runner configured binary `{configured}` and active daemon `{daemon}` execute runner phase; diagnostics={diagnostic_count}",
        provenance
            .controller_cli
            .build_identity
            .as_deref()
            .unwrap_or("unknown")
    )
}

fn add_path_check(
    checks: &mut Vec<LabRefreshPlanCheck>,
    label: &str,
    path: &str,
) -> homeboy::core::Result<()> {
    if !Path::new(path).exists() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            label,
            format!("{label} path does not exist: {path}"),
            None,
            None,
        ));
    }

    checks.push(LabRefreshPlanCheck {
        name: label.to_string(),
        status: "ok",
        detail: path.to_string(),
    });
    Ok(())
}

fn evidence_paths(args: &RefreshPlanArgs) -> Vec<LabRefreshPlanEvidencePath> {
    args.outputs
        .iter()
        .map(|path| LabRefreshPlanEvidencePath {
            kind: "artifact",
            path: path.clone(),
        })
        .chain(
            args.summaries
                .iter()
                .map(|path| LabRefreshPlanEvidencePath {
                    kind: "summary",
                    path: path.clone(),
                }),
        )
        .collect()
}

fn handoff_plan(
    args: &RefreshPlanArgs,
    execution_envelope: &RunnerExecutionEnvelope,
    evidence_paths: &[LabRefreshPlanEvidencePath],
    next_commands: &[LabRefreshPlanCommand],
    docs: &[String],
    homeboy_provenance: LabHomeboyProvenance,
) -> LabRefreshPlanHandoff {
    let artifact_paths = evidence_paths
        .iter()
        .filter(|path| path.kind == "artifact")
        .map(|path| path.path.clone())
        .collect();
    let inspection_commands = next_commands
        .iter()
        .filter(|command| matches!(command.label, "inspect-artifacts" | "inspect-evidence"))
        .cloned()
        .collect();
    let workspace_mapping = workspace_mapping(args);

    LabRefreshPlanHandoff {
        schema: "homeboy/lab-refresh-handoff/v1",
        run_id: args.run_id.clone(),
        handoff_id: execution_envelope.envelope_id.clone(),
        workload_id: args.run_id.clone(),
        execution_envelope_ref: LabRefreshPlanExecutionEnvelopeRef {
            schema: RUNNER_EXECUTION_ENVELOPE_SCHEMA,
            envelope_id: execution_envelope.envelope_id.clone(),
            source_kind: execution_envelope.source.kind.clone(),
        },
        execution_envelope: redacted_execution_envelope(execution_envelope),
        runner: LabRefreshPlanRunnerHandoff {
            id: args.runner.clone(),
            mode: None,
        },
        homeboy_provenance,
        workspace: LabRefreshPlanWorkspaceHandoff {
            controller_path: args.workspace.clone(),
            runner_cwd: args.runner_cwd.clone(),
            sync_mode: args.sync_mode.clone(),
            mapping: workspace_mapping,
        },
        env_plan: LabRefreshPlanEnvPlan {
            vars: execution_envelope_env_vars(execution_envelope),
            unknown: false,
        },
        secret_plan: execution_envelope_secret_plan(execution_envelope),
        runtime_refs: LabRefreshPlanRuntimeRefs {
            command: args.command.clone(),
            docs: docs.to_vec(),
            unknown: false,
        },
        lifecycle: LabRefreshPlanLifecycle {
            status: "planned",
            next: vec![
                "verify_runner",
                "sync_workspace",
                "run_refresh",
                "inspect_evidence",
            ],
        },
        artifact: LabRefreshPlanArtifactPlan {
            paths: artifact_paths,
            unknown: false,
        },
        evidence: LabRefreshPlanEvidencePlan {
            paths: evidence_paths.to_vec(),
            unknown: false,
        },
        result: LabRefreshPlanResultPlan {
            run_id: args.run_id.clone(),
            status: "planned",
            refs: Vec::new(),
        },
        inspection: LabRefreshPlanInspectionPlan {
            commands: inspection_commands,
            unknown: false,
        },
    }
}

fn workspace_mapping(args: &RefreshPlanArgs) -> LabRefreshPlanWorkspaceMapping {
    let ref_id = workspace_mapping_ref(&args.runner, &args.run_id);
    LabRefreshPlanWorkspaceMapping {
        schema: "homeboy/runner-workspace-mapping/v1",
        ref_id: ref_id.clone(),
        controller_path: args.workspace.clone(),
        runner_path: args.runner_cwd.clone(),
        sync_mode: args.sync_mode.clone(),
        lease: LabRefreshPlanWorkspaceLease {
            lease_id: format!("{ref_id}:lease"),
            owner_run_id: args.run_id.clone(),
            cleanup_policy: "operator_retains_until_evidence_verified",
            ttl: None,
        },
    }
}

fn workspace_mapping_ref(runner: &str, run_id: &str) -> String {
    format!("runner:{runner}:workspace:{run_id}")
}

fn execution_envelope_plan(
    args: &RefreshPlanArgs,
    evidence_paths: &[LabRefreshPlanEvidencePath],
    docs: &[String],
    homeboy_provenance: &LabHomeboyProvenance,
) -> RunnerExecutionEnvelope {
    let handoff_id = format!("lab-refresh:{}:{}", args.runner, args.run_id);
    let mapping = workspace_mapping(args);

    RunnerExecutionEnvelope::planned(&handoff_id, "lab_refresh_plan")
        .with_source_ref(&args.run_id)
        .with_secret_env(SecretEnvPlan::default())
        .with_lifecycle_policy(RunnerExecutionLifecyclePolicy {
            cleanup: Some("operator_retains_runner_workspace_until_evidence_verified".to_string()),
            retry: Some("rerun_refresh_plan_then_runner_exec".to_string()),
            gates: vec![
                "runner_doctor_lab_offload".to_string(),
                "workspace_sync".to_string(),
                "evidence_inspection".to_string(),
            ],
        })
        .with_artifact_declarations(artifact_declarations(evidence_paths))
        .with_result_refs(RunnerExecutionResultRefs {
            plan_id: Some(handoff_id),
            run_id: Some(args.run_id.clone()),
            ..RunnerExecutionResultRefs::default()
        })
        .with_metadata(serde_json::json!({
            "runner": {
                "id": args.runner,
            },
            "workspace": {
                "controller_path": args.workspace,
                "runner_cwd": args.runner_cwd,
                "sync_mode": args.sync_mode,
                "mapping": mapping,
            },
            "runtime": {
                "command": args.command,
            },
            "homeboy_provenance": homeboy_provenance,
            "docs": docs,
        }))
}

fn redacted_execution_envelope(envelope: &RunnerExecutionEnvelope) -> RunnerExecutionEnvelope {
    let mut redacted = envelope.clone();
    redacted.secret_env = redacted.secret_env.map(|secret_env| secret_env.redacted());
    redacted
}

fn execution_envelope_env_vars(envelope: &RunnerExecutionEnvelope) -> Vec<String> {
    envelope
        .secret_env
        .as_ref()
        .map(|secret_env| secret_env.public_env.keys().cloned().collect())
        .unwrap_or_default()
}

fn execution_envelope_secret_plan(envelope: &RunnerExecutionEnvelope) -> SecretEnvPlan {
    envelope
        .secret_env
        .as_ref()
        .map(SecretEnvPlan::redacted)
        .unwrap_or_default()
}

fn artifact_declarations(
    evidence_paths: &[LabRefreshPlanEvidencePath],
) -> Vec<RunnerExecutionArtifactDeclaration> {
    evidence_paths
        .iter()
        .enumerate()
        .map(
            |(index, evidence_path)| RunnerExecutionArtifactDeclaration {
                name: artifact_name(evidence_path, index),
                artifact_type: Some(evidence_path.kind.to_string()),
                artifact_schema: None,
                path: Some(evidence_path.path.clone()),
                required: true,
                description: Some(format!(
                    "{} produced by the lab refresh workload",
                    evidence_path.kind
                )),
                metadata: serde_json::Value::Null,
            },
        )
        .collect()
}

fn artifact_name(evidence_path: &LabRefreshPlanEvidencePath, index: usize) -> String {
    Path::new(&evidence_path.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}-{}", evidence_path.kind, index + 1))
}

fn next_commands(
    args: &RefreshPlanArgs,
    evidence_paths: &[LabRefreshPlanEvidencePath],
) -> Vec<LabRefreshPlanCommand> {
    let mut runner_exec = vec![
        "homeboy".to_string(),
        "runner".to_string(),
        "exec".to_string(),
        args.runner.clone(),
        "--cwd".to_string(),
        args.runner_cwd.clone(),
        "--run-id".to_string(),
        args.run_id.clone(),
    ];
    for evidence_path in evidence_paths {
        match evidence_path.kind {
            "artifact" => runner_exec.push("--artifact".to_string()),
            "summary" => runner_exec.push("--summary".to_string()),
            _ => continue,
        }
        runner_exec.push(evidence_path.path.clone());
    }
    runner_exec.push("--".to_string());
    runner_exec.extend(args.command.clone());

    vec![
        LabRefreshPlanCommand {
            label: "verify-runner",
            command: shell_join(&[
                "homeboy",
                "runner",
                "doctor",
                &args.runner,
                "--scope",
                "lab-offload",
            ]),
            purpose: "verify runner Homeboy binary, daemon, and Lab offload capability",
        },
        LabRefreshPlanCommand {
            label: "sync-workspace",
            command: shell_join(&[
                "homeboy",
                "runner",
                "workspace",
                "sync",
                &args.runner,
                "--path",
                &args.workspace,
                "--mode",
                &args.sync_mode,
            ]),
            purpose: "materialize the fresh controller workspace on the runner",
        },
        LabRefreshPlanCommand {
            label: "run-refresh",
            command: shell_join_owned(&runner_exec),
            purpose: "execute the workload and declare produced evidence paths",
        },
        LabRefreshPlanCommand {
            label: "inspect-artifacts",
            command: shell_join(&["homeboy", "runs", "artifacts", &args.run_id]),
            purpose: "confirm the produced files are attached to the persisted run",
        },
        LabRefreshPlanCommand {
            label: "inspect-evidence",
            command: shell_join(&["homeboy", "runs", "evidence", &args.run_id]),
            purpose: "get reviewer-facing artifact refs or fetch commands",
        },
    ]
}

fn shell_join(args: &[&str]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_join_owned(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA;

    fn clean_controller_identity() -> BuildIdentity {
        BuildIdentity {
            version: "0.265.0".to_string(),
            git_commit: Some("controller123".to_string()),
            git_dirty: Some(false),
            display: "homeboy 0.265.0+controller123".to_string(),
        }
    }

    fn test_provenance() -> LabHomeboyProvenance {
        lab_homeboy_provenance_from_parts(
            "lab-runner",
            clean_controller_identity(),
            Some("/controller/homeboy".to_string()),
            Some("/runner/homeboy".to_string()),
            Some(LabHomeboyBinaryIdentity {
                role: "active_daemon",
                owner: "runner",
                path: Some("http://127.0.0.1:1234".to_string()),
                version: Some("0.265.0".to_string()),
                build_identity: Some("homeboy 0.265.0+controller123".to_string()),
                dirty: None,
                purpose: "test daemon",
            }),
            None,
            None,
        )
    }

    #[test]
    fn plan_commands_include_existing_runner_artifact_primitives() {
        let args = RefreshPlanArgs {
            runner: "lab-runner".to_string(),
            workspace: "/workspace/source".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: vec!["artifacts/matrix".to_string()],
            summaries: vec!["artifacts/matrix/matrix-summary.json".to_string()],
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "snapshot".to_string(),
            command: vec![
                "sh".to_string(),
                "-lc".to_string(),
                "./run matrix".to_string(),
            ],
        };
        let evidence = evidence_paths(&args);
        let commands = next_commands(&args, &evidence);

        assert_eq!(commands[0].label, "verify-runner");
        assert_eq!(
            commands[1].command,
            "homeboy runner workspace sync lab-runner --path /workspace/source --mode snapshot"
        );
        assert_eq!(commands[2].label, "run-refresh");
        assert!(commands[2].command.contains(
            "--artifact artifacts/matrix --summary artifacts/matrix/matrix-summary.json"
        ));
        assert!(commands[2].command.contains("-- sh -lc './run matrix'"));
        assert_eq!(
            commands[3].command,
            "homeboy runs artifacts matrix-refresh-1"
        );
        assert_eq!(
            commands[4].command,
            "homeboy runs evidence matrix-refresh-1"
        );
    }

    #[test]
    fn handoff_plan_exposes_typed_generic_fields() {
        let args = RefreshPlanArgs {
            runner: "lab-runner".to_string(),
            workspace: "/workspace/source".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: vec!["artifacts/matrix".to_string()],
            summaries: vec!["artifacts/matrix/matrix-summary.json".to_string()],
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "snapshot-git".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
        };
        let evidence = evidence_paths(&args);
        let commands = next_commands(&args, &evidence);
        let docs = vec!["docs/commands/runner.md".to_string()];
        let provenance = test_provenance();
        let envelope = execution_envelope_plan(&args, &evidence, &docs, &provenance);

        let handoff = handoff_plan(&args, &envelope, &evidence, &commands, &docs, provenance);

        assert_eq!(handoff.schema, "homeboy/lab-refresh-handoff/v1");
        assert_eq!(handoff.run_id, "matrix-refresh-1");
        assert_eq!(
            handoff.handoff_id,
            "lab-refresh:lab-runner:matrix-refresh-1"
        );
        assert_eq!(handoff.workload_id, "matrix-refresh-1");
        assert_eq!(
            handoff.execution_envelope_ref.schema,
            RUNNER_EXECUTION_ENVELOPE_SCHEMA
        );
        assert_eq!(
            handoff.execution_envelope_ref.envelope_id,
            "lab-refresh:lab-runner:matrix-refresh-1"
        );
        assert_eq!(
            handoff.execution_envelope_ref.source_kind,
            "lab_refresh_plan"
        );
        assert_eq!(
            handoff.execution_envelope.schema,
            RUNNER_EXECUTION_ENVELOPE_SCHEMA
        );
        assert_eq!(
            handoff.execution_envelope.result_refs.run_id.as_deref(),
            Some("matrix-refresh-1")
        );
        assert_eq!(handoff.runner.id, "lab-runner");
        assert_eq!(handoff.runner.mode, None);
        assert_eq!(
            handoff.homeboy_provenance.controller_cli.role,
            "controller_cli"
        );
        assert_eq!(
            handoff
                .homeboy_provenance
                .runner_configured_binary
                .path
                .as_deref(),
            Some("/runner/homeboy")
        );
        assert_eq!(
            handoff
                .homeboy_provenance
                .active_daemon
                .as_ref()
                .and_then(|identity| identity.build_identity.as_deref()),
            Some("homeboy 0.265.0+controller123")
        );
        assert!(handoff.homeboy_provenance.diagnostics.is_empty());
        assert_eq!(handoff.workspace.controller_path, "/workspace/source");
        assert_eq!(handoff.workspace.runner_cwd, "/runner/source");
        assert_eq!(handoff.workspace.sync_mode, "snapshot-git");
        assert_eq!(
            handoff.workspace.mapping.ref_id,
            "runner:lab-runner:workspace:matrix-refresh-1"
        );
        assert_eq!(
            handoff.workspace.mapping.lease.cleanup_policy,
            "operator_retains_until_evidence_verified"
        );
        assert_eq!(handoff.workspace.mapping.lease.ttl, None);
        assert_eq!(handoff.env_plan.vars, Vec::<String>::new());
        assert!(!handoff.env_plan.unknown);
        assert_eq!(handoff.secret_plan.schema, SECRET_ENV_PLAN_SCHEMA);
        assert_eq!(handoff.secret_plan.secret_env_names(), Vec::<String>::new());
        assert_eq!(handoff.runtime_refs.command, vec!["cargo", "test"]);
        assert_eq!(handoff.artifact.paths, vec!["artifacts/matrix"]);
        assert_eq!(handoff.evidence.paths, evidence);
        assert_eq!(handoff.result.status, "planned");
        assert_eq!(handoff.inspection.commands.len(), 2);
    }

    #[test]
    fn refresh_plan_compiles_canonical_execution_envelope() {
        let args = RefreshPlanArgs {
            runner: "lab-runner".to_string(),
            workspace: "/workspace/source".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: vec!["artifacts/matrix".to_string()],
            summaries: vec!["artifacts/matrix/matrix-summary.json".to_string()],
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "snapshot".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
        };
        let evidence = evidence_paths(&args);
        let docs = vec!["docs/commands/runner.md".to_string()];
        let provenance = test_provenance();

        let envelope = execution_envelope_plan(&args, &evidence, &docs, &provenance);

        assert_eq!(envelope.schema, RUNNER_EXECUTION_ENVELOPE_SCHEMA);
        assert_eq!(
            envelope.envelope_id,
            "lab-refresh:lab-runner:matrix-refresh-1"
        );
        assert_eq!(envelope.source.kind, "lab_refresh_plan");
        assert_eq!(envelope.source.ref_id.as_deref(), Some("matrix-refresh-1"));
        assert_eq!(
            envelope.result_refs.plan_id.as_deref(),
            Some("lab-refresh:lab-runner:matrix-refresh-1")
        );
        assert_eq!(
            envelope.result_refs.run_id.as_deref(),
            Some("matrix-refresh-1")
        );
        assert_eq!(
            envelope.lifecycle_policy.cleanup.as_deref(),
            Some("operator_retains_runner_workspace_until_evidence_verified")
        );
        assert_eq!(
            envelope
                .secret_env
                .expect("secret env plan")
                .secret_env_names(),
            Vec::<String>::new()
        );
        assert_eq!(
            envelope
                .artifact_declarations
                .iter()
                .map(|artifact| (
                    artifact.name.as_str(),
                    artifact.artifact_type.as_deref(),
                    artifact.path.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("matrix", Some("artifact"), Some("artifacts/matrix")),
                (
                    "matrix-summary.json",
                    Some("summary"),
                    Some("artifacts/matrix/matrix-summary.json")
                ),
            ]
        );
        assert_eq!(envelope.metadata["runner"]["id"], "lab-runner");
        assert_eq!(
            envelope.metadata["workspace"]["mapping"]["ref_id"],
            "runner:lab-runner:workspace:matrix-refresh-1"
        );
        assert_eq!(
            envelope.metadata["workspace"]["mapping"]["lease"]["owner_run_id"],
            "matrix-refresh-1"
        );
        assert_eq!(
            envelope.metadata["runtime"]["command"],
            serde_json::json!(["cargo", "test"])
        );
        assert_eq!(
            envelope.metadata["homeboy_provenance"]["controller_cli"]["role"],
            "controller_cli"
        );
        assert_eq!(
            envelope.metadata["homeboy_provenance"]["active_daemon"]["build_identity"],
            "homeboy 0.265.0+controller123"
        );
    }

    #[test]
    fn homeboy_provenance_warns_for_dirty_controller_and_daemon_drift() {
        let controller_identity = BuildIdentity {
            version: "0.265.0".to_string(),
            git_commit: Some("controller123".to_string()),
            git_dirty: Some(true),
            display: "homeboy 0.265.0+controller123-dirty".to_string(),
        };

        let provenance = lab_homeboy_provenance_from_parts(
            "lab-runner",
            controller_identity,
            Some("/controller/homeboy".to_string()),
            Some("/runner/homeboy".to_string()),
            Some(LabHomeboyBinaryIdentity {
                role: "active_daemon",
                owner: "runner",
                path: None,
                version: Some("0.264.1".to_string()),
                build_identity: Some("homeboy 0.264.1+runner456".to_string()),
                dirty: None,
                purpose: "test daemon",
            }),
            None,
            None,
        );

        let codes = provenance
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            vec!["controller_dirty", "controller_daemon_version_drift"]
        );
        assert!(provenance.diagnostics[0]
            .message
            .contains("prepares the Lab argv/remap payload"));
        assert!(provenance.diagnostics[1]
            .message
            .contains("prepares the Lab payload"));
        assert!(provenance.diagnostics[1]
            .message
            .contains("executes the runner job"));
        assert!(provenance.diagnostics[1]
            .action
            .contains("refresh/reconnect the runner"));
    }

    #[test]
    fn homeboy_provenance_reports_unknown_active_daemon_with_action() {
        let provenance = lab_homeboy_provenance_from_parts(
            "lab-runner",
            clean_controller_identity(),
            Some("/controller/homeboy".to_string()),
            Some("/runner/homeboy".to_string()),
            None,
            None,
            Some("connection refused".to_string()),
        );

        assert_eq!(provenance.diagnostics.len(), 1);
        assert_eq!(provenance.diagnostics[0].code, "active_daemon_unknown");
        assert!(provenance.diagnostics[0]
            .message
            .contains("connection refused"));
        assert!(provenance.diagnostics[0]
            .action
            .contains("homeboy runner doctor lab-runner --scope lab-offload"));
    }

    #[test]
    fn refresh_plan_secret_plan_uses_redacted_canonical_shape() {
        let secret_plan =
            SecretEnvPlan::from_secret_env_names(["B_SECRET".to_string(), "A_SECRET".to_string()]);

        let redacted = serde_json::to_value(secret_plan.redacted()).expect("redacted json");
        let rendered = serde_json::to_string(&redacted).expect("redacted json string");

        assert_eq!(redacted["schema"], SECRET_ENV_PLAN_SCHEMA);
        assert_eq!(
            redacted["secret_env_names"],
            serde_json::json!(["A_SECRET", "B_SECRET"])
        );
        assert!(!rendered.contains("super-secret-value"));
        assert_eq!(secret_plan.redacted_env()["A_SECRET"], "[REDACTED]");
    }

    #[test]
    fn invalid_sync_mode_is_rejected() {
        let args = RefreshPlanArgs {
            runner: "missing-runner".to_string(),
            workspace: "/missing/workspace".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: Vec::new(),
            summaries: Vec::new(),
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "rsync".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
        };

        let err = refresh_plan(args).expect_err("sync mode should be validated");

        let message = err.to_string();
        assert!(message.contains("unsupported sync mode: rsync"));
    }

    #[test]
    fn shell_quote_handles_spaces_and_single_quotes() {
        assert_eq!(shell_quote("simple/path"), "simple/path");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's ok"), "'it'\\''s ok'");
    }
}
