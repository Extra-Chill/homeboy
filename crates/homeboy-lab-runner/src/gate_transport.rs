//! Adapter from controller-owned gates to the existing native runner execution
//! and snapshot owners. It creates no separate job or lifecycle mechanism.

use std::path::Path;
use std::sync::Arc;

use homeboy_agents::agent_tasks::gate::placement::{
    LabGateAdmission, LabGateFreshness, LabGateReceipt, LabGateRequest, LabGateTransport,
    RunnerBuildSkew,
};
use homeboy_agents::agent_tasks::gate::GateSupervision;
use homeboy_core::{Error, Result};

struct NativeLabGateTransport;

impl LabGateTransport for NativeLabGateTransport {
    fn declared_runner(&self, command: &str) -> Result<Option<String>> {
        crate::cli_resolver::resolve_gate_runner(command)
    }
    fn admission(&self, runner_id: &str) -> Result<LabGateAdmission> {
        observe_gate_admission(runner_id).map(|(admission, _)| admission)
    }
    fn build_skew(&self, runner_id: &str) -> Result<Option<RunnerBuildSkew>> {
        let controller = crate::controller_identity::compatibility_controller_identity().version;
        Ok(crate::connection::read_session(runner_id)?
            .and_then(|session| runner_build_skew(&controller, &session.homeboy_version)))
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
                let mut file = crate::staging_temp::file()
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

/// Controller and runner release versions, when they differ. Build commits
/// are not compared: one release is one gate protocol.
fn runner_build_skew(controller: &str, runner: &str) -> Option<RunnerBuildSkew> {
    let release = |identity: &str| {
        let token = identity.split_whitespace().last().unwrap_or_default();
        token.split('+').next().unwrap_or_default().to_string()
    };
    let (controller, runner) = (release(controller), release(runner));
    (!controller.is_empty() && !runner.is_empty() && controller != runner)
        .then_some(RunnerBuildSkew { controller, runner })
}

const LAB_GATE_PUBLIC_MESSAGE: &str =
    "Lab gate transport did not produce verified passing evidence";

/// How long past the gate's own `timeout_seconds` the controller waits for the
/// runner job's terminal receipt. The runner bounds dependency setup and the
/// gate command by one deadline of `timeout_seconds` (#16938), so this margin
/// only has to cover job startup, workspace verification and receipt transfer.
/// A gate that runs out of time therefore writes its own TimedOut report well
/// before the controller would cancel the job.
const LAB_GATE_RECEIPT_MARGIN: std::time::Duration = std::time::Duration::from_secs(120);

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

/// Carry the redacted cause through the public transport error so the gate
/// report does not replace actionable diagnostics with a generic message.
fn lab_gate_public_error(error: &Error, artifact_ref: String) -> Error {
    let mut details = serde_json::json!({
        "entry_path": error.details.get("entry_path"),
        "stage": error.details.get("stage"),
        "gate_disposition": error.details.get("gate_disposition"),
        "job_id": error.details.get("job_id"),
        "readiness_reasons": error.details.get("readiness_reasons"),
        "artifact_ref": artifact_ref,
    });
    details["diagnostic"] = homeboy_core::redaction::redact_json(&serde_json::json!({
        "code": error.code.as_str(), "message": error.message, "details": error.details,
    }));
    let cause = homeboy_core::redaction::redact_string(&error.message);
    let configuration_error = is_pre_handoff_configuration_error(error);
    if configuration_error {
        details["pre_handoff_configuration_error"] = serde_json::json!(true);
    }
    details["cause_message"] = serde_json::json!(cause);
    let mut public = Error::new(
        error.code,
        format!("{LAB_GATE_PUBLIC_MESSAGE}: {cause}"),
        details,
    );
    public.retryable = if configuration_error {
        Some(false)
    } else {
        error.retryable
    };
    public
}

/// A terminal job without receipt evidence did not judge the candidate. Keep
/// this out of the validation/configuration class so Cook's existing transport
/// retry budget can regate it (#17841).
fn missing_terminal_receipt(job_id: &str, problem: &str) -> Error {
    Error::new(
        homeboy_core::ErrorCode::RunnerLabTransportFailure,
        problem,
        serde_json::json!({
            "field": "gate.receipt",
            "problem": problem,
            "job_id": job_id,
            "stage": "terminal_projection",
            "gate_disposition": "unavailable",
        }),
    )
    .with_retryable(true)
}

fn terminal_receipt_stream<'a>(result: &'a serde_json::Value, job_id: &str) -> Result<&'a str> {
    result
        .get("stdout")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            missing_terminal_receipt(job_id, "terminal runner gate has no receipt stream")
        })
}

/// Summarize a non-receipt envelope without copying arbitrary gate output.
fn terminal_envelope_diagnostic(value: &serde_json::Value, stdout_bytes: usize) -> String {
    let diagnostic = homeboy_core::redaction::redact_json(&serde_json::json!({
        "schema": value.get("schema"),
        "success": value.get("success"),
        "summary": value.get("summary"),
        "error_code": value.pointer("/diagnostics/code")
            .or_else(|| value.pointer("/error/code")),
        "stdout_bytes": stdout_bytes,
    }));
    diagnostic.to_string()
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

/// Losing sight of an admitted gate job is not evidence against it: the job
/// keeps running on the Lab, and cancelling it needs the same transport that
/// was just lost. Cancel only for a supervisor cancellation, a heartbeat
/// failure, an overrun budget, or an authoritative observation failure.
fn observation_failure_cancels_job(
    error: &Error,
    supervisor_cancelled: bool,
    heartbeat_failed: bool,
) -> bool {
    supervisor_cancelled
        || heartbeat_failed
        || !crate::execution::daemon_observation_was_lost(error)
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
    base: Option<&str>,
) -> Result<GateWorkspace> {
    let (sync, _) = crate::sync_workspace(
        runner_id,
        crate::RunnerWorkspaceSyncOptions {
            path: path.display().to_string(),
            mode,
            controller_routed_git: false,
            changed_since_base: base.map(str::to_string),
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

/// Observe the runner once and project the gate admission from it. Whether
/// the gate is admitted is unchanged (accepting, connected, fresh daemon); the
/// reasons come from the `RunnerAvailability` projection `runner status`
/// reports, so a refusal names why and whether waiting can clear it.
fn observe_gate_admission(
    runner_id: &str,
) -> Result<(LabGateAdmission, crate::RunnerAdmissionSnapshot)> {
    let runner = crate::load(runner_id)?;
    let exact = crate::lab::offload::metadata::require_exact_runner_version(&runner.settings);
    let snapshot = crate::runner_admission_snapshot(runner_id)?;
    let status = &snapshot.status;
    // Capabilities are not part of gate admission, so they are not projected
    // as a refusal reason here.
    let projected = crate::runner_status_admission_snapshot_for_status(
        crate::runner_admission_snapshot_for_status(status.clone()),
        runner.settings.concurrency_limit,
        true,
        exact,
    );
    use crate::lab::offload::metadata::{lab_runner_admission_refusal, LabRunnerAdmissionRefusal};
    let freshness = match lab_runner_admission_refusal(status, exact) {
        None => LabGateFreshness::Fresh,
        Some(LabRunnerAdmissionRefusal::LeaseNotFresh {
            transient: true, ..
        }) => LabGateFreshness::NotObserved,
        Some(refusal) => LabGateFreshness::Refused(refusal.code().to_string()),
    };
    let mut reasons = projected.summary.reasons;
    if !status.connected && !reasons.iter().any(|reason| reason == "not_connected") {
        reasons.insert(0, "not_connected".to_string());
    }
    let accepts_jobs = snapshot.summary.accepting_jobs
        && status.connected
        && status
            .daemon_freshness
            .as_ref()
            .is_some_and(|freshness| freshness.fresh);
    let admission = LabGateAdmission {
        accepts_jobs,
        reasons,
        freshness,
        next_action: snapshot.summary.next_action.clone(),
    };
    Ok((admission, snapshot))
}

/// Stage admitted data once per digest, independently of candidate workspaces.
pub(crate) fn materialize_gate_inputs(
    runner: &crate::Runner,
    inputs: &[homeboy_agents::agent_tasks::gate::data_inputs::GateInput],
) -> Result<Option<(String, Vec<serde_json::Value>)>> {
    if inputs.is_empty() {
        return Ok(None);
    }
    let root = format!(
        "{}/_homeboy_gate_inputs",
        runner
            .workspace_root
            .as_deref()
            .ok_or_else(|| Error::invalid_argument("gate_input", "runner needs workspace_root"))?
            .trim_end_matches('/')
    );
    let local_root = homeboy_agents::agent_tasks::gate::data_inputs::store_root()?;
    if runner.kind == crate::RunnerKind::Local {
        let mut evidence = Vec::new();
        for input in inputs {
            let local = input.path_in(&local_root)?;
            input.verify(&local)?;
            let hit = input.materialize(&local, Path::new(&root))?;
            evidence.push(serde_json::json!({"id":input.id,"digest":input.digest,"size_bytes":input.size_bytes,"store_hit":hit}));
        }
        return Ok(Some((root, evidence)));
    }
    let transfer = crate::transport::RunnerFileTransfer::for_runner(runner)?;
    fn upload(
        transfer: &crate::transport::RunnerFileTransfer,
        local: &Path,
        remote: &Path,
    ) -> Result<()> {
        let meta = std::fs::symlink_metadata(local)
            .map_err(|e| Error::internal_io(e.to_string(), None))?;
        if meta.is_dir() {
            transfer.ensure_directory(&remote.display().to_string())?;
            for entry in
                std::fs::read_dir(local).map_err(|e| Error::internal_io(e.to_string(), None))?
            {
                let entry = entry.map_err(|e| Error::internal_io(e.to_string(), None))?;
                upload(transfer, &entry.path(), &remote.join(entry.file_name()))?;
            }
        } else {
            let digest = homeboy_engine_primitives::content_hash::sha256_file(local)?;
            transfer.upload_gate_input(
                &local.display().to_string(),
                &remote.display().to_string(),
                &digest,
                meta.len(),
            )?;
        }
        Ok(())
    }
    let mut evidence = Vec::new();
    for input in inputs {
        let local = input.path_in(&local_root)?;
        input.verify(&local)?;
        let remote = input.path_in(Path::new(&root))?;
        let store_hit = transfer.gate_input_store_hit(&remote.display().to_string());
        evidence.push(serde_json::json!({"id":input.id,"digest":input.digest,"size_bytes":input.size_bytes,"store_hit":store_hit}));
        if store_hit {
            eprintln!("gate input {}: store hit ({})", input.id, input.digest);
            continue;
        }
        transfer.ensure_private_directory(&remote.parent().unwrap().display().to_string())?;
        upload(&transfer, &local, &remote)?;
        transfer.finish_gate_input(&remote.display().to_string())?;
    }
    Ok(Some((root, evidence)))
}

fn execute_owned(
    cwd: &Path,
    request: &LabGateRequest,
    supervision: Option<&GateSupervision>,
    run_id: &str,
) -> Result<LabGateReceipt> {
    if supervision.is_some_and(|policy| (policy.is_cancelled)()) {
        let mut error =
            Error::invalid_argument("gate.placement", "Lab gate was cancelled before admission");
        error.details["gate_disposition"] = serde_json::json!("cancelled");
        error.details["stage"] = serde_json::json!("admission");
        return Err(error);
    }
    // Admission is observed before materializing any source or selecting
    // private HOME. The controller already waited out transient refusals;
    // native exec rechecks the live daemon at submission.
    let (admission, snapshot) =
        observe_gate_admission(&request.runner_id).map_err(|mut error| {
            error.details["gate_disposition"] = serde_json::json!("unavailable");
            error.details["stage"] = serde_json::json!("admission");
            error
        })?;
    if !admission.accepts_jobs {
        return Err(admission.refusal(&request.runner_id, None));
    }
    let status = &snapshot.status;
    let runner = crate::load(&request.runner_id)?;
    let input_store = materialize_gate_inputs(&runner, &request.environment.data_inputs)?;
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
        None,
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
        homeboy_agents::agent_tasks::gate::component_lint_base(&request.label),
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
    let receipt_file = homeboy_lab_contract::lab::handoff::runner_workspace_gate_receipt_path(
        &snapshot.remote_path,
        &uuid::Uuid::new_v4().to_string(),
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
    let mut options = lab_gate_exec_options(command, run_id, secret_env_names);
    if let Some((root, _)) = &input_store {
        options
            .env
            .insert("HOMEBOY_GATE_INPUT_STORE".to_string(), root.clone());
    }
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
    let stop_requested = std::cell::Cell::new(false);
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
                    stop_requested.set(true);
                    return true;
                }
            }
            let cancelled = (policy.is_cancelled)();
            if cancelled {
                stop_requested.set(true);
            }
            return cancelled;
        }
        false
    };
    let terminal = crate::execution::observe_daemon_job_until_terminal(
        &request.runner_id,
        job_id,
        std::time::Duration::from_secs(request.timeout_seconds)
            .saturating_add(LAB_GATE_RECEIPT_MARGIN),
        &cancelled,
    );
    let terminal = match terminal {
        Ok(terminal) => terminal,
        Err(error)
            if !observation_failure_cancels_job(
                &error,
                supervision.is_some_and(|policy| (policy.is_cancelled)()),
                heartbeat_error.borrow().is_some(),
            ) =>
        {
            // The job is still the runner's to finish. Both workspaces stay
            // retained for reconciliation (see `GateWorkspace::admitted`), and
            // the gate is deferred, not failed.
            let mut error = error;
            error.retryable = Some(true);
            error.details["job_id"] = serde_json::json!(job_id);
            error.details["stage"] = serde_json::json!("terminal_wait");
            error.details["gate_disposition"] = serde_json::json!("unavailable");
            return Err(error.with_hint(format!(
                "Lab gate job `{job_id}` was left running after the controller lost its daemon transport. Reconnect with `homeboy runner connect {}` and recover the run to read its result.",
                request.runner_id
            )));
        }
        Err(error) => {
            let cancelled_terminal = settle_gate_observation_failure(
                error.clone(),
                job_id,
                stop_requested.get(),
                || {
                    crate::runner_job_cancel(
                        &request.runner_id,
                        job_id,
                        &format!("gate controller requested stop: {}", error.message),
                    )?;
                    Ok(())
                },
                || {
                    crate::execution::observe_daemon_job_until_terminal(
                        &request.runner_id,
                        job_id,
                        std::time::Duration::from_secs(60),
                        &|| false,
                    )
                },
            )?;
            for workspace in [&mut snapshot_workspace, &mut candidate_workspace] {
                workspace.terminal(crate::WorkspaceTerminalOutcome::Cancelled);
            }
            let store =
                homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
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
        // Setup may fail before the receipt-producing command starts.
        let evidence = serde_json::to_string(&terminal.events).unwrap_or_default();
        let storage = Error::internal_io(evidence, Some("runner gate materialization".into()));
        if storage.is_storage_exhausted() {
            storage.with_retryable(true)
        } else {
            missing_terminal_receipt(job_id, "terminal runner gate has no result event")
        }
    })?;
    let stdout = terminal_receipt_stream(&result, job_id)?;
    let private =
        request.visibility == homeboy_agents::agent_tasks::gate::AgentTaskGateVisibility::Private;
    let stderr = result
        .get("stderr")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    // gate-execute that printed no envelope still names its stderr (#17649).
    let value: serde_json::Value = decode_gate_json("gate_stdout", stdout.as_bytes(), private)
        .map_err(|mut error| {
            error.details["stage"] = serde_json::json!("terminal_projection");
            error.details["job_id"] = serde_json::json!(job_id);
            if !private {
                let lines = stderr.lines().collect::<Vec<_>>();
                let tail = lines[lines.len().saturating_sub(20)..].join("\n");
                error.details["stderr_tail"] =
                    serde_json::json!(homeboy_core::redaction::redact_string(&tail));
            }
            error
        })?;
    let location = value.get("data").unwrap_or(&value);
    if location.get("schema").and_then(serde_json::Value::as_str)
        != Some("homeboy/lab-gate-receipt-location/v1")
    {
        return Err(Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            if request.visibility
                == homeboy_agents::agent_tasks::gate::AgentTaskGateVisibility::Private
            {
                "private runner gate did not return terminal artifact evidence".to_string()
            } else {
                format!(
                    "runner gate did not return terminal artifact evidence: {}; stderr: {}",
                    terminal_envelope_diagnostic(&value, stdout.len()),
                    homeboy_core::redaction::redact_string(stderr)
                )
            },
            serde_json::json!({"stage":"terminal_projection", "job_id":job_id,
                "diagnostic": terminal_envelope_diagnostic(&value, stdout.len())}),
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
    let mut receipt: LabGateReceipt = decode_gate_json("gate_receipt", &bytes, private)?;
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
            "gate_inputs": input_store.map(|(_, evidence)| evidence).unwrap_or_default(),
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
        homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
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

/// Observation alone never authorizes stopping a remote job. Only an explicit
/// supervision stop (including a heartbeat failure) or the gate deadline does.
/// Until cancellation and terminal observation both succeed, workspace handles
/// remain admitted and are retained for reconciliation on drop.
fn settle_gate_observation_failure<T>(
    error: Error,
    job_id: &str,
    stop_requested: bool,
    cancel: impl FnOnce() -> Result<()>,
    observe_terminal: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let deadline = error.details.get("daemon_terminal_wait_timeout").is_some();
    if !stop_requested && !deadline {
        return Err(gate_observation_error(error, job_id));
    }
    cancel().map_err(|error| gate_observation_error(error, job_id))?;
    observe_terminal().map_err(|error| gate_observation_error(error, job_id))
}

/// Gate payloads keep the `gate.receipt` contract; private gates withhold the
/// payload excerpt while retaining stage, position, category and size.
fn decode_gate_json<T: serde::de::DeserializeOwned>(
    stage: &str,
    payload: &[u8],
    private: bool,
) -> Result<T> {
    homeboy_core::transport_json::decode(stage, payload).map_err(|decode| {
        let mut details = decode.details;
        if private {
            details["payload_excerpt"] = serde_json::json!("private gate payload withheld");
        }
        let mut error = Error::invalid_argument(
            "gate.receipt",
            format!(
                "Lab gate {stage} unparseable ({} bytes) at line {} column {}: {}",
                payload.len(),
                details["line"],
                details["column"],
                details["error"].as_str().unwrap_or("invalid JSON")
            ),
        );
        error.details["transport_json"] = details;
        error
    })
}

#[cfg(test)]
#[test]
fn private_gate_decode_withholds_payload_excerpt() {
    for private in [false, true] {
        let error = decode_gate_json::<serde_json::Value>("gate_stdout", b"{secret-token", private)
            .unwrap_err();
        assert_eq!(error.details["field"], "gate.receipt");
        assert_eq!(error.details["transport_json"]["stage"], "gate_stdout");
        let excerpt = error.details["transport_json"]["payload_excerpt"].to_string();
        assert_eq!(excerpt.contains("secret-token"), !private, "{excerpt}");
    }
}

fn gate_observation_error(mut error: Error, job_id: &str) -> Error {
    // After admission, a lost or retryable transport is an observation gap,
    // not a gate outcome or a configuration refusal.
    let transport_lost = crate::daemon_health::runner_daemon_health_failure(&error).is_some()
        || error.retryable == Some(true);
    if transport_lost {
        error.code = homeboy_core::ErrorCode::RunnerLabTransportFailure;
        error.retryable = Some(true);
        error.details["gate_disposition"] = serde_json::json!("unavailable");
    }
    error.details["job_id"] = serde_json::json!(job_id);
    error.details["stage"] = serde_json::json!("terminal_wait");
    error
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
    homeboy_agents::agent_tasks::gate::placement::register_runner_identity_scope(|| {
        Box::new(crate::connection::StagingIdentityScope::enter())
    });
}

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod workspace_tests;

#[cfg(test)]
mod observation_tests;

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
mod observation_failure_tests {
    use super::*;
    use crate::daemon_http_get::{daemon_transport_error, DaemonHttpErrorKind};

    #[test]
    fn only_a_decisive_observation_failure_cancels_the_gate_job() {
        let lost = || {
            daemon_transport_error(
                DaemonHttpErrorKind::Connect,
                "/jobs/x",
                None,
                "query runner daemon",
                "refused",
            )
        };
        let mut overran =
            Error::internal_unexpected("runner job `x` did not reach a terminal status");
        overran.details =
            serde_json::json!({"daemon_terminal_wait_timeout": {"runner_job_id": "x"}});
        overran.retryable = Some(true);
        let cases = [
            ("lost transport", lost(), false, false, false),
            (
                "no current session",
                crate::connection::no_current_daemon_session_error("lab"),
                false,
                false,
                false,
            ),
            ("budget overrun", overran, false, false, true),
            (
                "unknown job",
                Error::validation_invalid_argument("job", "job not found", None, None),
                false,
                false,
                true,
            ),
            ("supervisor cancelled", lost(), true, false, true),
            ("heartbeat failed", lost(), false, true, true),
        ];
        for (name, error, supervisor_cancelled, heartbeat_failed, cancels) in cases {
            assert_eq!(
                observation_failure_cancels_job(&error, supervisor_cancelled, heartbeat_failed),
                cancels,
                "{name}"
            );
        }
    }
}

#[cfg(test)]
mod public_error_tests {
    use super::*;

    #[test]
    fn non_receipt_envelope_names_the_refusal_and_stream_size() {
        let value = serde_json::json!({
            "schema":"homeboy/command-result/v1", "success":false,
            "summary":"Lab gate workspace differs from the controller candidate",
            "diagnostics":{"code":"validation.invalid_argument"},
            "stdout":"arbitrary private gate output",
        });
        let diagnostic = terminal_envelope_diagnostic(&value, 14000000);
        assert!(diagnostic.contains("validation.invalid_argument"));
        assert!(diagnostic.contains("workspace differs"));
        assert!(diagnostic.contains("14000000"));
        assert!(!diagnostic.contains("arbitrary private gate output"));
    }

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
    fn errors_after_runner_handoff_preserve_the_redacted_cause() {
        let mut error = missing_secret_ref();
        error.details = serde_json::json!({"job_id": "job-1", "stage": "execution"});

        let public = lab_gate_public_error(&error, "homeboy://run/x/artifact/y".into());

        assert!(public.message.contains(&error.message));
        assert_eq!(public.details["diagnostic"]["code"], error.code.as_str());
        assert_eq!(public.details["diagnostic"]["details"]["job_id"], "job-1");
        assert_eq!(public.retryable, None);
    }

    #[test]
    fn content_refusal_preserves_workspace_relative_entry_path() {
        let mut error = Error::validation_invalid_argument(
            "workspace",
            "workspace content hash refused a symlink cycle at 'tracked/cycle'",
            Some("tracked/cycle".into()),
            None,
        );
        error.details["entry_path"] = serde_json::json!("tracked/cycle");
        let public = lab_gate_public_error(&error, "homeboy://run/x/artifact/y".into());
        assert!(public.message.contains("tracked/cycle"));
        assert_eq!(public.details["entry_path"], "tracked/cycle");
    }

    #[test]
    fn terminal_gate_without_receipt_is_retryable_transport_not_configuration() {
        for result in [
            serde_json::json!({"exit_code": 1}),
            serde_json::json!({"stdout": null}),
        ] {
            let error = terminal_receipt_stream(&result, "job-no-receipt").unwrap_err();
            let public = lab_gate_public_error(&error, "homeboy://diagnostic".into());
            assert_eq!(
                public.code,
                homeboy_core::ErrorCode::RunnerLabTransportFailure
            );
            assert_eq!(public.retryable, Some(true));
            assert_eq!(public.details["job_id"], "job-no-receipt");
            assert_eq!(public.details["stage"], "terminal_projection");
            assert_eq!(public.details["gate_disposition"], "unavailable");
            assert!(!is_pre_handoff_configuration_error(&public));
            assert!(public
                .message
                .contains("terminal runner gate has no receipt stream"));
        }
        // A present stream is left to receipt verification, not reclassified
        // based on the runner's failing exit status.
        let result = serde_json::json!({"exit_code": 3, "stdout": "failing receipt"});
        assert_eq!(
            terminal_receipt_stream(&result, "job-failed").unwrap(),
            "failing receipt"
        );
        let absent_result =
            missing_terminal_receipt("job-no-result", "terminal runner gate has no result event");
        assert_eq!(absent_result.retryable, Some(true));
        assert!(!is_pre_handoff_configuration_error(&absent_result));
    }

    #[test]
    fn transport_failures_preserve_the_cause() {
        let error = Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "ssh: connection reset",
            serde_json::json!({"stage": "transport"}),
        );

        let public = lab_gate_public_error(&error, "homeboy://run/x/artifact/y".into());

        assert!(public.message.contains("ssh: connection reset"));
        assert_eq!(
            public.details["diagnostic"]["details"]["stage"],
            "transport"
        );
        assert_eq!(public.retryable, None);
    }
}
