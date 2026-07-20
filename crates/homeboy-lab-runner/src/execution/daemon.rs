use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_workload_result;
use homeboy_core::api_jobs::{Job, JobEvent, JobStatus, RunnerJobLifecycleMetadata};
use homeboy_core::engine::command::CommandCaptureMetadata;
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::lab_contract::{run_location_index_path, LabRunnerWorkload};
use homeboy_core::redaction::redact_argv;
use homeboy_core::source_snapshot::SourceSnapshot;

use super::super::capabilities::{
    runner_capability_snapshot_for_preflight, validate_runner_capability_preflight,
};
use super::super::daemon_http_get::daemon_get;
use super::super::evidence::{
    local_job_run_id, mirror_daemon_evidence, mirror_daemon_job_progress, runner_exec_run_label,
    terminalize_mirrored_daemon_job,
};
use super::super::resource_metrics::RunnerResourceMetrics;
use super::super::{Runner, RunnerCapabilityPreflight, RunnerJob, RunnerKind};

#[allow(unused_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn exec_via_daemon(
    runner: &Runner,
    local_url: &str,
    accepted_session: Option<RunnerSession>,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
    secret_env_names: Vec<String>,
    capture_patch: bool,
    source_snapshot_override: Option<SourceSnapshot>,
    path_materialization_plan: Option<PathMaterializationPlan>,
    require_paths: Vec<String>,
    lab_runner_workload: Option<LabRunnerWorkload>,
    run_id: Option<String>,
    run_id_owns_generic_exec: bool,
    detach_after_handoff: bool,
    mirror_evidence: bool,
    print_handoff_output: bool,
    accepted_daemon_identity: Option<String>,
) -> Result<(RunnerExecOutput, i32)> {
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    let source_snapshot = source_snapshot_override.unwrap_or_else(|| {
        homeboy_core::source_snapshot::existing_remote(
            &runner.id,
            &cwd,
            runner.workspace_root.as_deref(),
        )
    });
    persist_runner_execution_transition(
        &RunnerExecutionRecord::planned(
            format!("runner-exec:{}:daemon", runner.id),
            runner.id.clone(),
            "daemon",
        )
        .with_path_materialization_plan(path_materialization_plan.clone())
        .with_orchestration_provenance(orchestration_target_provenance(
            runner,
            None,
            Some(&source_snapshot),
            &[],
        )),
        &cwd,
        &command,
    )?;
    let lifecycle = RunnerJobLifecycleMetadata {
        source: Some("runner-daemon".to_string()),
        kind: Some("runner.exec".to_string()),
        durable_run_id: run_id.clone(),
        ..Default::default()
    };
    let mut env = env;
    // `/exec` persists this environment with the accepted job. Snapshot the
    // command binary now so a later runner refresh cannot redirect queued or
    // running direct-daemon work.
    if !env.contains_key("HOMEBOY_COMMAND") {
        if let Some(homeboy_path) = runner.settings.homeboy_path.as_deref() {
            env.insert("HOMEBOY_COMMAND".to_string(), homeboy_path.to_string());
        }
    }
    let payload = json!({
        "runner_id": runner.id,
        "runner": runner,
        "project_id": project_id,
        "cwd": cwd,
        "command": command,
        "env": env,
        "secret_env_names": secret_env_names,
        "capture_patch": capture_patch,
        "source_snapshot": source_snapshot.clone(),
        "path_materialization_plan": path_materialization_plan.clone(),
        "require_paths": require_paths.clone(),
        "runner_workload": lab_runner_workload.clone(),
        "metadata": runner_exec_request_metadata(run_id.as_deref(), "daemon"),
        "lifecycle": lifecycle,
        // Explicit, first-class idempotency key the daemon dedupes `/exec` on.
        // The controller asserts it up front instead of the daemon having to
        // reconstruct it from nested lifecycle/metadata, so a resubmission after
        // a transport drop is a safe no-op. Uses the durable run id when present.
        "idempotency_key": run_id,
    });
    let response = submit_daemon_exec_with_session_recovery(
        local_url,
        accepted_session.as_ref(),
        |endpoint| {
            daemon_post_json_text(
                &client,
                endpoint,
                "/exec",
                &payload,
                DaemonPostOptions {
                    connection_close: true,
                },
            )
        },
        |accepted| recovered_daemon_submission_endpoint(&runner.id, accepted),
    )
    .map_err(|err| daemon_exec_loopback_transport_error(&runner.id, err))?;
    let status_code = response.status_code;
    let response_body = response.body;
    let envelope: DaemonEnvelope = serde_json::from_str(&response_body).map_err(|err| {
        // A stale/restarting daemon can answer the tunnel with a non-JSON or
        // empty body. Surface a clear, actionable error instead of a bare parse
        // failure so the caller knows to reconnect (#3631, #3624).
        daemon_exec_stale_response_error(&runner.id, status_code, &err.to_string())
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(daemon_exec_request_failed_error(
            &runner.id,
            status_code,
            &envelope,
        ));
    }

    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon exec returned no data"))?;
    let body = canonical_daemon_body(&data, "daemon exec response")?;
    let job_value = body
        .get("job")
        .ok_or_else(|| Error::internal_unexpected("daemon exec returned no job"))?;
    let mut job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon exec job".to_string()))
    })?;
    persist_runner_execution_transition(
        &RunnerExecutionRecord::in_flight(job.id.to_string(), runner.id.clone(), "daemon")
            .with_job_id(job.id.to_string())
            .with_path_materialization_plan(path_materialization_plan.clone())
            .with_orchestration_provenance(orchestration_target_provenance(
                runner,
                None,
                Some(&source_snapshot),
                &[],
            ))
            .with_next_actions(runner_execution_next_actions(
                &runner.id,
                &job.id.to_string(),
            )),
        &cwd,
        &command,
    )?;
    if let Some(run_id) = run_id.as_deref() {
        // The daemon has durably accepted this child. Bind it before waiting so
        // a lost controller still leaves cancellation and reconciliation with a
        // concrete runner job identity.
        if detach_after_handoff {
            // A detached handoff must durably transition the controller's pending
            // Lab handoff to accepted before the response can be lost, exactly as
            // the reverse-broker path does. Binding only the metadata runner job
            // id (below) leaves the typed handoff pending, so `runner_job_id()`
            // — which reads the accepted handoff first — reports no bound job and
            // the controller can expire or re-dispatch an already-accepted run.
            homeboy_agents::agent_task_lifecycle::record_detached_lab_run(
                homeboy_agents::agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: &runner.id,
                    runner_job_id: &job.id.to_string(),
                    remote_workspace: &cwd,
                    remote_command: &command,
                },
            )?;
        } else if run_id_owns_generic_exec {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                &runner.id,
                &job.id.to_string(),
                &cwd,
                &command,
            )?;
        } else {
            homeboy_agents::agent_task_lifecycle::record_runner_job_identity(
                run_id,
                &runner.id,
                &job.id.to_string(),
            )?;
        }
    }
    let persisted_run_id = mirror_evidence
        .then(|| persist_lab_offload_handoff_run(runner, &cwd, &command, &job, run_id.as_deref()))
        .flatten();
    validate_generic_exec_mirror_run_id(
        run_id_owns_generic_exec,
        run_id.as_deref(),
        persisted_run_id.as_deref(),
    )?;
    if detach_after_handoff {
        return Ok(detached_handoff_output(
            runner,
            RunnerExecMode::Daemon,
            cwd,
            command,
            source_snapshot,
            job,
            path_materialization_plan,
            require_paths,
            run_id,
            persisted_run_id,
        ));
    }

    let deadline = Instant::now() + runner_exec_wait_timeout();
    let mut daemon_endpoint = local_url.to_string();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events = fetch_daemon_events(&client, &daemon_endpoint, &job.id.to_string())
                .map(|events| redact_runner_job_events(&events, &env, &secret_env_names))
                .unwrap_or_default();
            return Err(daemon_job_wait_timeout(
                runner,
                &cwd,
                &command,
                &job,
                &events,
                "runner daemon job",
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
        let job_id = job.id.to_string();
        let (refreshed_job, refreshed_endpoint) = fetch_daemon_job_resilient_with_endpoint_reload(
            &client,
            &daemon_endpoint,
            &job_id,
            || refreshed_daemon_endpoint(&runner.id, accepted_daemon_identity.as_deref()),
        )
        .map_err(|err| {
            terminal_runner_poll_failure(
                runner,
                &cwd,
                &command,
                &job,
                "daemon",
                path_materialization_plan.as_ref(),
                &source_snapshot,
                &require_paths,
                persisted_run_id.as_deref(),
                accepted_daemon_identity.as_deref(),
                err,
            )
        })?;
        daemon_endpoint = refreshed_endpoint;
        job = refreshed_job;
    }
    let job_id = job.id.to_string();
    let mut events = match fetch_daemon_events(&client, &daemon_endpoint, &job_id) {
        Ok(events) => redact_runner_job_events(&events, &env, &secret_env_names),
        Err(err) => {
            return Err(lab_terminal_result_transport_error(
                runner, &cwd, &command, &job, err,
            ));
        }
    };
    append_agent_task_lifecycle_workload_event(
        &mut events,
        lab_runner_workload.as_ref(),
        &runner.id,
        &job_id,
    )?;

    let RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    } = runner_job_result_fields(&events, job.status, &env, &secret_env_names);

    let mirror = if mirror_evidence {
        mirror_daemon_evidence(
            runner,
            &cwd,
            &command,
            &job,
            &events,
            &result,
            run_id.as_deref(),
            lab_runner_workload
                .as_ref()
                .and_then(|workload| workload.notification_route.as_ref()),
        )?
    } else {
        None
    };
    let patch = mirror.as_ref().and_then(|evidence| evidence.patch.clone());
    let mirror_run_id = mirror.as_ref().map(|evidence| evidence.run.id.clone());
    validate_generic_exec_mirror_run_id(
        run_id_owns_generic_exec,
        run_id.as_deref(),
        mirror_run_id.as_deref(),
    )?;
    if let Some(run_id) = run_id.as_deref() {
        homeboy_agents::agent_task_lifecycle::project_terminal_runner_result(
            run_id,
            &homeboy_core::api_jobs::RunnerJobLogSnapshot {
                job: job.clone(),
                events: events.clone(),
            },
        )?;
    }
    let artifacts = job.artifacts.clone();
    let mutation_artifacts = mutation_artifacts_from_job(&job, &result);
    if print_handoff_output {
        print_lab_offload_handoff(
            &runner.id,
            Some(&cwd),
            &job.id.to_string(),
            mirror_run_id.as_deref(),
            DaemonJobHandoffState::Terminal(job.status),
        );
    }

    let runner_job = RunnerJob::from_job(&runner.id, "daemon", &command, Some(cwd.clone()), &job);
    let runner_result = runner_result(
        Some(&job),
        exit_code,
        &stdout,
        &stderr,
        mirror_run_id.as_deref(),
        mutation_artifacts.clone(),
    );
    let provenance_extensions = required_extensions_for_command(
        &command,
        &super::super::workload::merge_lab_runner_workload_required_extensions(
            Vec::new(),
            lab_runner_workload.as_ref(),
        ),
    );
    let handoff = lab_runner_handoff(
        runner,
        "daemon",
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );
    let execution_record = runner_execution_record_for_output(
        runner,
        "daemon",
        exit_code,
        Some(job.id.to_string()),
        mirror_run_id.clone(),
        Some(&source_snapshot),
        path_materialization_plan,
        &require_paths,
        &provenance_extensions,
        &artifacts,
        Some(&runner_result),
    );
    persist_runner_execution_transition(&execution_record, &cwd, &command)?;

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode: RunnerExecMode::Daemon,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: Some(source_snapshot.clone()),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            runner_job: Some(runner_job),
            job_events: Some(events),
            mirror_run_id: mirror_run_id.clone(),
            patch,
            mutation_artifacts,
            artifacts,
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics,
            capture,
            execution_record: Some(execution_record),
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        exit_code,
    ))
}

/// Keeps an admitted Lab offload visible in daemon active-job accounting until
/// its staged execution reaches a terminal or detached handoff outcome.
pub(crate) struct DaemonAdmissionReservation {
    local_url: String,
    job_id: String,
    token: Option<String>,
    renewer_stop: Option<Sender<()>>,
    renewer: Option<std::thread::JoinHandle<()>>,
    pub(crate) daemon_lease_id: String,
}

impl DaemonAdmissionReservation {
    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }
}

impl Drop for DaemonAdmissionReservation {
    fn drop(&mut self) {
        if let Some(stop) = self.renewer_stop.take() {
            let _ = stop.send(());
        }
        if let Some(renewer) = self.renewer.take() {
            let _ = renewer.join();
        }
        let Ok(client) = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
        else {
            return;
        };
        let _ = daemon_post_json_text(
            &client,
            &self.local_url,
            &format!("/admissions/{}/release", self.job_id),
            &self
                .token
                .as_ref()
                .map(|token| json!({ "admission_token": token }))
                .unwrap_or_else(|| json!({})),
            DaemonPostOptions::default(),
        );
    }
}

pub(crate) fn reserve_daemon_admission(
    runner_id: &str,
    local_url: &str,
    command: &str,
    expected_daemon_lease_id: &str,
    idempotency_key: Option<&str>,
) -> Result<DaemonAdmissionReservation> {
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| {
            Error::internal_unexpected(format!("build daemon admission client: {err}"))
        })?;
    let response = daemon_post_json_text(
        &client,
        local_url,
        "/admissions",
        &json!({
            "runner_id": runner_id,
            "command": command,
            "expected_daemon_lease_id": expected_daemon_lease_id,
            "idempotency_key": idempotency_key,
            "admission_lease_protocol": 1,
        }),
        DaemonPostOptions::default(),
    )?;
    let envelope: DaemonEnvelope = serde_json::from_str(&response.body).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse daemon admission response".to_string()),
        )
    })?;
    if response.status_code >= 400 || !envelope.success {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "runner `{runner_id}` refused Lab admission reservation: {}",
                envelope.error.unwrap_or(Value::Null)
            ),
            Some(runner_id.to_string()),
            None,
        ));
    }
    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon admission response missing data"))?;
    let body = canonical_daemon_body(&data, "daemon admission response")?;
    let daemon_lease_id = body
        .get("daemon_lease_id")
        .and_then(Value::as_str)
        .filter(|lease| !lease.is_empty())
        .ok_or_else(|| Error::internal_unexpected("daemon admission response missing lease ID"))?;
    if daemon_lease_id != expected_daemon_lease_id {
        return Err(Error::validation_invalid_argument(
            "expected_daemon_lease_id",
            format!(
                "runner `{runner_id}` admitted against daemon lease `{daemon_lease_id}`, expected `{expected_daemon_lease_id}`"
            ),
            Some(expected_daemon_lease_id.to_string()),
            None,
        ));
    }
    let job: Job = serde_json::from_value(body["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse daemon admission job".to_string()),
        )
    })?;
    let token = body
        .get("admission_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    let (renewer_stop, renewer) = match token.as_deref() {
        Some(token) => {
            let (stop, renewer) = spawn_admission_renewer(
                local_url.to_string(),
                job.id.to_string(),
                token.to_string(),
            );
            (Some(stop), Some(renewer))
        }
        // Older daemons ignore the opt-in marker and retain their legacy,
        // explicit-release-only reservation contract.
        None => (None, None),
    };
    Ok(DaemonAdmissionReservation {
        local_url: local_url.to_string(),
        job_id: job.id.to_string(),
        token,
        renewer_stop,
        renewer,
        daemon_lease_id: daemon_lease_id.to_string(),
    })
}

/// Renew at half the daemon's bounded lease interval while staging keeps the
/// handoff alive. Explicit release remains authoritative when the context ends.
fn spawn_admission_renewer(
    local_url: String,
    job_id: String,
    token: String,
) -> (Sender<()>, std::thread::JoinHandle<()>) {
    let (stop, shutdown) = mpsc::channel();
    let renewer = std::thread::spawn(move || {
        while shutdown.recv_timeout(Duration::from_secs(15)).is_err() {
            let Ok(client) = Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
            else {
                continue;
            };
            let _ = daemon_post_json_text(
                &client,
                &local_url,
                &format!("/admissions/{job_id}/renew"),
                &json!({ "admission_token": token }),
                DaemonPostOptions::default(),
            );
        }
    });
    (stop, renewer)
}

/// Recover only a connection that failed before the daemon could answer. A
/// timeout or response failure may already represent an accepted non-idempotent
/// `/exec`, so it is deliberately not retried here.
pub(super) fn submit_daemon_exec_with_session_recovery<Submit, Recover>(
    endpoint: &str,
    accepted_session: Option<&RunnerSession>,
    mut submit: Submit,
    mut recover: Recover,
) -> Result<DaemonHttpTextResponse>
where
    Submit: FnMut(&str) -> Result<DaemonHttpTextResponse>,
    Recover: FnMut(&RunnerSession) -> Result<String>,
{
    match submit(endpoint) {
        Ok(response) => Ok(response),
        Err(error) if daemon_submission_connection_was_lost(&error) => {
            let accepted_session = accepted_session.ok_or(error)?;
            let recovered_endpoint = recover(accepted_session)?;
            submit(&recovered_endpoint)
        }
        Err(error) => Err(error),
    }
}

fn daemon_submission_connection_was_lost(error: &Error) -> bool {
    error
        .details
        .pointer("/daemon_transport_error/kind")
        .and_then(Value::as_str)
        == Some("connect")
}

fn recovered_daemon_submission_endpoint(
    runner_id: &str,
    accepted_session: &RunnerSession,
) -> Result<String> {
    let accepted_lease = accepted_session
        .remote_daemon_lease_id
        .as_deref()
        .filter(|lease| !lease.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                format!(
                    "runner `{runner_id}` lost its submission tunnel without a proven daemon lease; refusing to submit through a replacement session"
                ),
                Some(runner_id.to_string()),
                None,
            )
        })?;
    let recovered = crate::connection::status_for_admission(runner_id)?;
    let session = recovered
        .session
        .filter(|_| recovered.connected)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "runner",
                format!("runner `{runner_id}` did not recover a connected daemon session"),
                Some(runner_id.to_string()),
                None,
            )
        })?;
    if session.mode != crate::RunnerTunnelMode::DirectSsh
        || session.remote_daemon_lease_id.as_deref() != Some(accepted_lease)
    {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "runner `{runner_id}` recovered a different daemon lease; refusing to submit a request proven for lease `{accepted_lease}`"
            ),
            Some(runner_id.to_string()),
            None,
        ));
    }
    session.local_url.ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            format!("runner `{runner_id}` recovered without a direct daemon endpoint"),
            Some(runner_id.to_string()),
            None,
        )
    })
}

pub(super) fn preflight_runner_capability_plan(
    runner: &Runner,
    preflight: Option<&RunnerCapabilityPreflight>,
    request_env: &HashMap<String, String>,
) -> Result<()> {
    let Some(preflight) = preflight else {
        return Ok(());
    };
    if preflight.is_empty() || runner.kind != RunnerKind::Ssh {
        return Ok(());
    }

    let capabilities = runner_capability_snapshot_for_preflight(runner, preflight)?;
    validate_runner_capability_preflight(&runner.id, preflight, &capabilities, request_env)
}

pub(super) fn fetch_daemon_job(client: &Client, local_url: &str, job_id: &str) -> Result<Job> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}"))?;
    let body = canonical_daemon_body(&data, "daemon job response")?;
    let job: Job = serde_json::from_value(body["job"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job".to_string()))
    })?;
    validate_daemon_job_identity(job_id, &job)?;
    Ok(job)
}

pub(super) fn validate_daemon_job_identity(requested_job_id: &str, job: &Job) -> Result<()> {
    let returned_job_id = job.id.to_string();
    if returned_job_id == requested_job_id {
        return Ok(());
    }

    Err(Error::new(
        ErrorCode::InternalUnexpected,
        format!(
            "runner daemon returned job `{returned_job_id}` while polling requested job `{requested_job_id}`"
        ),
        json!({
            "requested_job_id": requested_job_id,
            "returned_job_id": returned_job_id,
        }),
    ))
}

pub(super) fn detached_handoff_output(
    runner: &Runner,
    mode: RunnerExecMode,
    cwd: String,
    command: Vec<String>,
    source_snapshot: SourceSnapshot,
    job: Job,
    path_materialization_plan: Option<PathMaterializationPlan>,
    require_paths: Vec<String>,
    accepted_run_id: Option<String>,
    mirror_run_id: Option<String>,
) -> (RunnerExecOutput, i32) {
    let job_id = job.id.to_string();
    let record_path_materialization_plan = path_materialization_plan
        .clone()
        .or_else(|| fallback_path_materialization_plan(Some(&source_snapshot), &require_paths));
    print_lab_offload_handoff(
        &runner.id,
        Some(&cwd),
        &job_id,
        mirror_run_id.as_deref(),
        DaemonJobHandoffState::InFlight,
    );
    let envelope = homeboy_core::lab_contract::LabRunnerHandoffEnvelope::detached_lab_offload(
        &runner.id,
        &job_id,
        cwd.clone(),
        record_path_materialization_plan.clone(),
        accepted_run_id,
        mirror_run_id.clone(),
        job_timestamp_ms_to_rfc3339(job.updated_at_ms),
    );
    let stdout = serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| "{}".to_string());
    let transport = match mode {
        RunnerExecMode::ReverseBroker => "reverse_broker",
        _ => "daemon",
    };
    let runner_job = RunnerJob::from_job(&runner.id, transport, &command, Some(cwd.clone()), &job);
    let run_location_index_path = run_location_index_path(&cwd);
    let mut runner_result =
        runner_result(Some(&job), 0, &stdout, "", mirror_run_id.as_deref(), None);
    runner_result.artifact_refs.push(crate::RunnerArtifactRef {
        artifact_id: "run_location_index".to_string(),
        name: Some("run location index".to_string()),
        path: Some(run_location_index_path.clone()),
        url: None,
        mime: Some("application/json".to_string()),
        size_bytes: None,
        sha256: None,
        transport: Some(transport.to_string()),
    });
    let handoff = lab_runner_handoff(
        runner,
        transport,
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );
    let execution_record =
        RunnerExecutionRecord::in_flight(job_id.clone(), runner.id.clone(), transport.to_string())
            .with_job_id(job_id.clone())
            .with_mirror_run_id(mirror_run_id.clone())
            .with_path_materialization_plan(record_path_materialization_plan)
            .with_orchestration_provenance(orchestration_target_provenance(
                runner,
                None,
                Some(&source_snapshot),
                &[],
            ))
            .with_artifact_refs([RunnerExecutionArtifactRef {
                id: "run_location_index".to_string(),
                name: Some("run location index".to_string()),
                path: Some(run_location_index_path),
                url: None,
            }])
            .with_next_actions(runner_execution_next_actions(&runner.id, &job_id));

    (
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code: 0,
            stdout,
            stderr: String::new(),
            source_snapshot: Some(source_snapshot.clone()),
            job: Some(job.clone()),
            runner_job: Some(runner_job),
            job_id: Some(job.id.to_string()),
            job_events: None,
            mirror_run_id: mirror_run_id.clone(),
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: Some(execution_record),
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        0,
    )
}

fn job_timestamp_ms_to_rfc3339(timestamp_ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms as i64)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// Grace window during which a transient daemon polling failure (connection
/// refused while the daemon restarts, a stale tunnel returning `null`, etc.) is
/// retried instead of aborting the wait. A daemon-managed exec job persists its
/// status across restarts, so a brief reconnection gap should not cost the
/// caller the real terminal result of in-flight work (#4770, #3631, #3624).
pub(super) const DAEMON_POLL_TRANSIENT_GRACE: Duration = Duration::from_secs(30);
pub(super) const DAEMON_POLL_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Poll a daemon job, tolerating transient failures within the grace window.
///
/// The job store is durable across daemon restarts, so a connection error or a
/// `null` envelope during the restart window is recoverable: the daemon comes
/// back and serves the persisted (and possibly already-terminal) job. Only after
/// the grace window elapses without a successful read do we surface the error,
/// and we annotate it so the caller knows the remote job may still be in flight
/// rather than reporting a misleading hard failure.
pub(super) fn fetch_daemon_job_resilient(
    client: &Client,
    local_url: &str,
    job_id: &str,
) -> Result<Job> {
    fetch_daemon_job_resilient_with_endpoint_reload(client, local_url, job_id, || Ok(None))
        .map(|(job, _)| job)
}

pub(super) fn fetch_daemon_job_resilient_with_endpoint_reload<Reload>(
    client: &Client,
    local_url: &str,
    job_id: &str,
    reload_endpoint: Reload,
) -> Result<(Job, String)>
where
    Reload: Fn() -> Result<Option<String>>,
{
    let transient_deadline = Instant::now() + DAEMON_POLL_TRANSIENT_GRACE;
    let mut endpoint = local_url.to_string();
    loop {
        match fetch_daemon_job(client, &endpoint, job_id) {
            Ok(job) => return Ok((job, endpoint)),
            Err(err) => {
                if let Some(refreshed_endpoint) = reload_endpoint()? {
                    if refreshed_endpoint != endpoint {
                        endpoint = refreshed_endpoint;
                        continue;
                    }
                }
                if Instant::now() >= transient_deadline {
                    let mut surfaced = err;
                    surfaced.retryable = surfaced.retryable.or(Some(true));
                    return Err(surfaced.with_hint(format!(
                        "Lost contact with the runner daemon while polling job `{job_id}` for longer than {}s; the remote job may still be in flight. Reconnect with `homeboy runner connect <runner-id>` and inspect `homeboy runner job logs <runner-id> {job_id}`.",
                        DAEMON_POLL_TRANSIENT_GRACE.as_secs()
                    )));
                }
                std::thread::sleep(DAEMON_POLL_RETRY_BACKOFF);
            }
        }
    }
}

fn refreshed_daemon_endpoint(
    runner_id: &str,
    expected_identity: Option<&str>,
) -> Result<Option<String>> {
    let report = super::super::status(runner_id)?;
    let Some(session) = report.session.filter(|_| report.connected) else {
        return Ok(None);
    };
    if session.mode != crate::RunnerTunnelMode::DirectSsh {
        return Ok(None);
    }
    let Some(local_url) = session.local_url else {
        return Ok(None);
    };
    if let Some(expected_identity) = expected_identity.filter(|identity| !identity.is_empty()) {
        let actual_identity =
            super::super::daemon_endpoint_identity(&local_url).map_err(|error| {
                Error::internal_unexpected(format!(
                    "verify refreshed runner daemon identity: {error}"
                ))
            })?;
        if actual_identity.trim() != expected_identity.trim() {
            return Err(Error::validation_invalid_argument(
                "runner",
                format!(
                    "refreshed runner `{runner_id}` daemon identity `{actual_identity}` does not match the accepted daemon `{expected_identity}`"
                ),
                Some(runner_id.to_string()),
                None,
            ));
        }
    }
    Ok(Some(local_url))
}

pub(super) fn fetch_daemon_events(
    client: &Client,
    local_url: &str,
    job_id: &str,
) -> Result<Vec<JobEvent>> {
    let data = daemon_get(client, local_url, &format!("/jobs/{job_id}/events"))?;
    let body = canonical_daemon_body(&data, "daemon job events response")?;
    serde_json::from_value(body["events"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job events".to_string()))
    })
}

pub(super) fn daemon_job_context_error(
    runner_id: &str,
    job_id: &str,
    persisted_run_id: Option<&str>,
    err: Error,
) -> Error {
    let runner_exec_prefix = format!("homeboy runner exec {runner_id} --");
    let runner_runs_list =
        format!("{runner_exec_prefix} homeboy runs list --status running --limit 20");
    let runner_job_logs = format!("homeboy runner job logs {runner_id} {job_id} --follow");
    let runner_job_cancel = format!("homeboy runner job cancel {runner_id} {job_id}");
    let runner_run_show = format!("{runner_exec_prefix} homeboy runs show <run-id>");
    let runner_run_evidence = format!("{runner_exec_prefix} homeboy runs evidence <run-id>");
    let runner_run_artifacts = format!("{runner_exec_prefix} homeboy runs artifacts <run-id>");
    let persisted_run_show = persisted_run_id.map(|run_id| format!("homeboy runs show {run_id}"));
    let persisted_run_evidence =
        persisted_run_id.map(|run_id| format!("homeboy runs evidence {run_id}"));
    let persisted_run_artifacts =
        persisted_run_id.map(|run_id| format!("homeboy runs artifacts {run_id}"));
    let source_code = err.code.as_str();
    let source_message = err.message;
    let source_details = err.details;
    let source_hints = err.hints;
    let mut with_context = Error::new(
        ErrorCode::RunnerControllerDisconnected,
        format!(
            "Lost contact with runner `{runner_id}` daemon while polling known job `{job_id}`: {source_message}"
        ),
        json!({
            "status": "recoverable_followup_required",
            "runner_id": runner_id,
            "job_id": job_id,
            "persisted_run_id": persisted_run_id,
            "reason": "daemon_job_poll_failed",
            "recovery": {
                "mode": "durable_runner_job",
                "job_logs": runner_job_logs,
                "job_cancel": runner_job_cancel,
                "runner_runs_list": runner_runs_list,
                "runner_run_show": runner_run_show,
                "runner_run_evidence": runner_run_evidence,
                "runner_run_artifacts": runner_run_artifacts,
                "persisted_run_show": persisted_run_show,
                "persisted_run_evidence": persisted_run_evidence,
                "persisted_run_artifacts": persisted_run_artifacts,
            },
            "source": {
                "code": source_code,
                "message": source_message,
                "details": source_details,
            },
        }),
    );
    with_context.hints = source_hints;
    for hint in lab_offload_handoff_hints(
        runner_id,
        None,
        job_id,
        persisted_run_id,
        DaemonJobHandoffState::InFlight,
        true,
    ) {
        with_context = with_context.with_hint(hint);
    }
    with_context.retryable = Some(true);
    with_context
}

#[allow(clippy::too_many_arguments)]
pub(super) fn terminal_runner_poll_failure(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    transport: &str,
    path_materialization_plan: Option<&PathMaterializationPlan>,
    source_snapshot: &SourceSnapshot,
    _require_paths: &[String],
    persisted_run_id: Option<&str>,
    accepted_daemon_identity: Option<&str>,
    source: Error,
) -> Error {
    let job_id = job.id.to_string();
    let mut error = daemon_job_context_error(&runner.id, &job_id, persisted_run_id, source);
    error.retryable = Some(false);
    error.details["status"] = Value::String("terminal_failure".to_string());
    error.details["reason"] = Value::String("runner_job_unobservable".to_string());
    let current_daemon_identity = super::super::status(&runner.id).ok().and_then(|status| {
        status
            .session
            .and_then(|session| session.homeboy_build_identity)
    });
    if let Some(transition) =
        daemon_identity_transition(accepted_daemon_identity, current_daemon_identity.as_deref())
    {
        error.details["daemon_identity_transition"] = transition;
    }

    let diagnostic = json!({
        "error_code": error.code.as_str(),
        "message": error.message.clone(),
        "details": error.details.clone(),
    });
    let mirror_run_id = match terminalize_mirrored_daemon_job(
        runner,
        cwd,
        command,
        job,
        persisted_run_id,
        &diagnostic,
    ) {
        Ok(run) => Some(run.id),
        Err(persistence_error) => {
            error = error.with_hint(format!(
                "Could not persist terminal controller diagnostics for runner job `{job_id}`: {}",
                persistence_error.message
            ));
            None
        }
    };
    let record = RunnerExecutionRecord::terminal(job_id.clone(), runner.id.clone(), transport, 1)
        .with_job_id(job_id.clone())
        .with_mirror_run_id(mirror_run_id.clone())
        .with_path_materialization_plan(path_materialization_plan.cloned())
        .with_orchestration_provenance(orchestration_target_provenance(
            runner,
            None,
            Some(source_snapshot),
            &[],
        ))
        .with_next_actions(runner_execution_next_actions(&runner.id, &job_id));
    if let Err(persistence_error) = persist_runner_execution_transition(&record, cwd, command) {
        error = error.with_hint(format!(
            "Could not persist the terminal runner execution record for job `{job_id}`: {}",
            persistence_error.message
        ));
    }
    if let Some(run_id) = mirror_run_id {
        error.details["persisted_run_id"] = Value::String(run_id.clone());
        error = error.with_hint(format!(
            "Persisted terminal controller diagnostics as run `{run_id}`; inspect it with `homeboy runs show {run_id}`."
        ));
    }
    error
}

pub(super) fn daemon_identity_transition(
    accepted_identity: Option<&str>,
    current_identity: Option<&str>,
) -> Option<Value> {
    let (Some(from), Some(to)) = (accepted_identity, current_identity) else {
        return None;
    };
    (from != to).then(|| {
        json!({
            "status": "changed",
            "accepted_daemon_build_identity": from,
            "observed_daemon_build_identity": to,
        })
    })
}

pub(super) fn lab_terminal_result_transport_error(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    err: Error,
) -> Error {
    let job_id = job.id.to_string();
    let run_id = local_job_run_id(&runner.id, &job_id, &runner_exec_run_label(command));
    let mut error = Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!(
            "Lab offload runner `{}` daemon job `{job_id}` finished with status `{}`, but Homeboy could not retrieve or parse the daemon result report: {}. This is a Lab transport/reporting failure, not a remote command failure.",
            runner.id,
            job.status.daemon_status_label(),
            err.message
        ),
        json!({
            "runner_id": runner.id,
            "job_id": job_id,
            "persisted_run_id": run_id,
            "remote_cwd": cwd,
            "command": redact_argv(command),
            "job_status": job.status.daemon_status_label(),
            "source": err.details,
        }),
    );
    error.retryable = Some(true);
    for hint in lab_offload_handoff_hints(
        &runner.id,
        Some(cwd),
        &job_id,
        Some(&run_id),
        DaemonJobHandoffState::Terminal(job.status),
        true,
    ) {
        error = error.with_hint(hint);
    }
    error
        .with_hint(format!(
            "Recover the Lab result from persisted evidence instead of forcing local execution: `homeboy runs show {run_id}`, `homeboy runs evidence {run_id}`, and `homeboy runs artifacts {run_id}`."
        ))
        .with_hint(format!(
            "Inspect the daemon job report with `homeboy runner job logs {} {job_id}`.",
            runner.id
        ))
}

pub(super) fn daemon_job_wait_timeout(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    label: &str,
    supports_cancellation: bool,
) -> Error {
    let job_id = job.id.to_string();
    let mirrored = mirror_daemon_job_progress(runner, cwd, command, job, events, None);
    let mirrored_run_id = mirrored.as_ref().ok().map(|run| run.id.clone());
    let timeout_hint = format!(
        "Set controller-side `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}` before invoking homeboy to change this wait budget, e.g. `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}=2400 homeboy ...`; workload settings are applied inside the remote job and cannot extend the controller wait."
    );
    // Opt-in (#6891): when the operator set `HOMEBOY_RUNNER_CANCEL_ON_WAIT_TIMEOUT`,
    // best-effort cancel the still-running remote job so it stops holding its rig
    // lock. Off by default — the historical contract is preserved exactly.
    let cancel_outcome = attempt_wait_timeout_cancel(&runner.id, &job_id);
    let message_tail = match &cancel_outcome {
        WaitTimeoutCancelOutcome::Disabled => {
            "the remote job is still in flight and was not cancelled".to_string()
        }
        WaitTimeoutCancelOutcome::Cancelled => format!(
            "remote cancellation was requested on the runner job (opt-in `{RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV}`)"
        ),
        WaitTimeoutCancelOutcome::Failed(reason) => format!(
            "remote cancellation was requested (opt-in `{RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV}`) but failed: {reason}; the remote job may still be in flight"
        ),
    };
    let mut error = Error::internal_unexpected(format!(
        "{label} {job_id} on runner {} did not finish before timeout; {message_tail}",
        runner.id
    ));
    error.details["runner_id"] = Value::String(runner.id.clone());
    error.details["job_id"] = Value::String(job_id.clone());
    // The controller stopped waiting, not the daemon job. Preserve this
    // discriminator so the Lab adapter retains the durable handoff rather than
    // recording a pre-dispatch failure for an already accepted job.
    error.details["status"] = Value::String("controller_wait_expired".to_string());
    error.details["reason"] = Value::String("controller_wait_expired".to_string());
    error.details["remote_cwd"] = Value::String(cwd.to_string());
    error.details["command"] = json!(redact_argv(command));
    error.details["cancel_on_wait_timeout"] = Value::String(
        match &cancel_outcome {
            WaitTimeoutCancelOutcome::Disabled => "disabled",
            WaitTimeoutCancelOutcome::Cancelled => "requested",
            WaitTimeoutCancelOutcome::Failed(_) => "failed",
        }
        .to_string(),
    );
    match mirrored {
        Ok(run) => {
            error.details["active_run_id"] = Value::String(run.id.clone());
            error = error
                .with_hint(format!(
                    "Mirrored controller timeout state as run `{}`; inspect it with `homeboy runs show {}`.",
                    run.id, run.id
                ))
                .with_hint(format!(
                    "After the remote job finishes, run `homeboy runs artifacts {}` to refresh and list mirrored Lab artifacts without SSH temp-directory spelunking.",
                    run.id
                ));
        }
        Err(err) => {
            error = error.with_hint(format!(
                "Could not persist a local timeout mirror for remote job `{job_id}`: {}",
                err.message
            ));
        }
    }
    for hint in lab_offload_handoff_hints(
        &runner.id,
        Some(cwd),
        &job_id,
        mirrored_run_id.as_deref(),
        DaemonJobHandoffState::InFlight,
        supports_cancellation,
    ) {
        error = error.with_hint(hint);
    }
    match &cancel_outcome {
        WaitTimeoutCancelOutcome::Disabled => {}
        WaitTimeoutCancelOutcome::Cancelled => {
            error = error.with_hint(format!(
                "Opt-in `{RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV}` is set: requested remote cancellation of job `{job_id}` to release its rig lock. Confirm with `homeboy runner job logs {} {job_id}`.",
                runner.id
            ));
        }
        WaitTimeoutCancelOutcome::Failed(reason) => {
            error = error.with_hint(format!(
                "Opt-in `{RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV}` is set but remote cancellation failed: {reason}. Cancel manually with `homeboy runner job cancel {} {job_id}`.",
                runner.id
            ));
        }
    }
    error.retryable = Some(true);
    error.with_hint(timeout_hint)
}

pub(crate) fn result_event_data(events: &[JobEvent]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, homeboy_core::api_jobs::JobEventKind::Result))
        .and_then(|event| event.data.clone())
}

fn append_agent_task_lifecycle_workload_event(
    events: &mut Vec<JobEvent>,
    lab_runner_workload: Option<&LabRunnerWorkload>,
    runner_id: &str,
    runner_job_id: &str,
) -> Result<()> {
    let Some(result) = result_event_data(events) else {
        return Ok(());
    };
    let Some(event) = agent_task_run_plan_lifecycle_event_from_workload_result(
        lab_runner_workload,
        runner_id,
        runner_job_id,
        &result,
    )?
    else {
        return Ok(());
    };
    events.push(JobEvent {
        sequence: events
            .last()
            .map(|event| event.sequence.saturating_add(1))
            .unwrap_or(1),
        job_id: events
            .last()
            .map(|event| event.job_id)
            .unwrap_or_else(uuid::Uuid::nil),
        kind: homeboy_core::api_jobs::JobEventKind::Progress,
        timestamp_ms: events.last().map(|event| event.timestamp_ms).unwrap_or(0),
        message: Some("agent-task lifecycle event".to_string()),
        data: Some(json!({
            "schema": "homeboy/runner-workload-agent-task-lifecycle-event/v1",
            "agent_task_lifecycle_event": event,
        })),
    });
    Ok(())
}

/// Stream + metric fields derived from a runner job's terminal result event.
pub(super) struct RunnerJobResultFields {
    pub(super) result: Value,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) metrics: Option<RunnerResourceMetrics>,
    pub(super) capture: Option<CommandCaptureMetadata>,
    pub(super) exit_code: i32,
}

/// Extract the terminal result payload from a runner job's events and derive
/// the redacted streams, metrics, and exit code. Shared by the reverse-broker
/// and daemon execution paths to keep their result handling identical (#5067).
pub(super) fn runner_job_result_fields(
    events: &[JobEvent],
    job_status: JobStatus,
    redaction_env: &HashMap<String, String>,
    redaction_secret_env_names: &[String],
) -> RunnerJobResultFields {
    let result = result_event_data(events).unwrap_or_else(|| json!({}));
    let (stdout, stderr) = redact_runner_exec_streams(
        string_field(&result, "stdout"),
        string_field(&result, "stderr"),
        redaction_env,
        redaction_secret_env_names,
    );
    let metrics = result
        .get("metrics")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let capture = result
        .get("capture")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .unwrap_or_else(|| {
            if job_status == JobStatus::Succeeded {
                0
            } else {
                1
            }
        });
    RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    }
}
