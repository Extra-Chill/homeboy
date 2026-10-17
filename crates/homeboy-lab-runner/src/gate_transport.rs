//! Adapter from controller-owned gates to the existing native runner execution
//! and snapshot owners. It creates no separate job or lifecycle mechanism.

use std::path::Path;
use std::sync::Arc;

use homeboy_agents::agent_tasks::gate::placement::{
    LabGateReceipt, LabGateRequest, LabGateTransport,
};
use homeboy_agents::agent_tasks::gate::GateSupervision;
use homeboy_core::{Error, Result};

struct NativeLabGateTransport;

impl LabGateTransport for NativeLabGateTransport {
    fn declared_runner(&self, command: &str) -> Result<Option<String>> {
        crate::cli_resolver::resolve_gate_runner(command)
    }
    fn execute(
        &self,
        cwd: &Path,
        request: &LabGateRequest,
        supervision: Option<&GateSupervision>,
    ) -> Result<LabGateReceipt> {
        let run_id = format!("lab-gate-{}", uuid::Uuid::new_v4());
        homeboy_agents::agent_task_lifecycle::ensure_generic_runner_exec_run(
            &run_id,
            &request.runner_id,
            &cwd.display().to_string(),
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "gate-execute".to_string(),
            ],
        )?;
        let result = execute_owned(cwd, request, supervision, &run_id);
        match result {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                use std::io::Write;
                let mut file = tempfile::NamedTempFile::new()
                    .map_err(|error| Error::internal_io(error.to_string(), None))?;
                let diagnostic = homeboy_core::redaction::redact_json(
                    &serde_json::json!({"code":error.code.as_str(), "message":error.message, "details":error.details, "request_sha256":request.identity()?, "candidate":request.candidate}),
                );
                file.write_all(
                    &serde_json::to_vec(&diagnostic)
                        .map_err(|error| Error::internal_json(error.to_string(), None))?,
                )
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
                let store = homeboy_core::observation::ObservationStore::open_initialized()?;
                let artifact = store.record_artifact_with_metadata(&run_id, "lab-gate-diagnostic", file.path(), serde_json::json!({"visibility":request.visibility, "source":"native_gate_transport"}))?;
                let mut public = lab_gate_public_error(
                    &error,
                    format!("homeboy://run/{run_id}/artifact/{}", artifact.id),
                );
                if public.retryable.is_none() {
                    public.retryable = error.retryable;
                }
                homeboy_agents::agent_task_lifecycle::finish_runner_exec_pre_handoff_failure(
                    &run_id,
                    "lab_gate",
                    "transport",
                    false,
                    &public,
                )?;
                Err(public)
            }
        }
    }
}

const LAB_GATE_PUBLIC_MESSAGE: &str =
    "Lab gate transport did not produce verified passing evidence";

/// Whether a transport failure is a controller-side configuration error that
/// was rejected before any runner job existed (for example a missing runner
/// `secret_env` ref). It carries no gate output, so its message is safe to
/// surface, and nothing changes between retries, so it is not retryable
/// (#16121).
fn is_pre_handoff_configuration_error(error: &Error) -> bool {
    error.code.as_str().starts_with("validation.")
        && error.retryable != Some(true)
        && error
            .details
            .get("job_id")
            .is_none_or(serde_json::Value::is_null)
}

/// The bounded public error for a failed Lab gate transport. Gate output stays
/// in the private diagnostic artifact; a pre-handoff configuration error names
/// its redacted cause so `status` shows something an operator can act on.
fn lab_gate_public_error(error: &Error, artifact_ref: String) -> Error {
    let mut details = serde_json::json!({
        "stage": error.details.get("stage"),
        "gate_disposition": error.details.get("gate_disposition"),
        "job_id": error.details.get("job_id"),
        "artifact_ref": artifact_ref,
    });
    if !is_pre_handoff_configuration_error(error) {
        return Error::new(error.code, LAB_GATE_PUBLIC_MESSAGE, details);
    }
    let cause = homeboy_core::redaction::redact_string(&error.message);
    details["pre_handoff_configuration_error"] = serde_json::json!(true);
    details["cause_message"] = serde_json::json!(cause);
    let mut public = Error::new(
        error.code,
        format!("{LAB_GATE_PUBLIC_MESSAGE}: {cause}"),
        details,
    );
    public.retryable = Some(false);
    public
}

/// Secret identity names the admitted component's test capability declares.
///
/// A Lab gate runs `homeboy review test <component>` on the runner, where the
/// extension child requires its declared `test.secret_env` (for example a test
/// database). Declaring the names on the exec lets the runner resolve them from
/// its own runner-deferred `secret_env` refs; no controller value crosses
/// (#15824). Only names the component's test extension declares are requested,
/// so unrelated runner secrets never reach a gate. An unresolvable component or
/// declaration yields no names and the gate reports its own error, as before.
fn admitted_component_test_secret_env_names(
    admitted_component_id: Option<&str>,
    cwd: &Path,
) -> Vec<String> {
    let Ok(component) = homeboy_core::component::resolve_effective(
        admitted_component_id,
        Some(&cwd.to_string_lossy()),
        None,
    ) else {
        return Vec::new();
    };
    homeboy_core::extension::test::declared_secret_env_names(&component).unwrap_or_default()
}

fn lab_gate_exec_options(
    command: Vec<String>,
    run_id: &str,
    secret_env_names: Vec<String>,
) -> crate::RunnerExecOptions {
    crate::RunnerExecOptions {
        command,
        run_id: Some(run_id.to_string()),
        print_handoff: false,
        detach_after_handoff: true,
        secret_env_names,
        ..Default::default()
    }
}

/// One Lab workspace a gate materializes (its declared resource package or
/// its candidate snapshot).
///
/// The gate's durable controller run (`lab-gate-*`) owns the workspace lease,
/// so prune can resolve the owner's authority, and the run-scoped
/// [`crate::MaterializedWorkspace`] handle releases it on drop: reaped when the
/// gate fails before any runner job exists or once the accepted job's terminal
/// state is observed, and retained for job reconciliation when that terminal
/// state is never observed.
struct GateWorkspace {
    sync: crate::RunnerWorkspaceSyncOutput,
    handle: crate::MaterializedWorkspace,
}

impl GateWorkspace {
    /// The runner accepted the gate job that executes against this workspace.
    /// Until its terminal state is observed, the live job keeps the workspace.
    fn admitted(&mut self, job_id: &str, daemon_generation: Option<&str>) {
        self.handle
            .retain_for_reconciliation(job_id, daemon_generation);
    }

    /// The accepted gate job reached an authoritatively observed terminal state.
    fn terminal(&mut self, outcome: crate::WorkspaceTerminalOutcome) {
        self.handle.set_terminal_outcome(outcome);
    }
}

fn materialize_gate_workspace(
    runner_id: &str,
    path: &Path,
    mode: crate::RunnerWorkspaceSyncMode,
    run_id: &str,
) -> Result<GateWorkspace> {
    let (sync, _) = crate::sync_workspace(
        runner_id,
        crate::RunnerWorkspaceSyncOptions {
            path: path.display().to_string(),
            mode,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: vec![],
            snapshot_includes: vec![],
            allow_dirty_lab_workspace: false,
            validation_dependency_ids: None,
            run_isolation_token: Some(run_id.to_string()),
        },
    )?;
    let handle = crate::MaterializedWorkspace::new(
        runner_id.to_string(),
        sync.remote_path.clone(),
        None,
        crate::WorkspaceCleanupPolicy::DeleteAlways,
    );
    Ok(GateWorkspace { sync, handle })
}

fn execute_owned(
    cwd: &Path,
    request: &LabGateRequest,
    supervision: Option<&GateSupervision>,
    run_id: &str,
) -> Result<LabGateReceipt> {
    if supervision.is_some_and(|policy| (policy.is_cancelled)()) {
        return Err(Error::invalid_argument(
            "gate.placement",
            "Lab gate was cancelled before admission",
        ));
    }
    // Admission is observed before materializing any source or selecting
    // private HOME. Native exec rechecks the live daemon at submission.
    let admission = crate::runner_admission_snapshot(&request.runner_id).map_err(|mut error| {
        error.details["gate_disposition"] = serde_json::json!("unavailable");
        error.details["stage"] = serde_json::json!("admission");
        error
    })?;
    let status = &admission.status;
    if !admission.summary.accepting_jobs
        || !status.connected
        || status
            .daemon_freshness
            .as_ref()
            .is_none_or(|freshness| !freshness.fresh)
    {
        return Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "Lab gate runner admission is stale or unavailable",
            serde_json::json!({"gate_disposition":"unavailable", "stage":"admission"}),
        ));
    }
    let runner = crate::load(&request.runner_id)?;
    let resource_run = homeboy_core::engine::run_dir::RunDir::create()?;
    let resource_invocation = homeboy_core::engine::invocation::InvocationGuard::acquire(
        &resource_run,
        &Default::default(),
    )?;
    let resource_root = resource_invocation
        .context()
        .tmp_dir
        .join("declared-gate-resources");
    std::fs::create_dir_all(&resource_root)
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    let (sealed, sealed_provenance) =
        homeboy_agents::agent_tasks::gate::placement::seal_extension_resources(
            &resource_root,
            &request.environment.extension_inputs,
        )
        .map_err(|mut error| {
            error.details["stage"] = serde_json::json!("extension_materialization");
            error
        })?;
    // This non-secret descriptor makes an empty closure a normal owned
    // snapshot too. Private request files are never put in the source tree.
    std::fs::write(resource_root.join("resources.json"), b"{}")
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    let mut resources = sealed.clone();
    let mut snapshot_workspace = materialize_gate_workspace(
        &request.runner_id,
        &resource_root,
        crate::RunnerWorkspaceSyncMode::Snapshot,
        run_id,
    )?;
    let snapshot = snapshot_workspace.sync.clone();
    for resource in &mut resources {
        let relative = Path::new(&resource.source)
            .strip_prefix(&resource_root)
            .map_err(|_| {
                Error::invalid_argument(
                    "gate.resources",
                    "sealed extension is outside its owned resource package",
                )
            })?;
        resource.source = Path::new(&snapshot.remote_path)
            .join(relative)
            .display()
            .to_string();
    }
    let mut candidate_workspace = materialize_gate_workspace(
        &request.runner_id,
        cwd,
        crate::RunnerWorkspaceSyncMode::SnapshotGit,
        run_id,
    )?;
    let candidate = candidate_workspace.sync.clone();
    let binary = runner.settings.homeboy_path.as_deref().ok_or_else(|| {
        Error::invalid_argument(
            "gate.runtime",
            "Lab gate runner has no pinned Homeboy executable",
        )
    })?;
    let request_json = serde_json::to_vec(request).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("encode admitted Lab gate".to_string()),
        )
    })?;
    let request_file = private_input_file(
        &resource_invocation.context().tmp_dir,
        "gate-request.json",
        &request_json,
    )?;
    let resources_file = private_input_file(
        &resource_invocation.context().tmp_dir,
        "gate-resources.json",
        &serde_json::to_vec(&resources)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )?;
    let receipt_file = format!(
        "{}/.homeboy/gate-receipts/{}.json",
        snapshot.remote_path,
        uuid::Uuid::new_v4()
    );
    let mut command = vec![
        binary.to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "agent-task".to_string(),
        "gate-execute".to_string(),
        "--request".to_string(),
        format!("@{}", request_file.display()),
        "--resources".to_string(),
        format!("@{}", resources_file.display()),
        "--receipt-file".to_string(),
        receipt_file.clone(),
    ];
    let mut at_files = crate::lab_args::lab_at_file_specs(&command, cwd, &snapshot.remote_path)?;
    for spec in &mut at_files {
        spec.require_private();
    }
    crate::lab::offload::materialize_lab_at_files_on_runner(&request.runner_id, &at_files)?;
    command = crate::lab_args::remap_lab_at_file_args(&command, &at_files);
    let secret_env_names = admitted_component_test_secret_env_names(
        request.environment.admitted_component_id.as_deref(),
        cwd,
    );
    let options = lab_gate_exec_options(command, run_id, secret_env_names);
    let mut execution = crate::RunnerExecRequest::new(&request.runner_id, options);
    execution.workspace_ref = Some(candidate.workspace_ref);
    let daemon_generation = status
        .session
        .as_ref()
        .and_then(|session| session.remote_daemon_lease_id.clone());
    let output = match crate::exec_request(execution) {
        Ok((output, _)) => output,
        Err(error) => {
            // A lost exec response may still have been accepted by the daemon.
            // Keep that job's workspaces until reconciliation proves it ended.
            if let Some(job) =
                crate::lab::offload::accepted_runner_job_id(&request.runner_id, run_id)
            {
                for workspace in [&mut snapshot_workspace, &mut candidate_workspace] {
                    workspace.admitted(&job.id, job.daemon_generation.as_deref());
                }
            }
            return Err(error);
        }
    };
    let job_id = output.job_id.as_deref().ok_or_else(|| {
        Error::invalid_argument(
            "gate.receipt",
            "Lab gate output is missing its accepted job identity",
        )
    })?;
    for workspace in [&mut snapshot_workspace, &mut candidate_workspace] {
        workspace.admitted(job_id, daemon_generation.as_deref());
    }
    let started = std::time::Instant::now();
    let last_heartbeat = std::cell::Cell::new(started);
    let heartbeat_error = std::cell::RefCell::new(None);
    let cancelled = || {
        if let Some(policy) = supervision {
            if last_heartbeat.get().elapsed() >= policy.heartbeat_interval {
                last_heartbeat.set(std::time::Instant::now());
                let status = homeboy_agents::agent_tasks::gate::AgentTaskGateLiveStatus {
                    visibility: request.visibility,
                    reveal_policy: request.reveal_policy,
                    elapsed_ms: started.elapsed().as_millis(),
                    last_progress_ms_ago: None,
                    progress: None,
                    output_tail: "waiting for admitted runner gate".to_string(),
                };
                if let Err(error) = (policy.on_heartbeat)(&status) {
                    *heartbeat_error.borrow_mut() = Some(error);
                    return true;
                }
            }
            return (policy.is_cancelled)();
        }
        false
    };
    let terminal = crate::execution::observe_daemon_job_until_terminal(
        &request.runner_id,
        job_id,
        daemon_generation.as_deref(),
        std::time::Duration::from_secs(request.timeout_seconds.saturating_add(120)),
        &cancelled,
    );
    let terminal = match terminal {
        Ok(terminal) => terminal,
        Err(error) => {
            // Cancellation goes through the admitted job owner, then wait
            // for its authoritative terminal state before releasing gates.
            crate::runner_job_cancel(&request.runner_id, job_id)?;
            let cancelled_terminal = crate::execution::observe_daemon_job_until_terminal(
                &request.runner_id,
                job_id,
                None,
                std::time::Duration::from_secs(60),
                &|| false,
            )?;
            for workspace in [&mut snapshot_workspace, &mut candidate_workspace] {
                workspace.terminal(crate::WorkspaceTerminalOutcome::Cancelled);
            }
            let store = homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
            homeboy_agents::agent_task_lifecycle::project_terminal_runner_result_in_store(
                &store,
                run_id,
                &homeboy_core::api_jobs::RunnerJobLogSnapshot {
                    job: cancelled_terminal.job,
                    events: cancelled_terminal.events,
                },
            )?;
            let mut error = heartbeat_error.borrow_mut().take().unwrap_or(error);
            error.details["job_id"] = serde_json::json!(job_id);
            error.details["stage"] = serde_json::json!("terminal_wait");
            if supervision.is_some_and(|policy| (policy.is_cancelled)()) {
                error.details["gate_disposition"] = serde_json::json!("cancelled");
            }
            return Err(error);
        }
    };
    let outcome = if terminal.job.status == homeboy_core::api_jobs::JobStatus::Succeeded {
        crate::WorkspaceTerminalOutcome::Success
    } else {
        crate::WorkspaceTerminalOutcome::Failure
    };
    // The receipt is downloaded from the resource workspace below; the handles
    // reap both workspaces when this function returns.
    for workspace in [&mut snapshot_workspace, &mut candidate_workspace] {
        workspace.terminal(outcome);
    }
    let result = crate::execution::result_event_data(&terminal.events).ok_or_else(|| {
        Error::invalid_argument("gate.receipt", "terminal runner gate has no result event")
    })?;
    let stdout = result
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            Error::invalid_argument("gate.receipt", "terminal runner gate has no receipt stream")
        })?;
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|error| {
        Error::invalid_argument(
            "gate.receipt",
            format!(
                "Lab gate returned no canonical receipt: {error}; {}",
                if request.visibility
                    == homeboy_agents::agent_tasks::gate::AgentTaskGateVisibility::Private
                {
                    "private terminal stderr withheld"
                } else {
                    result
                        .get("stderr")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                }
            ),
        )
    })?;
    let location = value.get("data").unwrap_or(&value);
    if location.get("schema").and_then(serde_json::Value::as_str)
        != Some("homeboy/lab-gate-receipt-location/v1")
    {
        let stderr = result
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        return Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            if request.visibility
                == homeboy_agents::agent_tasks::gate::AgentTaskGateVisibility::Private
            {
                "private runner gate did not return terminal artifact evidence".to_string()
            } else {
                format!(
                    "runner gate did not return terminal artifact evidence: {}",
                    homeboy_core::redaction::redact_string(stderr)
                )
            },
            serde_json::json!({"stage":"terminal_projection", "job_id":job_id}),
        ));
    }
    let receipt_path = resource_invocation
        .context()
        .tmp_dir
        .join("received-gate-receipt.json");
    crate::lab::offload::lab_runner_file_transfer(&request.runner_id)?
        .download_file(&receipt_file, &receipt_path.display().to_string())?;
    let bytes = std::fs::read(&receipt_path)
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    if location.get("sha256").and_then(serde_json::Value::as_str)
        != Some(homeboy_engine_primitives::content_hash::sha256_hex(&bytes).as_str())
    {
        return Err(Error::invalid_argument(
            "gate.receipt",
            "terminal gate artifact content identity changed during transport",
        ));
    }
    let mut receipt: LabGateReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        Error::invalid_argument(
            "gate.receipt",
            format!("invalid terminal Lab gate receipt: {error}"),
        )
    })?;
    request.verify_receipt(&receipt)?;
    if (receipt.report.status == homeboy_agents::agent_tasks::gate::AgentTaskGateStatus::Succeeded
        || receipt
            .readiness
            .as_ref()
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            == Some("ready"))
        && (terminal.job.status != homeboy_core::api_jobs::JobStatus::Succeeded
            || result.get("exit_code").and_then(serde_json::Value::as_i64) != Some(0))
    {
        return Err(Error::invalid_argument(
            "gate.receipt",
            "passing gate artifact conflicts with the authoritative terminal job outcome",
        ));
    }
    if receipt.report.status != homeboy_agents::agent_tasks::gate::AgentTaskGateStatus::Unavailable
        && !request.preflight_only
    {
        homeboy_agents::agent_tasks::gate::placement::verify_extension_resource_closure(
            &sealed_provenance,
            &receipt.report.environment.extension_inputs,
        )?;
    }
    if receipt
        .execution_context
        .pointer("/context/runner_job_id")
        .and_then(serde_json::Value::as_str)
        != Some(job_id)
        || receipt
            .execution_context
            .pointer("/context/controller_run_id")
            .and_then(serde_json::Value::as_str)
            != Some(run_id)
    {
        return Err(Error::invalid_argument(
            "gate.receipt",
            "Lab gate receipt belongs to a different accepted runner job",
        ));
    }
    let actual_sources = serde_json::to_value(&receipt.report.environment.extension_inputs)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let (_, revalidated_provenance) =
        homeboy_agents::agent_tasks::gate::placement::seal_extension_resources(
            &resource_invocation
                .context()
                .tmp_dir
                .join("revalidated-resources"),
            &request.environment.extension_inputs,
        )?;
    homeboy_agents::agent_tasks::gate::placement::verify_extension_resource_closure(
        &sealed_provenance,
        &revalidated_provenance,
    )
    .map_err(|mut error| {
        error.details["stage"] = serde_json::json!("controller_closure_revalidation");
        error
    })?;
    for (actual, original) in receipt
        .report
        .environment
        .extension_inputs
        .iter_mut()
        .zip(&sealed_provenance)
    {
        actual.source = original.source.clone();
        for asset in &mut actual.shared_assets {
            let source = original
                .shared_assets
                .iter()
                .find(|original| original.path == asset.path)
                .ok_or_else(|| {
                    Error::invalid_argument(
                        "gate.resources",
                        "receipt widened the declared shared closure",
                    )
                })?;
            asset.source = source.source.clone();
        }
    }
    receipt.materialization = Some(
        serde_json::json!({"runner_extension_sources":actual_sources,
            "candidate_snapshot":candidate.snapshot_identity, "resource_snapshot":snapshot.snapshot_identity,
            "private_input_sha256":at_files.iter().map(|spec| &spec.content_sha256).collect::<Vec<_>>()}),
    );
    let run_id = output.mirror_run_id.as_deref().ok_or_else(|| {
        Error::invalid_argument(
            "gate.receipt",
            "native gate receipt has no durable evidence owner",
        )
    })?;
    let path = resource_invocation
        .context()
        .tmp_dir
        .join("terminal-gate-receipt.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&receipt)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
    .map_err(|error| Error::internal_io(error.to_string(), None))?;
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    let artifact = store.record_artifact_with_metadata(run_id, "admitted-lab-gate-receipt", &path, serde_json::json!({"candidate": request.candidate, "request_sha256": request.identity()?, "runner_job_id": job_id, "visibility": request.visibility}))?;
    receipt.artifact_ref = Some(format!("homeboy://run/{run_id}/artifact/{}", artifact.id));
    let lifecycle =
        homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_refs_in_store(
        &lifecycle,
        run_id,
        &[artifact],
    )?;
    homeboy_agents::agent_task_lifecycle::project_terminal_runner_result_in_store(
        &lifecycle,
        run_id,
        &homeboy_core::api_jobs::RunnerJobLogSnapshot {
            job: terminal.job,
            events: terminal.events,
        },
    )?;
    resource_run.finish(true);
    Ok(receipt)
}

fn private_input_file(root: &Path, name: &str, bytes: &[u8]) -> Result<std::path::PathBuf> {
    #[cfg(not(unix))]
    return Err(Error::invalid_argument(
        "gate.private_input",
        "private Lab inputs require owner-only filesystem guarantees",
    ));
    use std::io::Write;
    let path = root.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    Ok(path)
}

pub fn register() {
    homeboy_agents::agent_tasks::gate::placement::register_lab_gate_transport(Arc::new(
        NativeLabGateTransport,
    ));
}

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod workspace_tests;

#[cfg(test)]
mod secret_env_tests {
    use super::*;

    #[test]
    fn lab_gate_exec_declares_component_test_secret_names() {
        let names = vec![
            "WP_CODEBOX_DB_HOST".to_string(),
            "WP_CODEBOX_DB_PASSWORD".to_string(),
        ];
        let options =
            lab_gate_exec_options(vec!["homeboy".to_string()], "lab-gate-x", names.clone());
        assert_eq!(options.secret_env_names, names);
        assert!(
            options.env.is_empty(),
            "secret values never travel in request env"
        );
        assert_eq!(options.run_id.as_deref(), Some("lab-gate-x"));
        assert!(options.detach_after_handoff);
    }

    #[test]
    fn unresolvable_admitted_component_declares_no_secret_names() {
        let dir = tempfile::tempdir().expect("cwd");
        assert!(admitted_component_test_secret_env_names(
            Some("homeboy-15824-no-such-component"),
            dir.path()
        )
        .is_empty());
    }
}

#[cfg(test)]
mod public_error_tests {
    use super::*;

    fn missing_secret_ref() -> Error {
        Error::validation_invalid_argument(
            "secret_env",
            "missing runner secret env ref for WP_CODEBOX_DB_HOST",
            None,
            None,
        )
    }

    #[test]
    fn pre_handoff_configuration_error_surfaces_cause_and_is_terminal() {
        let public =
            lab_gate_public_error(&missing_secret_ref(), "homeboy://run/x/artifact/y".into());

        assert!(
            public
                .message
                .contains("missing runner secret env ref for WP_CODEBOX_DB_HOST"),
            "{}",
            public.message
        );
        assert!(public.message.starts_with(LAB_GATE_PUBLIC_MESSAGE));
        assert_eq!(public.details["pre_handoff_configuration_error"], true);
        assert_eq!(public.retryable, Some(false));
        assert_eq!(public.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn errors_after_runner_handoff_keep_the_bounded_public_message() {
        let mut error = missing_secret_ref();
        error.details = serde_json::json!({"job_id": "job-1", "stage": "execution"});

        let public = lab_gate_public_error(&error, "homeboy://run/x/artifact/y".into());

        assert_eq!(public.message, LAB_GATE_PUBLIC_MESSAGE);
        assert!(public.details.get("cause_message").is_none());
        assert_eq!(public.retryable, None);
    }

    #[test]
    fn transport_failures_keep_the_bounded_public_message() {
        let error = Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "ssh: connection reset",
            serde_json::json!({"stage": "transport"}),
        );

        let public = lab_gate_public_error(&error, "homeboy://run/x/artifact/y".into());

        assert_eq!(public.message, LAB_GATE_PUBLIC_MESSAGE);
        assert_eq!(public.retryable, None);
    }
}
