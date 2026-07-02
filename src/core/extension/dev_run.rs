use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::core::error::{Error, Result};
use crate::core::runners::{
    self, RunnerCapabilityPreflight, RunnerExecOptions, RunnerExecOutput,
    RunnerWorkspaceMaterializationPlan, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
    RunnerWorkspaceSyncOutput,
};
use crate::core::source_snapshot::SourceSnapshot;

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
    pub command: RunnerExecOutput,
    pub provenance: ExtensionDevRunProvenance,
    pub persistent_state: String,
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
        |runner_id, options| runners::sync_workspace(runner_id, options),
        |runner_id, options| runners::exec(runner_id, options),
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
    let (synced, _) = sync(
        &plan.runner_id,
        RunnerWorkspaceSyncOptions {
            path: plan.source.clone(),
            mode: plan.sync_mode,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?;

    plan.install_command = vec![
        "homeboy".to_string(),
        "extension".to_string(),
        "refresh".to_string(),
        synced.remote_path.clone(),
        "--id".to_string(),
        plan.extension_id.clone(),
    ];

    let prior_extension_state = probe_runner_extension_state(&plan, &mut exec);
    let source_snapshot = SourceSnapshot::collect_local(
        &plan.runner_id,
        Path::new(&synced.local_path),
        Some(&synced.remote_path),
        synced.sync_mode.label(),
    );
    let provenance = ExtensionDevRunProvenance {
        extension_id: plan.extension_id.clone(),
        runner_id: plan.runner_id.clone(),
        local_source: synced.local_path.clone(),
        remote_source: synced.remote_path.clone(),
        sync_mode: synced.sync_mode.label().to_string(),
        snapshot_identity: synced.snapshot_identity.clone(),
        install_command: plan.install_command.clone(),
        command: plan.command.clone(),
    };
    let provenance_json = serde_json::to_string(&provenance).map_err(|err| {
        Error::internal_json(
            format!("failed to serialize extension dev-run provenance: {err}"),
            None,
        )
    })?;

    let (install, _) = exec(
        &plan.runner_id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: false,
            command: plan.install_command.clone(),
            env: provenance_env(&provenance_json),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: Some(source_snapshot.clone()),
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "extension.dev-run.install".to_string(),
                required_commands: vec!["homeboy".to_string()],
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;
    if install.exit_code != 0 {
        return Ok((
            ExtensionDevRunOutput {
                extension_id: plan.extension_id.clone(),
                runner_id: plan.runner_id.clone(),
                source: plan.source.clone(),
                remote_source: synced.remote_path.clone(),
                plan,
                sync: synced,
                prior_extension_state,
                install,
                command: empty_skipped_exec_output(),
                provenance,
                persistent_state:
                    "runner extension refresh failed; inspect install output for persistent state"
                        .to_string(),
            },
            1,
        ));
    }

    let (command_output, command_exit_code) = exec(
        &plan.runner_id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: false,
            command: plan.command.clone(),
            env: provenance_env(&provenance_json),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: Some(source_snapshot),
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "extension.dev-run".to_string(),
                required_commands: plan.command.first().cloned().into_iter().collect(),
                ..Default::default()
            }),
            required_extensions: vec![plan.extension_id.clone()],
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;

    Ok((
        ExtensionDevRunOutput {
            extension_id: plan.extension_id.clone(),
            runner_id: plan.runner_id.clone(),
            source: plan.source.clone(),
            remote_source: synced.remote_path.clone(),
            persistent_state: plan.persistent_state.clone(),
            plan,
            sync: synced,
            prior_extension_state,
            install,
            command: command_output,
            provenance,
        },
        command_exit_code,
    ))
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
            command: command.clone(),
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "extension.dev-run.probe".to_string(),
                required_commands: vec!["homeboy".to_string()],
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: false,
            print_handoff: false,
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
        mode: runners::RunnerExecMode::Local,
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

    use super::*;
    use crate::core::runner::{
        ByteFileCounts, RunnerLifecycleOwner, RunnerWorkspaceCurrentSummary, RunnerWorkspaceLease,
    };

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
        assert!(execs[2]
            .2
            .contains_key("HOMEBOY_EXTENSION_DEV_RUN_PROVENANCE_JSON"));
        assert_eq!(output.remote_source, "/remote/demo");
        assert!(output.persistent_state.contains("left refreshed/linked"));
    }

    fn sync_output() -> RunnerWorkspaceSyncOutput {
        RunnerWorkspaceSyncOutput {
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/local/demo".to_string(),
            remote_path: "/remote/demo".to_string(),
            materialization_plan: RunnerWorkspaceMaterializationPlan {
                workspace_root: "/remote".to_string(),
                local_path: "/local/demo".to_string(),
                local_basename: "demo".to_string(),
                remote_path: "/remote/demo".to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                identity: "snapshot-1".to_string(),
                path_strategy: "workspace_root_lab_workspaces_sanitized_basename_identity_digest",
                run_isolation_token: None,
            },
            current_workspace: RunnerWorkspaceCurrentSummary {
                local_path: "/local/demo".to_string(),
                remote_path: "/remote/demo".to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                materialized: true,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
                synthetic_checkout_commit: None,
            },
            workspace_lease: RunnerWorkspaceLease {
                runner_id: "lab".to_string(),
                local_path: "/local/demo".to_string(),
                remote_path: "/remote/demo".to_string(),
                sync_mode: "snapshot".to_string(),
                materialized: true,
                lifecycle_owner: RunnerLifecycleOwner::Controller,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
            },
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: "snapshot-1".to_string(),
            counts: ByteFileCounts { files: 1, bytes: 2 },
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "snapshot_unique_workspace".to_string(),
            validation_dependencies: Vec::new(),
        }
    }

    fn exec_output(runner_id: &str, command: Vec<String>, exit_code: i32) -> RunnerExecOutput {
        RunnerExecOutput {
            variant: "runner_exec",
            command: "runner.exec",
            runner_id: runner_id.to_string(),
            dry_run: false,
            mode: runners::RunnerExecMode::Local,
            argv: command,
            remote_cwd: String::new(),
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
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
}
