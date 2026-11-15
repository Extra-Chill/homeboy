//! Runner-side implementation of core's `RunnerExecDriver` hook.
//!
//! Core's daemon `/exec` endpoint prepares and runs a runner job as a local
//! child. This adapter builds the runner process plan and drives the child,
//! keeping runner types (`Runner`, `PreparedRunnerProcess`, `ProcessOutput`)
//! inside the runner layer while the daemon sees only slim core types.

use std::sync::Arc;

use serde_json::Value;

use homeboy_core::daemon::runner_exec_driver::{
    DaemonExecOutput, ExecCancellationProbe, ExecChildStarted, ExecProgressSink,
    PreparedDaemonExec, PreparedDaemonExecRequest, RunnerExecDriver, RunnerExecPrepareRequest,
};
use homeboy_core::error::Result;
use homeboy_core::runner_job_execution_context::RunnerJobExecutionContext;
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_runner_contract::RunnerExecutionEnvelope;
use uuid::Uuid;

use super::execution::{
    execute_runner_process_until_cancelled_with_progress, prepare_daemon_local_process,
    PreparedRunnerProcess, RunnerProcessRequest,
};
use super::Runner;

struct DaemonPreparedPlan {
    _private_files: super::staged_workspace::PrivateAtFileCleanup,
    runner: Runner,
    extension_env_providers: Vec<String>,
    authoritative_run_id: Option<String>,
    staged: Option<DaemonStagedPlan>,
}

struct DaemonStagedPlan {
    envelope: RunnerExecutionEnvelope,
    workspace: Option<super::staged_workspace::StagedWorkspaceDirectory>,
}

/// The runner layer's `RunnerExecDriver`. Registered with core at startup.
pub struct RunnerDaemonExecDriver;

impl RunnerExecDriver for RunnerDaemonExecDriver {
    fn prepare(&self, request: RunnerExecPrepareRequest) -> Result<PreparedDaemonExec> {
        prepare_daemon_exec(request, None)
    }

    fn prepare_staged(
        &self,
        mut request: RunnerExecPrepareRequest,
        job_id: Uuid,
        envelope: &RunnerExecutionEnvelope,
    ) -> Result<PreparedDaemonExec> {
        // The sealed submission is the source authority. Only the runner driver
        // may derive its workspace location after extracting and verifying it.
        let mut materialized_envelope = envelope.clone();
        let workspace = super::staged_workspace::materialize_staged_source_artifact(
            &request.runner_id,
            &job_id.to_string(),
            &mut materialized_envelope,
        )?;
        super::staged_workspace::verify_staged_workspace_before_execution(
            &request.runner_id,
            &materialized_envelope,
            workspace.as_ref(),
        )?;
        let dispatch = materialized_envelope.dispatch.as_ref().ok_or_else(|| {
            homeboy_core::error::Error::internal_unexpected(
                "staged runner job has no execution dispatch",
            )
        })?;
        request.cwd = dispatch.cwd.clone();
        request.command = dispatch.command.clone();
        request.source_snapshot = dispatch.source_snapshot.clone();
        prepare_daemon_exec(
            request,
            Some(DaemonStagedPlan {
                envelope: materialized_envelope,
                workspace,
            }),
        )
    }

    fn execute(
        &self,
        prepared: &PreparedDaemonExec,
        execution_context: &RunnerJobExecutionContext,
        is_cancelled: ExecCancellationProbe,
        progress_sink: Option<ExecProgressSink>,
        require_child_identity_acknowledgement: bool,
        child_started: Option<ExecChildStarted>,
    ) -> Result<DaemonExecOutput> {
        execute_daemon_exec(
            prepared,
            execution_context,
            is_cancelled,
            progress_sink,
            require_child_identity_acknowledgement,
            child_started,
        )
    }
}

fn prepare_daemon_exec(
    mut request: RunnerExecPrepareRequest,
    staged: Option<DaemonStagedPlan>,
) -> Result<PreparedDaemonExec> {
    let private_files =
        super::staged_workspace::verify_private_command_arguments(&mut request.command)?;
    let runner: Option<Runner> = match request.runner {
        Some(value) => Some(serde_json::from_value(value).map_err(|err| {
            homeboy_core::error::Error::validation_invalid_argument(
                "runner",
                format!("invalid runner descriptor in exec request: {err}"),
                None,
                None,
            )
        })?),
        None => None,
    };
    let secret_env_plan: Option<SecretEnvPlan> = match request.secret_env_plan {
        Some(Value::Null) | None => None,
        Some(value) => Some(serde_json::from_value(value).map_err(|err| {
            homeboy_core::error::Error::validation_invalid_argument(
                "secret_env_plan",
                format!("invalid secret env plan in exec request: {err}"),
                None,
                None,
            )
        })?),
    };

    let provider_secret_names = request
        .extension_env_providers
        .iter()
        .map(|id| homeboy_core::extension::invoke::declared_secret_names(id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let secret_env_plan = super::execution::runner_exec_secret_env_plan(
        &request.command,
        None,
        &provider_secret_names,
        &request.env,
        secret_env_plan,
    );
    let authoritative_run_id = request.authoritative_run_id;
    // This driver runs inside the daemon, and its child reads the daemon's
    // build-specific job store, so the child's Homeboy is this binary. If the
    // path cannot be resolved the child's build stamp check names the skew.
    let mut env = request.env;
    if let Ok(daemon_executable) = homeboy_core::daemon::daemon_launchable_executable() {
        super::command_path::select_daemon_child_homeboy(&mut env, &daemon_executable);
    }
    let mut plan = prepare_daemon_local_process(RunnerProcessRequest {
        runner_id: request.runner_id,
        runner,
        cwd: request.cwd,
        project_id: request.project_id,
        command: request.command,
        env,
        secret_env_names: request.secret_env_names,
        secret_env_plan: Some(secret_env_plan),
        capture_patch: request.capture_patch,
        raw_exec: request.raw_exec,
        source_snapshot: request.source_snapshot,
        require_paths: request.require_paths,
        validate_require_paths_on_host: request.validate_require_paths_on_host,
    })?;
    super::command_path::front_selected_homeboy_on_path(&mut plan.env);
    // A Homeboy child authenticates against this daemon's build-specific store.
    // Select its executable before the daemon records the execution context.
    if let Some(program) = plan.command.first_mut().filter(|program| {
        std::path::Path::new(program.as_str()).file_name() == Some(std::ffi::OsStr::new("homeboy"))
    }) {
        *program = homeboy_core::daemon::daemon_launchable_executable()
            .map_err(|error| homeboy_core::error::Error::internal_io(error.to_string(), None))?
            .display()
            .to_string();
    }
    // The private token keeps only runner metadata, not a second process plan.
    Ok(PreparedDaemonExec::new(PreparedDaemonExecRequest {
        runner_id: plan.runner.id.clone(),
        cwd: plan.cwd.clone(),
        command: plan.command,
        env: plan.env,
        secret_env_names: plan.secret_env_names.clone(),
        source_snapshot: plan.source_snapshot.clone(),
        require_paths: plan.require_paths.clone(),
        concurrency_limit: plan.runner.settings.concurrency_limit,
        heartbeat_only_stall: plan.runner.settings.heartbeat_only_stall.clone(),
        // This is a durable declaration, not provider output. The provider
        // itself is resolved only after the authenticated context arrives
        // at `execute`.
        extension_env_provenance: serde_json::json!({ "providers": request.extension_env_providers.clone() }),
        plan_token: Arc::new(DaemonPreparedPlan {
            _private_files: private_files,
            runner: plan.runner,
            extension_env_providers: request.extension_env_providers,
            authoritative_run_id,
            staged,
        }),
    }))
}

fn execute_daemon_exec(
    prepared: &PreparedDaemonExec,
    execution_context: &RunnerJobExecutionContext,
    is_cancelled: ExecCancellationProbe,
    progress_sink: Option<ExecProgressSink>,
    require_child_identity_acknowledgement: bool,
    child_started: Option<ExecChildStarted>,
) -> Result<DaemonExecOutput> {
    let base = prepared
        .plan_token()
        .downcast_ref::<DaemonPreparedPlan>()
        .ok_or_else(|| {
            homeboy_core::error::Error::internal_unexpected(
                "runner exec driver received a plan it did not prepare",
            )
        })?;

    if execution_context.verify_integrity().is_err()
        || execution_context.runner_id() != prepared.runner_id
        || execution_context.runtime_id()
            != prepared
                .command
                .first()
                .map(String::as_str)
                .unwrap_or_default()
    {
        return Err(homeboy_core::error::Error::validation_invalid_argument(
            "execution_context",
            "daemon execution context does not match the accepted runner job",
            Some(prepared.runner_id.clone()),
            Some(vec![
                "Claim a fresh runner job through the controller before retrying.".to_string(),
            ]),
        ));
    }
    // Spawn exactly the process descriptor verified above, including the
    // environment the daemon updated with the child reservation after prepare.
    let mut plan = PreparedRunnerProcess {
        runner: base.runner.clone(),
        cwd: prepared.cwd.clone(),
        command: prepared.command.clone(),
        env: prepared.env.clone(),
        secret_env_names: prepared.secret_env_names.clone(),
        source_snapshot: prepared.source_snapshot.clone(),
        require_paths: prepared.require_paths.clone(),
    };
    if let Some(staged) = &base.staged {
        // Revalidate immediately before providers can inspect the workspace or
        // a child can be spawned. The retained descriptor remains authoritative
        // while capacity admission waits.
        super::staged_workspace::verify_staged_workspace_before_execution(
            &prepared.runner_id,
            &staged.envelope,
            staged.workspace.as_ref(),
        )?;
    }
    // Provider adapters execute only after the daemon supplied the verified
    // typed context. No environment value participates in this decision.
    let contributions = super::execution::resolve_provider_env_with_execution_context(
        execution_context,
        &base.extension_env_providers,
        std::path::Path::new(&plan.cwd),
        &plan.env,
        base.authoritative_run_id.as_deref(),
    )?;
    for contribution in &contributions {
        for (key, value) in &contribution.public_env {
            plan.env.insert(key.clone(), value.clone());
        }
    }
    let diagnostic_hints = super::execution::apply_explicit_runner_exec_run_id_env(
        &mut plan.env,
        base.authoritative_run_id.as_deref(),
    )
    .into_iter()
    .collect();
    let extension_env_provenance = serde_json::to_value(&contributions).map_err(|err| {
        homeboy_core::error::Error::internal_json(
            err.to_string(),
            Some("serialize extension env provenance".to_string()),
        )
    })?;

    if let Some(dir) = &prepared.output_dir {
        // A daemon that re-adopts this child after a restart redacts with these.
        let policy = homeboy_core::redaction::RedactionPolicy::default();
        homeboy_core::daemon::runner_exec_job::write_redaction_values(
            dir,
            &super::execution::redaction::runner_exec_secret_values(
                &plan.env,
                &plan.secret_env_names,
                &policy,
            ),
        )?;
    }
    let output = execute_runner_process_until_cancelled_with_progress(
        &plan,
        is_cancelled,
        progress_sink,
        require_child_identity_acknowledgement,
        child_started,
        #[cfg(unix)]
        base.staged
            .as_ref()
            .and_then(|staged| staged.workspace.as_ref())
            .map(super::staged_workspace::StagedWorkspaceDirectory::fd),
        prepared.output_dir.as_deref(),
        prepared.cpus.as_deref(),
    )?;
    let (stdout, stderr) = super::execution::redaction::redact_runner_exec_streams(
        output.stdout,
        output.stderr,
        &plan.env,
        &plan.secret_env_names,
    );

    Ok(DaemonExecOutput {
        stdout,
        stderr,
        exit_code: output.exit_code,
        metrics: output
            .metrics
            .and_then(|metrics| serde_json::to_value(metrics).ok()),
        capture: output
            .capture
            .and_then(|capture| serde_json::to_value(capture).ok()),
        extension_env_provenance,
        diagnostic_hints,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_PROBE_ENV: &str = "HOMEBOY_DAEMON_EXEC_DRIVER_CHILD_PROBE";

    #[test]
    fn daemon_exec_child_probe() {
        if std::env::var_os(CHILD_PROBE_ENV).is_some() {
            println!(
                "daemon-exec-child={}",
                std::env::current_exe().unwrap().display()
            );
        }
    }

    #[test]
    fn daemon_exec_spawns_the_recorded_daemon_runtime() {
        let workspace = tempfile::tempdir().unwrap();
        let daemon_executable = homeboy_core::daemon::daemon_launchable_executable().unwrap();
        // This nonexistent controller slot must never be launched. The daemon
        // executable here is the test harness; an exact test acts as its probe.
        let controller_homeboy = workspace.path().join("controller-slot/homeboy");
        assert_ne!(controller_homeboy, daemon_executable);
        let driver = RunnerDaemonExecDriver;
        let prepared = driver
            .prepare(RunnerExecPrepareRequest {
                runner_id: "daemon-exec-test".to_string(),
                runner: None,
                project_id: None,
                cwd: Some(workspace.path().display().to_string()),
                command: vec![
                    controller_homeboy.display().to_string(),
                    "--exact".to_string(),
                    "daemon_exec_driver::tests::daemon_exec_child_probe".to_string(),
                    "--nocapture".to_string(),
                ],
                env: std::collections::HashMap::from([(
                    CHILD_PROBE_ENV.to_string(),
                    "1".to_string(),
                )]),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                capture_patch: false,
                raw_exec: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                authoritative_run_id: None,
                validate_require_paths_on_host: false,
            })
            .unwrap();
        assert_eq!(prepared.command[0], daemon_executable.display().to_string());
        let context = RunnerJobExecutionContext::direct_daemon(
            None,
            &prepared.runner_id,
            "daemon-exec-job",
            &prepared.command[0],
            "daemon-exec-reservation",
        )
        .unwrap();
        let output = driver
            .execute(&prepared, &context, Box::new(|| false), None, false, None)
            .unwrap();
        assert_eq!(output.exit_code, 0, "{}", output.stderr);
        let observed_runtime = output
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("daemon-exec-child="))
            .expect("spawned daemon executable must report its own path");
        assert_eq!(observed_runtime, daemon_executable.to_str().unwrap());
        assert_eq!(observed_runtime, context.runtime_id());
    }
}

/// Register the runner daemon-exec driver with core. Called once at startup.
pub fn register() {
    homeboy_core::daemon::runner_exec_driver::register_runner_exec_driver(Arc::new(
        RunnerDaemonExecDriver,
    ));
}
