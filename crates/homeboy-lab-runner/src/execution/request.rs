use std::path::Path;
use std::time::{Duration, Instant};

use homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore;
use homeboy_core::api_jobs::RunnerJobLogSnapshot;
use homeboy_core::observation::ArtifactRecord;
use homeboy_core::runner_execution_envelope::RunnerExecutionRecord;
use homeboy_core::{Error, Result};

use super::{exec, RunnerExecOptions, RunnerExecOutput, RunnerTransport};

/// All controller-side work required before dispatching a runner command.
///
/// `options` remains the low-level runner execution contract. The remaining
/// fields deliberately describe work that must be durably owned before a
/// runner is asked to accept that contract.
#[derive(Debug, Clone)]
pub struct RunnerExecRequest {
    pub runner_id: String,
    pub options: RunnerExecOptions,
    pub sync_workspace: Option<String>,
    pub workspace_ref: Option<String>,
    pub hydrate_deps: bool,
    pub workspace_sync_timeout: Duration,
    pub artifact_outputs: Vec<String>,
    pub artifact_dir_outputs: Vec<String>,
    pub summary_outputs: Vec<String>,
}

impl RunnerExecRequest {
    pub fn new(runner_id: impl Into<String>, options: RunnerExecOptions) -> Self {
        Self {
            runner_id: runner_id.into(),
            options,
            sync_workspace: None,
            workspace_ref: None,
            hydrate_deps: false,
            workspace_sync_timeout: Duration::from_secs(240),
            artifact_outputs: Vec::new(),
            artifact_dir_outputs: Vec::new(),
            summary_outputs: Vec::new(),
        }
    }
}

/// Execute a request after creating and updating the durable owner for every
/// controller-side phase. `exec` intentionally remains available for callers
/// that already own all of this orchestration.
pub fn exec_request(mut request: RunnerExecRequest) -> Result<(RunnerExecOutput, i32)> {
    let has_declared_outputs = !request.artifact_outputs.is_empty()
        || !request.artifact_dir_outputs.is_empty()
        || !request.summary_outputs.is_empty();
    let run_id = persisted_run_id(request.options.run_id.take(), has_declared_outputs)?;
    request.options.run_id = run_id.clone();
    request.options.run_id_owns_generic_exec = run_id.is_some();

    let lifecycle_store = run_id
        .as_deref()
        .map(|_| AgentTaskLifecycleStore::from_environment())
        .transpose()?;
    let has_workspace_sync = request.sync_workspace.is_some();
    if has_workspace_sync {
        if let (Some(run_id), Some(lifecycle_store)) = (run_id.as_deref(), lifecycle_store.as_ref())
        {
            ensure_request_run(lifecycle_store, run_id, &request)?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_pre_handoff_phase(
                run_id,
                "workspace_sync",
            )?;
        }
    }
    let deadline = has_workspace_sync.then(|| Instant::now() + request.workspace_sync_timeout);
    let runner_id = request.runner_id.clone();
    let cwd = request.options.cwd.take();
    let sync_workspace = request.sync_workspace.take();
    let workspace_ref = request.workspace_ref.take();
    let hydrate_deps = request.hydrate_deps;
    let control = crate::WorkspaceControl::before(deadline);
    let control = if has_workspace_sync {
        control.with_progress()
    } else {
        control
    };
    let (cwd, source_snapshot, hydration_source) =
        bounded_workspace_preparation(control, move |control| {
            workspace_context(
                &runner_id,
                cwd,
                sync_workspace,
                workspace_ref,
                hydrate_deps,
                control,
            )
        })
        .map_err(|error| {
            if has_workspace_sync {
                finish_pre_handoff_failure(run_id.as_deref(), "workspace_sync", &error);
            }
            error
        })?;
    request.options.cwd = cwd;
    request.options.source_snapshot = source_snapshot;

    if request.hydrate_deps {
        if let (Some(run_id), Some(lifecycle_store)) = (run_id.as_deref(), lifecycle_store.as_ref())
        {
            if !has_workspace_sync {
                ensure_request_run(lifecycle_store, run_id, &request)?;
            }
            homeboy_agents::agent_task_lifecycle::record_runner_exec_pre_handoff_phase(
                run_id,
                "dependency_hydration",
            )?;
        }
        (|| {
            let local_path = hydration_source.ok_or_else(|| {
                Error::validation_invalid_argument(
                    "hydrate_deps",
                    "--hydrate-deps requires --sync-workspace or --workspace-ref",
                    None,
                    None,
                )
            })?;
            let remote_path = request.options.cwd.as_deref().ok_or_else(|| {
                Error::internal_unexpected("synced runner workspace is missing its remote path")
            })?;
            crate::hydrate_runner_workspace_dependencies(
                &request.runner_id,
                &local_path,
                remote_path,
            )
        })()
        .map_err(|error| {
            finish_pre_handoff_failure(run_id.as_deref(), "dependency_hydration", &error);
            error
        })?;
    }

    if let (Some(run_id), Some(lifecycle_store)) = (run_id.as_deref(), lifecycle_store.as_ref()) {
        ensure_request_run(lifecycle_store, run_id, &request)?;
        homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_declarations_in_store(
            lifecycle_store,
            run_id,
            &request.artifact_outputs,
            &request.artifact_dir_outputs,
            &request.summary_outputs,
        )?;
        let execution_record =
            RunnerExecutionRecord::planned(run_id, &request.runner_id, "dispatch")
                .with_orchestration_provenance(Some(crate::runner_exec_orchestration_provenance(
                    &request.runner_id,
                )?));
        homeboy_agents::agent_task_lifecycle::record_runner_exec_execution_record_in_store(
            lifecycle_store,
            run_id,
            &execution_record,
        )?;
    }

    let options = std::mem::take(&mut request.options);
    let (mut output, exit_code) = exec(&request.runner_id, options)?;
    if output.mirror_run_id.is_none() {
        output.mirror_run_id.clone_from(&run_id);
    }
    persist_execution_output(
        lifecycle_store.as_ref(),
        run_id.as_deref(),
        &request,
        &mut output,
        exit_code,
    )?;
    Ok((output, exit_code))
}

/// Keep the caller's wall-clock bound even during filesystem or provenance
/// operations that cannot cooperatively poll. The worker retains its deadline,
/// so it cannot advance to a later controlled stage after the caller returns.
fn bounded_workspace_preparation<T: Send + 'static>(
    control: crate::WorkspaceControl,
    work: impl FnOnce(&crate::WorkspaceControl) -> Result<T> + Send + 'static,
) -> Result<T> {
    let Some(deadline) = control.deadline() else {
        return work(&control);
    };
    let started = Instant::now();
    control.checkpoint()?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker_control = control.clone();
    std::thread::Builder::new()
        .name("workspace-sync".into())
        .spawn(move || {
            let result = work(&worker_control);
            let _ = sender.send(result);
        })
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("start workspace sync worker".into()),
            )
        })?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(control.timeout_error());
        }
        match receiver.recv_timeout(remaining.min(Duration::from_secs(10))) {
            Ok(result) => {
                control.checkpoint()?;
                if result.is_ok() {
                    eprintln!("sync: done in {:.1}s", started.elapsed().as_secs_f64());
                }
                return result;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                control.checkpoint()?;
                control.heartbeat();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::internal_unexpected(
                    "workspace synchronization worker stopped",
                ));
            }
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn uncooperative_sync_stages_cannot_extend_the_deadline() {
        for stage in [
            "scanning",
            "planning on runner",
            "transferring 0/1 MB",
            "applying on runner",
            "collecting source provenance",
        ] {
            let started = Instant::now();
            let control =
                crate::WorkspaceControl::before(Some(started + Duration::from_millis(300)))
                    .with_progress();
            control.stage(stage);
            let error = bounded_workspace_preparation(control, |_| {
                std::thread::sleep(Duration::from_secs(3));
                Ok(())
            })
            .unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(2));
            assert_eq!(error.details["workspace_sync"]["timed_out"], true);
            assert_eq!(error.details["workspace_sync"]["action"], stage);
            assert!(
                error.details["workspace_sync"]["timeout_ms"]
                    .as_u64()
                    .unwrap()
                    >= 300
            );
        }
    }
}

fn ensure_request_run(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    request: &RunnerExecRequest,
) -> Result<()> {
    let runner = crate::load(&request.runner_id)?;
    let remote_cwd = request
        .options
        .cwd
        .as_deref()
        .or(runner.workspace_root.as_deref())
        .unwrap_or(".");
    homeboy_agents::agent_task_lifecycle::ensure_generic_runner_exec_run_in_store(
        lifecycle_store,
        run_id,
        &request.runner_id,
        remote_cwd,
        &request.options.command,
    )?;
    Ok(())
}

fn workspace_context(
    runner_id: &str,
    cwd: Option<String>,
    sync_workspace: Option<String>,
    workspace_ref: Option<String>,
    verify_hydration_source: bool,
    control: &crate::WorkspaceControl,
) -> Result<(
    Option<String>,
    Option<homeboy_core::source_snapshot::SourceSnapshot>,
    Option<String>,
)> {
    if let Some(workspace_ref) = workspace_ref {
        if cwd.is_some() || sync_workspace.is_some() {
            return Err(Error::validation_invalid_argument(
                "workspace_ref",
                "--workspace-ref is mutually exclusive with --cwd and --sync-workspace",
                Some(workspace_ref),
                None,
            ));
        }
        let resolved = crate::resolve_workspace_ref(runner_id, &workspace_ref)?;
        if verify_hydration_source {
            crate::verify_workspace_ref_hydration_source(&resolved)?;
        }
        return Ok((
            Some(resolved.remote_path),
            Some(resolved.source_snapshot),
            Some(resolved.local_path),
        ));
    }
    let Some(local_path) = sync_workspace else {
        return Ok((cwd, None, None));
    };
    if cwd.is_some() {
        return Err(Error::validation_invalid_argument("cwd", "--cwd and --sync-workspace are mutually exclusive; --sync-workspace executes from the materialized runner path", None, None));
    }
    let options = crate::RunnerWorkspaceSyncOptions {
        path: local_path,
        mode: crate::RunnerWorkspaceSyncMode::Snapshot,
        controller_routed_git: false,
        changed_since_base: None,
        git_fetch_refs: Vec::new(),
        snapshot_includes: Vec::new(),
        allow_dirty_lab_workspace: false,
        validation_dependency_ids: None,
        run_isolation_token: None,
    };
    let (synced, _) = crate::sync_workspace(runner_id, options, control)?;
    control.stage("collecting source provenance");
    control.checkpoint()?;
    let mut source_snapshot = homeboy_core::source_snapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&synced.remote_path),
        synced.sync_mode.as_str(),
    );
    control.checkpoint()?;
    source_snapshot.workspace_snapshot_identity = Some(synced.snapshot_identity.clone());
    Ok((
        Some(synced.remote_path),
        Some(source_snapshot),
        Some(synced.local_path),
    ))
}

fn persisted_run_id(run_id: Option<String>, has_declared_outputs: bool) -> Result<Option<String>> {
    let run_id = run_id
        .or_else(|| has_declared_outputs.then(|| format!("runner-exec-{}", uuid::Uuid::new_v4())));
    match run_id {
        Some(run_id) if run_id.trim().is_empty() => Err(Error::validation_invalid_argument(
            "run_id",
            "runner exec --run-id must not be empty",
            Some(run_id),
            None,
        )),
        Some(run_id) => Ok(Some(run_id.trim().to_string())),
        None => Ok(None),
    }
}

fn finish_pre_handoff_failure(run_id: Option<&str>, phase: &str, error: &Error) {
    if let Some(run_id) = run_id {
        let _ = homeboy_agents::agent_task_lifecycle::finish_runner_exec_pre_handoff_failure(
            run_id, phase, phase, false, error,
        );
    }
}

fn persist_execution_output(
    lifecycle_store: Option<&AgentTaskLifecycleStore>,
    run_id: Option<&str>,
    request: &RunnerExecRequest,
    output: &mut RunnerExecOutput,
    exit_code: i32,
) -> Result<()> {
    let (Some(lifecycle_store), Some(run_id)) = (lifecycle_store, run_id) else {
        return Ok(());
    };
    if let Some(execution_record) = output.execution_record.as_ref() {
        homeboy_agents::agent_task_lifecycle::record_runner_exec_execution_record_in_store(
            lifecycle_store,
            run_id,
            execution_record,
        )?;
    }
    if let (Some(job), Some(events)) = (output.job.as_ref(), output.job_events.as_ref()) {
        homeboy_agents::agent_task_lifecycle::record_runner_exec_terminal_checkpoint_in_store(
            lifecycle_store,
            run_id,
            &RunnerJobLogSnapshot {
                job: job.clone(),
                events: events.clone(),
            },
        )?;
    }
    let artifacts = promote_declarations(
        lifecycle_store,
        run_id,
        output,
        "artifact",
        &request.artifact_outputs,
        crate::promote_runner_exec_artifacts_in_store,
    )?;
    let artifact_dirs = promote_declarations(
        lifecycle_store,
        run_id,
        output,
        "artifact_dir",
        &request.artifact_dir_outputs,
        crate::promote_runner_exec_artifact_dirs_in_store,
    )?;
    let summaries = promote_declarations(
        lifecycle_store,
        run_id,
        output,
        "summary",
        &request.summary_outputs,
        crate::promote_runner_exec_summaries_in_store,
    )?;
    let promoted_artifacts = artifacts
        .iter()
        .filter_map(|record| crate::promoted_output(output, record))
        .collect::<Vec<_>>();
    let promoted_artifact_dirs = artifact_dirs
        .iter()
        .filter_map(|record| crate::promoted_output(output, record))
        .collect::<Vec<_>>();
    let structured_summaries = summaries
        .iter()
        .filter_map(|record| crate::runner_exec_structured_summary(output, record))
        .collect::<Vec<_>>();
    let promoted_summaries = summaries
        .iter()
        .filter_map(|record| crate::promoted_output(output, record))
        .collect::<Vec<_>>();
    output.promoted_outputs.extend(promoted_artifacts);
    output.promoted_outputs.extend(promoted_artifact_dirs);
    output.structured_summaries.extend(structured_summaries);
    output.promoted_outputs.extend(promoted_summaries);
    let retained = artifacts
        .into_iter()
        .chain(artifact_dirs)
        .chain(summaries)
        .collect::<Vec<_>>();
    homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_refs_in_store(
        lifecycle_store,
        run_id,
        &retained,
    )?;
    if let (Some(job), Some(events)) = (output.job.as_ref(), output.job_events.as_ref()) {
        homeboy_agents::agent_task_lifecycle::project_terminal_runner_result_in_store(
            lifecycle_store,
            run_id,
            &RunnerJobLogSnapshot {
                job: job.clone(),
                events: events.clone(),
            },
        )?;
    } else if matches!(
        output.mode,
        RunnerTransport::DiagnosticSsh | RunnerTransport::Local
    ) {
        homeboy_agents::agent_task_lifecycle::finish_runner_exec_direct_in_store(
            lifecycle_store,
            run_id,
            if matches!(output.mode, RunnerTransport::DiagnosticSsh) {
                "diagnostic_ssh"
            } else {
                "local"
            },
            exit_code,
        )?;
    }
    Ok(())
}

fn promote_declarations(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    output: &RunnerExecOutput,
    kind: &str,
    declarations: &[String],
    promote: impl Fn(
        &AgentTaskLifecycleStore,
        &str,
        &RunnerExecOutput,
        &[String],
    ) -> Result<Vec<ArtifactRecord>>,
) -> Result<Vec<ArtifactRecord>> {
    let mut records = Vec::new();
    for declaration in declarations {
        let promoted = promote(store, run_id, output, std::slice::from_ref(declaration))?;
        homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion_in_store(
            store,
            run_id,
            kind,
            declaration,
            &promoted,
        )?;
        records.extend(promoted);
    }
    Ok(records)
}
