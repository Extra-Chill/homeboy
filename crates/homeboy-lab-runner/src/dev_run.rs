use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    RunnerCapabilityPreflight, RunnerExecOptions, RunnerExecOutput, RunnerWorkspaceSyncMode,
    RunnerWorkspaceSyncOptions, RunnerWorkspaceSyncOutput,
};
use homeboy_core::error::{Error, Result};
use homeboy_core::lab_contract::{
    LabRunnerWorkload, LabRunnerWorkloadArtifactRef, LabRunnerWorkloadAssignment,
    LabRunnerWorkloadCommandFamily, LabRunnerWorkloadKind, LabRunnerWorkloadMutationPolicy,
    LabRunnerWorkloadResultRefs, LabRunnerWorkloadSecrets, LabRunnerWorkloadState,
    LabRunnerWorkloadWorkspaceMappings, LAB_RUNNER_WORKLOAD_SCHEMA,
};
use homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy;
use homeboy_core::runner_execution_envelope::RunnerExecutionProjection;
use homeboy_core::source_snapshot::SourceSnapshot;
use homeboy_lab_runner_contract::RunnerWorkspaceLease;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionDevRunPlan {
    pub extension_id: String,
    pub runner_id: String,
    pub source: String,
    pub sync_mode: RunnerWorkspaceSyncMode,
    pub install_command: Vec<String>,
    pub command: Vec<String>,
    pub persistent_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtensionDevRunSourceMaterialization {
    sync_source: String,
    remote_install_suffix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionDevRunOutput {
    pub extension_id: String,
    pub runner_id: String,
    pub source: String,
    pub remote_source: String,
    pub plan: ExtensionDevRunPlan,
    pub sync: RunnerWorkspaceSyncOutput,
    pub prior_extension_state: RunnerExtensionStateProbe,
    pub install: RunnerExecOutput,
    pub install_outcome: ExtensionDevRunExecutionOutcome,
    pub command: RunnerExecOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_outcome: Option<ExtensionDevRunExecutionOutcome>,
    pub provenance: ExtensionDevRunProvenance,
    pub lifecycle: ExtensionDevRunOverlayLifecycle,
    pub persistent_state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionDevRunOverlayLifecycle {
    pub overlay_id: String,
    pub source_revision: ExtensionDevRunSourceRevision,
    pub install_mode: String,
    pub cleanup_policy: ResourceCleanupPolicy,
    pub ttl: Option<String>,
    pub retain_policy: String,
    pub revert_policy: String,
    pub lease: RunnerWorkspaceLease,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionDevRunSourceRevision {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    pub source_dirty: bool,
    pub snapshot_identity: String,
    pub snapshot_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionDevRunExecutionOutcome {
    #[serde(rename = "runner_workload")]
    pub lab_runner_workload: LabRunnerWorkload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_record: Option<RunnerExecutionProjection>,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerExtensionStateProbe {
    pub command: Vec<String>,
    pub captured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtensionDevRunProvenance {
    pub extension_id: String,
    pub runner_id: String,
    pub local_source: String,
    pub remote_source: String,
    pub sync_mode: String,
    pub snapshot_identity: String,
    pub source_revision: ExtensionDevRunSourceRevision,
    pub install_mode: String,
    pub cleanup_policy: ResourceCleanupPolicy,
    pub ttl: Option<String>,
    pub install_command: Vec<String>,
    pub command: Vec<String>,
}

pub fn plan_extension_dev_run(
    extension_id: &str,
    runner_id: &str,
    source: &str,
    command: &[String],
) -> Result<ExtensionDevRunPlan> {
    let extension_id = required_arg("extension_id", extension_id)?;
    let runner_id = required_arg("runner", runner_id)?;
    let source = required_arg("source", source)?;
    if command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "extension dev-run requires a command after --",
            None,
            Some(vec![
                "Example: homeboy extension dev-run my-extension --source . --runner lab -- homeboy extension run my-extension".to_string(),
            ]),
        ));
    }

    Ok(ExtensionDevRunPlan {
        extension_id: extension_id.clone(),
        runner_id: runner_id.clone(),
        source,
        sync_mode: RunnerWorkspaceSyncMode::Snapshot,
        install_command: Vec::new(),
        command: command.to_vec(),
        persistent_state: "runner extension is left refreshed/linked to the synced source path"
            .to_string(),
    })
}

pub fn run_extension_dev_run(
    extension_id: &str,
    runner_id: &str,
    source: &str,
    command: &[String],
) -> Result<(ExtensionDevRunOutput, i32)> {
    run_extension_dev_run_with(
        extension_id,
        runner_id,
        source,
        command,
        |runner_id, options| crate::runners::sync_workspace(runner_id, options),
        |runner_id, options| crate::runners::exec(runner_id, options),
    )
}

pub(crate) fn run_extension_dev_run_with(
    extension_id: &str,
    runner_id: &str,
    source: &str,
    command: &[String],
    mut sync: impl FnMut(&str, RunnerWorkspaceSyncOptions) -> Result<(RunnerWorkspaceSyncOutput, i32)>,
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(RunnerExecOutput, i32)>,
) -> Result<(ExtensionDevRunOutput, i32)> {
    let mut plan = plan_extension_dev_run(extension_id, runner_id, source, command)?;
    let source_materialization =
        extension_dev_run_source_materialization(&plan.extension_id, &plan.source)?;
    let (synced, _) = sync(
        &plan.runner_id,
        RunnerWorkspaceSyncOptions {
            path: source_materialization.sync_source.clone(),
            mode: plan.sync_mode,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?;
    let remote_install_source = remote_install_source(
        &synced.remote_path,
        source_materialization.remote_install_suffix.as_deref(),
    );

    plan.install_command = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "refresh".to_string(),
        remote_install_source.clone(),
        "--id".to_string(),
        plan.extension_id.clone(),
    ];

    let prior_extension_state = probe_runner_extension_state(&plan, &mut exec);
    let mut source_snapshot = homeboy_core::source_snapshot::collect_local(
        &plan.runner_id,
        Path::new(&synced.local_path),
        Some(&synced.remote_path),
        synced.sync_mode.label(),
    );
    source_snapshot.workspace_snapshot_identity = Some(synced.snapshot_identity.clone());
    let lifecycle = extension_dev_run_overlay_lifecycle(&plan, &synced, &source_snapshot);
    let provenance = ExtensionDevRunProvenance {
        extension_id: plan.extension_id.clone(),
        runner_id: plan.runner_id.clone(),
        local_source: synced.local_path.clone(),
        remote_source: remote_install_source.clone(),
        sync_mode: synced.sync_mode.label().to_string(),
        snapshot_identity: synced.snapshot_identity.clone(),
        source_revision: lifecycle.source_revision.clone(),
        install_mode: lifecycle.install_mode.clone(),
        cleanup_policy: lifecycle.cleanup_policy,
        ttl: lifecycle.ttl.clone(),
        install_command: plan.install_command.clone(),
        command: plan.command.clone(),
    };
    let provenance_json = serde_json::to_string(&provenance).map_err(|err| {
        Error::internal_json(
            format!("failed to serialize extension dev-run provenance: {err}"),
            None,
        )
    })?;
    let install_workload = extension_dev_run_workload(
        &plan,
        "install",
        "extension refresh",
        &synced.remote_path,
        Vec::new(),
    );

    let (install, _) = exec(
        &plan.runner_id,
        RunnerExecOptions {
            cwd: Some(synced.remote_path.clone()),
            project_id: None,
            allow_diagnostic_ssh: false,
            diagnostic_ssh_timeout: None,
            command: plan.install_command.clone(),
            env: provenance_env(&provenance_json),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            env_materialization: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: Some(source_snapshot.clone()),
            path_materialization_plan: None,
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "extension.dev-run.install".to_string(),
                required_commands: vec!["homeboy".to_string()],
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            accepted_extension_settings: Vec::new(),
            require_paths: Vec::new(),
            lab_runner_workload: Some(install_workload.clone()),
            run_id: None,
            run_id_owns_generic_exec: false,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
            read_only_artifact_access: false,
        },
    )?;
    let install_outcome = extension_dev_run_execution_outcome(&install_workload, &install);
    if install.exit_code != 0 {
        return Ok((
            ExtensionDevRunOutput {
                extension_id: plan.extension_id.clone(),
                runner_id: plan.runner_id.clone(),
                source: plan.source.clone(),
                remote_source: remote_install_source,
                plan,
                sync: synced,
                prior_extension_state,
                install,
                install_outcome,
                command: empty_skipped_exec_output(),
                command_outcome: None,
                provenance,
                lifecycle,
                persistent_state:
                    "runner extension refresh failed; inspect install output for persistent state"
                        .to_string(),
            },
            1,
        ));
    }
    let command_workload = extension_dev_run_workload(
        &plan,
        "command",
        &extension_dev_run_command_label(&plan.command),
        &synced.remote_path,
        vec![plan.extension_id.clone()],
    );

    let (command_output, command_exit_code) = exec(
        &plan.runner_id,
        RunnerExecOptions {
            cwd: Some(synced.remote_path.clone()),
            project_id: None,
            allow_diagnostic_ssh: false,
            diagnostic_ssh_timeout: None,
            command: plan.command.clone(),
            env: provenance_env(&provenance_json),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            env_materialization: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: Some(source_snapshot),
            path_materialization_plan: None,
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "extension.dev-run".to_string(),
                required_commands: plan.command.first().cloned().into_iter().collect(),
                ..Default::default()
            }),
            required_extensions: vec![plan.extension_id.clone()],
            accepted_extension_settings: Vec::new(),
            require_paths: Vec::new(),
            lab_runner_workload: Some(command_workload.clone()),
            run_id: None,
            run_id_owns_generic_exec: false,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
            read_only_artifact_access: false,
        },
    )?;
    let command_outcome = extension_dev_run_execution_outcome(&command_workload, &command_output);

    Ok((
        ExtensionDevRunOutput {
            extension_id: plan.extension_id.clone(),
            runner_id: plan.runner_id.clone(),
            source: plan.source.clone(),
            remote_source: remote_install_source,
            persistent_state: plan.persistent_state.clone(),
            plan,
            sync: synced,
            prior_extension_state,
            install,
            install_outcome,
            command: command_output,
            command_outcome: Some(command_outcome),
            provenance,
            lifecycle,
        },
        command_exit_code,
    ))
}

fn extension_dev_run_source_materialization(
    extension_id: &str,
    source: &str,
) -> Result<ExtensionDevRunSourceMaterialization> {
    let source_path = absolute_path(source)?;
    let manifest = source_path.join(format!("{extension_id}.json"));
    let Some(parent) = source_path.parent() else {
        return Ok(ExtensionDevRunSourceMaterialization {
            sync_source: source.to_string(),
            remote_install_suffix: None,
        });
    };

    let shared_project_scripts = parent.join("scripts/lib/project-scripts.sh");
    if manifest.exists() && shared_project_scripts.is_file() {
        let suffix = source_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "source",
                    format!(
                        "Could not determine extension directory name from {}",
                        source_path.display()
                    ),
                    Some(source.to_string()),
                    None,
                )
            })?
            .to_string();

        return Ok(ExtensionDevRunSourceMaterialization {
            sync_source: parent.display().to_string(),
            remote_install_suffix: Some(suffix),
        });
    }

    Ok(ExtensionDevRunSourceMaterialization {
        sync_source: source.to_string(),
        remote_install_suffix: None,
    })
}

fn absolute_path(source: &str) -> Result<PathBuf> {
    let path = Path::new(source);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Ok(std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("get current dir".to_string())))?
        .join(path))
}

fn remote_install_source(remote_path: &str, suffix: Option<&str>) -> String {
    match suffix {
        Some(suffix) => format!("{}/{}", remote_path.trim_end_matches('/'), suffix),
        None => remote_path.to_string(),
    }
}

fn required_arg(name: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::validation_invalid_argument(
            name,
            format!("{name} must not be empty"),
            None,
            None,
        ));
    }
    Ok(value.to_string())
}

fn extension_dev_run_workload(
    plan: &ExtensionDevRunPlan,
    stage: &str,
    command_label: &str,
    remote_workspace: &str,
    required_extensions: Vec<String>,
) -> LabRunnerWorkload {
    let plan_id = format!(
        "extension-dev-run:{}:{}:{stage}",
        plan.runner_id, plan.extension_id
    );
    LabRunnerWorkload {
        schema: LAB_RUNNER_WORKLOAD_SCHEMA.to_string(),
        workload_id: format!("{plan_id}.runner_workload"),
        kind: LabRunnerWorkloadKind {
            command_label: command_label.to_string(),
            command_family: LabRunnerWorkloadCommandFamily::from_command_label(command_label),
        },
        agent_task: None,
        notification_route: homeboy_core::notification_route::current(),
        workspace_mappings: LabRunnerWorkloadWorkspaceMappings {
            source_path_mode: "snapshot".to_string(),
            workspace_mode_policy: "snapshot_unique_workspace".to_string(),
            mapping_ref: Some(plan.source.clone()),
        },
        required_capabilities: Vec::new(),
        required_secrets: LabRunnerWorkloadSecrets {
            categories: Vec::new(),
            secret_env_plan: Default::default(),
        },
        required_extensions,
        required_extension_revisions: Vec::new(),
        mutation_policy: LabRunnerWorkloadMutationPolicy {
            capture_patch: false,
            mutation_flag: None,
            allow_dirty_lab_workspace: false,
        },
        assignment: LabRunnerWorkloadAssignment {
            runner_id: Some(plan.runner_id.clone()),
            runner_mode: None,
            source: Some(plan.source.clone()),
        },
        state: LabRunnerWorkloadState {
            status: "dispatched".to_string(),
            remote_workspace: Some(remote_workspace.to_string()),
            fallback_reason: None,
        },
        result_refs: LabRunnerWorkloadResultRefs {
            plan_id,
            proof_id: None,
            workspace_mapping_ref: Some(plan.source.clone()),
            job_id: None,
            mirror_run_id: None,
            artifacts: Vec::new(),
        },
    }
}

fn extension_dev_run_command_label(command: &[String]) -> String {
    let args = match command.first().map(String::as_str) {
        Some(binary)
            if Path::new(binary).file_name().and_then(|name| name.to_str()) == Some("homeboy") =>
        {
            &command[1..]
        }
        _ => command,
    };
    args.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
}

fn extension_dev_run_execution_outcome(
    lab_runner_workload: &LabRunnerWorkload,
    output: &RunnerExecOutput,
) -> ExtensionDevRunExecutionOutcome {
    let mut lab_runner_workload = lab_runner_workload.clone();
    lab_runner_workload.result_refs.job_id = output.job_id.clone();
    lab_runner_workload.result_refs.mirror_run_id = output.mirror_run_id.clone();
    lab_runner_workload.result_refs.artifacts = output
        .execution_record
        .as_ref()
        .map(|record| {
            record
                .artifact_refs
                .iter()
                .map(|artifact| LabRunnerWorkloadArtifactRef {
                    id: artifact.id.clone(),
                    name: artifact.name.clone(),
                    path: artifact.path.clone(),
                    url: artifact.url.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    ExtensionDevRunExecutionOutcome {
        lab_runner_workload,
        execution_record: output
            .execution_record
            .as_ref()
            .map(|record| record.projection()),
        exit_code: output.exit_code,
    }
}

fn extension_dev_run_overlay_lifecycle(
    plan: &ExtensionDevRunPlan,
    synced: &RunnerWorkspaceSyncOutput,
    source_snapshot: &SourceSnapshot,
) -> ExtensionDevRunOverlayLifecycle {
    ExtensionDevRunOverlayLifecycle {
        overlay_id: format!("extension-overlay:{}:{}", plan.runner_id, plan.extension_id),
        source_revision: ExtensionDevRunSourceRevision {
            source_ref: source_snapshot.git_branch.clone(),
            source_commit: source_snapshot.git_sha.clone(),
            source_dirty: source_snapshot.dirty,
            snapshot_identity: synced.snapshot_identity.clone(),
            snapshot_hash: source_snapshot.snapshot_hash.clone(),
        },
        install_mode: "refresh_from_synced_source_snapshot".to_string(),
        cleanup_policy: ResourceCleanupPolicy::Preserve,
        ttl: None,
        retain_policy: "retain runner extension overlay until an operator refreshes, relinks, reinstalls, or uninstalls the extension".to_string(),
        revert_policy: "manual: restore the prior extension state with extension refresh, relink, install, or uninstall".to_string(),
        lease: synced.workspace_lease.clone(),
    }
}

fn probe_runner_extension_state(
    plan: &ExtensionDevRunPlan,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(RunnerExecOutput, i32)>,
) -> RunnerExtensionStateProbe {
    let command = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "show".to_string(),
        plan.extension_id.clone(),
    ];
    match exec(
        &plan.runner_id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: false,
            diagnostic_ssh_timeout: None,
            command: command.clone(),
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            env_materialization: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            path_materialization_plan: None,
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "extension.dev-run.probe".to_string(),
                required_commands: vec!["homeboy".to_string()],
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            accepted_extension_settings: Vec::new(),
            require_paths: Vec::new(),
            lab_runner_workload: None,
            run_id: None,
            run_id_owns_generic_exec: false,
            detach_after_handoff: false,
            mirror_evidence: false,
            print_handoff: false,
            read_only_artifact_access: false,
        },
    ) {
        Ok((output, _)) => RunnerExtensionStateProbe {
            command,
            captured: output.exit_code == 0,
            exit_code: Some(output.exit_code),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            error: None,
        },
        Err(err) => RunnerExtensionStateProbe {
            command,
            captured: false,
            exit_code: None,
            stdout: None,
            stderr: None,
            error: Some(err.to_string()),
        },
    }
}

fn provenance_env(provenance_json: &str) -> HashMap<String, String> {
    HashMap::from([(
        "HOMEBOY_EXTENSION_DEV_RUN_PROVENANCE_JSON".to_string(),
        provenance_json.to_string(),
    )])
}

fn empty_skipped_exec_output() -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "runner_exec",
        command: "runner.exec",
        runner_id: String::new(),
        dry_run: false,
        mode: crate::runners::RunnerExecMode::Local,
        argv: Vec::new(),
        remote_cwd: String::new(),
        exit_code: 1,
        stdout: String::new(),
        stderr: "skipped because extension refresh failed".to_string(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: None,
        job_events: None,
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;

    use super::*;
    use homeboy_core::runner_execution_envelope::RunnerExecutionRecord;
    use homeboy_lab_runner_contract::{
        ByteFileCounts, RunnerLifecycleOwner, RunnerWorkspaceCurrentSummary, RunnerWorkspaceLease,
    };
    use tempfile::TempDir;

    #[test]
    fn plans_refresh_then_requested_command() {
        let command = vec![
            "homeboy".to_string(),
            "extension".to_string(),
            "run".to_string(),
            "x".to_string(),
        ];
        let plan = plan_extension_dev_run("x", "lab", "/tmp/ext", &command).expect("plan");

        assert_eq!(plan.sync_mode, RunnerWorkspaceSyncMode::Snapshot);
        assert_eq!(plan.runner_id, "lab");
        assert_eq!(plan.source, "/tmp/ext");
        assert_eq!(plan.command, command);
        assert!(plan.persistent_state.contains("left refreshed/linked"));
    }

    #[test]
    fn rejects_missing_command() {
        let err = plan_extension_dev_run("x", "lab", "/tmp/ext", &[]).expect_err("missing command");

        assert!(err.to_string().contains("requires a command after --"));
    }

    #[test]
    fn executes_probe_refresh_and_command_with_provenance() {
        let seen_sync = RefCell::new(Vec::new());
        let seen_exec = RefCell::new(Vec::new());
        let command = vec![
            "homeboy".to_string(),
            "extension".to_string(),
            "run".to_string(),
            "demo".to_string(),
        ];

        let (output, exit_code) = run_extension_dev_run_with(
            "demo",
            "lab",
            "/local/demo",
            &command,
            |runner_id, options| {
                seen_sync
                    .borrow_mut()
                    .push((runner_id.to_string(), options));
                Ok((sync_output(), 0))
            },
            |runner_id, options| {
                seen_exec.borrow_mut().push((
                    runner_id.to_string(),
                    options.command.clone(),
                    options.env.clone(),
                    options.required_extensions.clone(),
                    options.lab_runner_workload.clone(),
                ));
                Ok((exec_output(runner_id, options.command, 0), 0))
            },
        )
        .expect("dev run");

        assert_eq!(exit_code, 0);
        assert_eq!(
            seen_sync.borrow()[0].1.mode,
            RunnerWorkspaceSyncMode::Snapshot
        );
        let execs = seen_exec.borrow();
        assert_eq!(execs[0].1, vec!["homeboy", "extension", "show", "demo"]);
        assert_eq!(
            execs[1].1,
            vec![
                "homeboy",
                "extension",
                "refresh",
                "/remote/demo",
                "--id",
                "demo"
            ]
        );
        assert_eq!(execs[2].1, command);
        assert_eq!(execs[2].3, vec!["demo".to_string()]);
        assert!(execs[0].4.is_none());
        assert_eq!(
            execs[1]
                .4
                .as_ref()
                .expect("install workload")
                .kind
                .command_label,
            "extension refresh"
        );
        assert_eq!(
            execs[2]
                .4
                .as_ref()
                .expect("command workload")
                .required_extensions,
            vec!["demo".to_string()]
        );
        assert!(execs[2]
            .2
            .contains_key("HOMEBOY_EXTENSION_DEV_RUN_PROVENANCE_JSON"));
        let provenance: serde_json::Value = serde_json::from_str(
            execs[2]
                .2
                .get("HOMEBOY_EXTENSION_DEV_RUN_PROVENANCE_JSON")
                .expect("provenance env"),
        )
        .expect("provenance json");
        assert_eq!(
            provenance["install_mode"],
            "refresh_from_synced_source_snapshot"
        );
        assert_eq!(provenance["cleanup_policy"], "preserve");
        assert!(provenance["ttl"].is_null());
        assert_eq!(
            provenance["source_revision"]["snapshot_identity"],
            "snapshot-1"
        );
        assert_eq!(output.remote_source, "/remote/demo");
        assert!(output.persistent_state.contains("left refreshed/linked"));
        assert_eq!(
            output.lifecycle.overlay_id,
            "extension-overlay:lab:demo".to_string()
        );
        assert_eq!(
            output.lifecycle.install_mode,
            "refresh_from_synced_source_snapshot"
        );
        assert_eq!(
            output.lifecycle.cleanup_policy,
            ResourceCleanupPolicy::Preserve
        );
        assert_eq!(output.lifecycle.ttl, None);
        assert_eq!(output.lifecycle.lease.remote_path, "/remote/demo");
        assert!(output.lifecycle.retain_policy.contains("retain"));
        assert!(output.lifecycle.revert_policy.contains("manual"));
        assert_eq!(
            output
                .install_outcome
                .lab_runner_workload
                .kind
                .command_label,
            "extension refresh"
        );
        assert_eq!(output.install_outcome.exit_code, 0);
        assert_eq!(
            output
                .command_outcome
                .as_ref()
                .expect("command outcome")
                .lab_runner_workload
                .result_refs
                .job_id,
            Some("job-homeboy-extension-run-demo".to_string())
        );
        assert_eq!(
            output
                .command_outcome
                .as_ref()
                .expect("command outcome")
                .lab_runner_workload
                .result_refs
                .mirror_run_id,
            Some("run-homeboy-extension-run-demo".to_string())
        );
        assert_eq!(
            output
                .command_outcome
                .as_ref()
                .expect("command outcome")
                .execution_record
                .as_ref()
                .expect("command execution projection")
                .status,
            "succeeded"
        );
    }

    #[test]
    fn dev_run_stages_dispatch_from_the_workload_remote_workspace() {
        let stages = RefCell::new(Vec::new());
        let command = vec![
            "homeboy".to_string(),
            "extension".to_string(),
            "show".to_string(),
            "demo".to_string(),
        ];

        run_extension_dev_run_with(
            "demo",
            "lab",
            "/local/demo",
            &command,
            |_runner_id, _options| Ok((sync_output(), 0)),
            |runner_id, options| {
                if let Some(workload) = options.lab_runner_workload.clone() {
                    stages.borrow_mut().push((options.cwd.clone(), workload));
                }
                Ok((exec_output(runner_id, options.command, 0), 0))
            },
        )
        .expect("dev run");

        assert_eq!(stages.borrow().len(), 2);
        for (cwd, workload) in stages.borrow().iter() {
            assert_eq!(cwd.as_deref(), Some("/remote/demo"));
            assert_eq!(
                workload.state.remote_workspace.as_deref(),
                cwd.as_deref(),
                "each controller-owned stage must dispatch from its declared remote workspace"
            );
        }
    }

    #[test]
    fn dev_run_syncs_monorepo_root_when_extension_uses_shared_project_scripts() {
        let source_root = TempDir::new().expect("source root");
        let extension_dir = source_root.path().join("nodejs");
        fs::create_dir_all(extension_dir.join("scripts/lib")).expect("extension lib");
        fs::create_dir_all(source_root.path().join("scripts/lib")).expect("shared lib");
        fs::write(extension_dir.join("nodejs.json"), r#"{"name":"Node.js"}"#).expect("manifest");
        fs::write(
            extension_dir.join("scripts/lib/node-helpers.sh"),
            "source \"$(dirname \"${BASH_SOURCE[0]}\")/../../../scripts/lib/project-scripts.sh\"\n",
        )
        .expect("node helper");
        fs::write(
            source_root.path().join("scripts/lib/project-scripts.sh"),
            "homeboy_project_init() { :; }\n",
        )
        .expect("shared project scripts");

        let seen_sync = RefCell::new(Vec::new());
        let seen_exec = RefCell::new(Vec::new());
        let command = vec!["homeboy".to_string(), "bench".to_string()];
        let remote_root = "/remote/homeboy-extensions";

        run_extension_dev_run_with(
            "nodejs",
            "lab",
            &extension_dir.to_string_lossy(),
            &command,
            |runner_id, options| {
                seen_sync.borrow_mut().push(options.path.clone());
                Ok((sync_output_for(runner_id, &options.path, remote_root), 0))
            },
            |runner_id, options| {
                seen_exec.borrow_mut().push(options.command.clone());
                Ok((exec_output(runner_id, options.command, 0), 0))
            },
        )
        .expect("dev run");

        assert_eq!(
            seen_sync.borrow()[0],
            source_root.path().display().to_string()
        );
        assert_eq!(
            seen_exec.borrow()[1],
            vec![
                "homeboy",
                "extension",
                "refresh",
                "/remote/homeboy-extensions/nodejs",
                "--id",
                "nodejs"
            ]
        );
    }

    fn sync_output() -> RunnerWorkspaceSyncOutput {
        sync_output_for("lab", "/local/demo", "/remote/demo")
    }

    fn sync_output_for(
        runner_id: &str,
        local_path: &str,
        remote_path: &str,
    ) -> RunnerWorkspaceSyncOutput {
        RunnerWorkspaceSyncOutput {
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: runner_id.to_string(),
            local_path: local_path.to_string(),
            remote_path: remote_path.to_string(),
            materialization_plan: crate::RunnerWorkspaceMaterializationPlan::from_test_parts(
                "/remote",
                local_path,
                Path::new(local_path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("demo"),
                remote_path,
                RunnerWorkspaceSyncMode::Snapshot,
                "snapshot-1",
            ),
            current_workspace: RunnerWorkspaceCurrentSummary {
                local_path: local_path.to_string(),
                remote_path: remote_path.to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                materialized: true,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
                synthetic_checkout_commit: None,
                synthetic_checkout_ref: None,
                synthetic_checkout_tree: None,
            },
            workspace_lease: RunnerWorkspaceLease {
                runner_id: runner_id.to_string(),
                local_path: local_path.to_string(),
                remote_path: remote_path.to_string(),
                sync_mode: "snapshot".to_string(),
                materialized: true,
                lifecycle_owner: RunnerLifecycleOwner::Controller,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
            },
            resource_lifecycle: crate::workspace_resource_lifecycle(
                runner_id,
                remote_path,
                None,
                homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
            ),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: "snapshot-1".to_string(),
            prepared_workspace_lease: None,
            counts: ByteFileCounts { files: 1, bytes: 2 },
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "snapshot_unique_workspace".to_string(),
            validation_dependencies: Vec::new(),
        }
    }

    fn exec_output(runner_id: &str, command: Vec<String>, exit_code: i32) -> RunnerExecOutput {
        let execution_id = command.join("-");
        let execution_record = Some(RunnerExecutionRecord::terminal(
            format!("exec-{runner_id}-{execution_id}"),
            runner_id,
            "local",
            exit_code,
        ));
        RunnerExecOutput {
            variant: "runner_exec",
            command: "runner.exec",
            runner_id: runner_id.to_string(),
            dry_run: false,
            mode: crate::runners::RunnerExecMode::Local,
            argv: command,
            remote_cwd: String::new(),
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: Some(format!("job-{execution_id}")),
            job_events: None,
            mirror_run_id: Some(format!("run-{execution_id}")),
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        }
    }
}
