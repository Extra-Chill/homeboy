use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_workload_result;
use homeboy_core::api_jobs::{Job, JobEvent, JobStatus, RunnerJobLifecycleMetadata};
use homeboy_core::engine::command::CommandCaptureMetadata;
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::lab_contract::{run_location_index_path, JobArtifactMetadata, LabRunnerWorkload};
use homeboy_core::redaction::redact_argv;
use homeboy_core::source_snapshot::SourceSnapshot;

use super::super::capabilities::{
    runner_capability_snapshot_for_preflight, validate_runner_capability_preflight,
};
use super::super::daemon_http_get::daemon_get;
use super::super::evidence::{local_job_run_id, runner_exec_run_label};
use super::super::evidence::{
    mirror_daemon_evidence, mirror_daemon_job_progress, terminalize_mirrored_daemon_job,
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
    extension_env_providers: Vec<String>,
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
    let workspace_owner_request = if run_id_owns_generic_exec {
        None
    } else {
        run_id
            .as_deref()
            .map(homeboy_agents::agent_task_lifecycle::workspace_owner_registration_if_present)
            .transpose()?
            .flatten()
    };
    if workspace_owner_request.is_some() {
        require_daemon_workspace_owner_lease_v2(&client, local_url)?;
    }
    let mut payload = json!({
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
        "extension_env_providers": extension_env_providers.clone(),
        "runner_workload": lab_runner_workload.clone(),
        "metadata": runner_exec_request_metadata(run_id.as_deref(), "daemon", &runner.id),
        "lifecycle": lifecycle,
        // Explicit, first-class idempotency key the daemon dedupes `/exec` on.
        // The controller asserts it up front instead of the daemon having to
        // reconstruct it from nested lifecycle/metadata, so a resubmission after
        // a transport drop is a safe no-op. Uses the durable run id when present.
        "idempotency_key": run_id,
    });
    if let Some((workspace, owner_id)) = workspace_owner_request.as_ref() {
        payload["workspace_owner_request"] = json!({
            "workspace": workspace,
            "owner_id": owner_id,
            "ttl_ms": homeboy_core::workspace_claim::MAX_WORKSPACE_CLAIM_TTL_MS,
        });
    }
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
    let job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon exec job".to_string()))
    })?;
    let roots = homeboy_core::paths::PathRoots::from_environment()?;
    let lease_store = run_id
        .as_deref()
        .map(|_| ObservationStore::open_initialized_in_roots(&roots))
        .transpose()?;
    let foreground_source_lease = std::cell::RefCell::new(None);
    let daemon_endpoint = std::cell::RefCell::new(local_url.to_string());
    return complete_submitted_runner_job(
        SubmittedRunnerJobFlow {
            runner,
            mode: RunnerExecMode::Daemon,
            transport: "daemon",
            runner_job_transport: "daemon",
            timeout_label: "runner daemon job",
            cwd: cwd.clone(),
            command: command.clone(),
            redaction_env: &env,
            secret_env_names: &secret_env_names,
            source_snapshot: source_snapshot.clone(),
            path_materialization_plan: path_materialization_plan.clone(),
            require_paths: require_paths.clone(),
            lab_runner_workload: lab_runner_workload.clone(),
            run_id: run_id.clone(),
            run_id_owns_generic_exec,
            detach_after_handoff,
            mirror_evidence,
            print_handoff_output,
            handoff_endpoint: None,
        },
        job,
        |job| {
            if let Some(session) = accepted_session.as_ref() {
                super::super::generation_store::record_job(
                    &runner.id,
                    session,
                    &job.id.to_string(),
                )?;
                if let Some(durable_run_id) = run_id.as_deref() {
                    super::super::generation_store::record_job_run(
                        &runner.id,
                        session,
                        &job.id.to_string(),
                        durable_run_id,
                    )?;
                }
            }
            Ok(())
        },
        |job| {
            if let Some(run_id) = run_id.as_deref() {
                let token = uuid::Uuid::new_v4().to_string();
                let store = lease_store
                    .as_ref()
                    .expect("lease store is opened whenever run_id is present");
                let runner_job_id = job.id.to_string();
                let claimed = if run_id_owns_generic_exec {
                    store.bind_and_claim_running_runner_exec_source(
                        run_id,
                        "foreground-runner-exec",
                        &token,
                        &runner_job_id,
                    )?
                } else {
                    store.claim_running_runner_exec_recovery_source(
                        run_id,
                        "foreground-runner-exec",
                        &token,
                        &runner_job_id,
                    )?
                };
                if !claimed {
                    return Err(runner_exec_source_claim_error(
                        store,
                        run_id,
                        &runner_job_id,
                    )?);
                }
                *foreground_source_lease.borrow_mut() = Some((run_id.to_string(), token));
            }
            Ok(())
        },
        |current| {
            if let Some((run_id, token)) = foreground_source_lease.borrow().as_ref() {
                lease_store
                    .as_ref()
                    .expect("lease store is opened whenever a lease is held")
                    .renew_running_runner_exec_source_lease(run_id, token)?;
            }
            let job_id = current.id.to_string();
            let (refreshed, endpoint) = fetch_daemon_job_resilient_with_endpoint_reload(
                &client,
                &daemon_endpoint.borrow(),
                &job_id,
                || {
                    refreshed_daemon_endpoint(
                        &runner.id,
                        &job_id,
                        accepted_daemon_identity.as_deref(),
                    )
                },
            )
            .map_err(|err| {
                terminal_runner_poll_failure(
                    runner,
                    &cwd,
                    &command,
                    current,
                    "daemon",
                    path_materialization_plan.as_ref(),
                    &source_snapshot,
                    &require_paths,
                    None,
                    accepted_daemon_identity.as_deref(),
                    err,
                )
            })?;
            *daemon_endpoint.borrow_mut() = endpoint;
            Ok(refreshed)
        },
        |job| {
            fetch_daemon_events(&client, &daemon_endpoint.borrow(), &job.id.to_string()).map_err(
                |error| lab_terminal_result_transport_error(runner, &cwd, &command, job, error),
            )
        },
        |job, events, result| {
            let request = crate::evidence::MirrorEvidenceRequest::new(
                runner,
                &cwd,
                &command,
                job,
                events,
                result,
                run_id.as_deref(),
                lab_runner_workload
                    .as_ref()
                    .and_then(|workload| workload.notification_route.as_ref()),
            );
            let request = if run_id_owns_generic_exec {
                request.with_generic_runner_exec_run()
            } else if run_id.is_some() {
                request.with_agent_task_run()
            } else {
                request
            };
            mirror_daemon_evidence(request, &roots).and_then(|evidence| {
                evidence
                    .map(|evidence| {
                        Ok(MirroredJobEvidence {
                            run_id: evidence.run.id,
                            patch: evidence.patch,
                            artifacts: crate::evidence::controller_artifact_metadata(
                                &evidence.runs,
                            )?,
                        })
                    })
                    .transpose()
            })
        },
        || {
            if let Some((run_id, token)) = foreground_source_lease.borrow_mut().take() {
                lease_store
                    .as_ref()
                    .expect("lease store is opened whenever a lease is held")
                    .release_running_runner_exec_source_lease(&run_id, &token)?;
            }
            Ok(())
        },
        |job, artifacts| {
            if let Some(session) = accepted_session.as_ref() {
                super::super::generation_store::record_job_artifacts(
                    &runner.id,
                    session,
                    &job.id.to_string(),
                    artifacts.iter().map(|artifact| artifact.id.clone()),
                )?;
            }
            Ok(())
        },
    );
}

pub(super) fn record_and_report_promotion_progress_frames(
    run_id: Option<&str>,
    runner_job_id: &str,
    events: &[JobEvent],
    last_sequence: &mut u64,
) {
    let frames = promotion_progress_frames(events, last_sequence);
    if let Some(run_id) = run_id {
        let _ = homeboy_agents::agent_task_lifecycle::record_promotion_progress_frames(
            run_id,
            runner_job_id,
            frames.iter().map(|(sequence, _)| {
                let event = events
                    .iter()
                    .find(|event| event.sequence == *sequence)
                    .expect("selected promotion event exists");
                (*sequence, event.data.clone().expect("promotion frame data"))
            }),
        );
    }
    for (_, message) in frames {
        eprintln!("{message}");
    }
}

fn promotion_progress_frames(events: &[JobEvent], last_sequence: &mut u64) -> Vec<(u64, String)> {
    let mut frames = Vec::new();
    for event in events {
        if event.sequence <= *last_sequence {
            continue;
        }
        *last_sequence = event.sequence;
        let Some(message) = event
            .data
            .as_ref()
            .and_then(|data| data.pointer("/metadata/promotion/schema"))
            .filter(|schema| {
                schema.as_str() == Some(crate::progress::PROMOTION_PROGRESS_FRAME_SCHEMA)
            })
            .and_then(|_| {
                event
                    .data
                    .as_ref()?
                    .pointer("/metadata/promotion/message")?
                    .as_str()
            })
        else {
            continue;
        };
        frames.push((event.sequence, message.to_string()));
    }
    frames
}

#[cfg(test)]
mod promotion_progress_tests {
    use super::*;

    fn frame(sequence: u64, message: &str) -> JobEvent {
        JobEvent {
            sequence,
            job_id: uuid::Uuid::nil(),
            kind: homeboy_core::api_jobs::JobEventKind::Progress,
            timestamp_ms: sequence,
            message: None,
            data: Some(json!({
                "schema": "homeboy/runner-progress/v1",
                "phase": "promotion",
                "metadata": {
                    "promotion": {
                        "schema": crate::progress::PROMOTION_PROGRESS_FRAME_SCHEMA,
                        "message": message,
                    }
                }
            })),
        }
    }

    #[test]
    fn promotion_progress_replays_in_sequence_once_for_late_observers() {
        let events = vec![
            frame(4, "promotion progress: applying patch"),
            frame(5, "promotion progress: gate running"),
        ];
        let mut cursor = 0;

        assert_eq!(
            promotion_progress_frames(&events, &mut cursor),
            vec![
                (4, "promotion progress: applying patch".to_string()),
                (5, "promotion progress: gate running".to_string()),
            ]
        );
        assert_eq!(cursor, 5);
        assert!(promotion_progress_frames(&events, &mut cursor).is_empty());
    }
}

pub(super) fn runner_exec_source_claim_error(
    store: &ObservationStore,
    run_id: &str,
    runner_job_id: &str,
) -> Result<Error> {
    let Some(run) = store.get_run(run_id)? else {
        return Ok(Error::validation_invalid_argument(
            "runner_exec",
            "runner exec source observation identity is missing",
            Some(run_id.to_string()),
            Some(vec![
                "Persist the running agent-task observation record before accepting its daemon job."
                    .to_string(),
            ]),
        ));
    };
    if run.status != RunStatus::Running.as_str() {
        return Ok(Error::validation_invalid_argument(
            "runner_exec",
            "runner exec source observation identity is not running",
            Some(run_id.to_string()),
            None,
        ));
    }
    if runner_exec_source_job_id(&run.metadata_json) != Some(runner_job_id) {
        return Ok(Error::validation_invalid_argument(
            "runner_exec",
            "runner exec source observation identity is not bound to the accepted daemon job",
            Some(run_id.to_string()),
            None,
        ));
    }
    if run.metadata_json["runner_exec_source_lease"]["expires_at_ms"]
        .as_i64()
        .is_some_and(|expires_at| expires_at >= chrono::Utc::now().timestamp_millis())
    {
        return Ok(Error::validation_invalid_argument(
            "runner_exec",
            "runner exec source is already owned by an active lifecycle",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(Error::validation_invalid_argument(
        "runner_exec",
        "runner exec source observation identity could not be claimed",
        Some(run_id.to_string()),
        None,
    ))
}

fn runner_exec_source_job_id(metadata: &serde_json::Value) -> Option<&str> {
    metadata["runner_job_id"]
        .as_str()
        .or_else(|| metadata["agent_task_run"]["metadata"]["runner_job_id"].as_str())
}

/// Workspace-bound direct work must establish that the daemon understands the
/// v2 lease contract before any mutating endpoint is called.
fn require_daemon_workspace_owner_lease_v2(client: &Client, local_url: &str) -> Result<()> {
    let data = daemon_get(client, local_url, "/capabilities").map_err(|error| {
        Error::validation_invalid_argument(
            "daemon_capabilities",
            format!(
                "direct daemon is unavailable for workspace owner lease v2 preflight: {}",
                error.message
            ),
            Some(local_url.to_string()),
            None,
        )
    })?;
    let body = canonical_daemon_body(&data, "daemon capabilities response")?;
    let supported = body
        .get("capabilities")
        .and_then(Value::as_array)
        .is_some_and(|capabilities| {
            capabilities.iter().any(|capability| {
                capability.get("capability").and_then(Value::as_str)
                    == Some(homeboy_core::workspace_claim::WORKSPACE_OWNER_LEASE_CAPABILITY)
                    && capability.get("version").and_then(Value::as_u64)
                        == Some(
                            homeboy_core::workspace_claim::WORKSPACE_CLAIM_PROTOCOL_VERSION as u64,
                        )
            })
        });
    supported.then_some(()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "daemon_capabilities",
            "direct daemon does not advertise workspace owner lease v2",
            Some(local_url.to_string()),
            None,
        )
    })
}

/// Selects whether an admission may interoperate with legacy daemon responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonAdmissionPolicy {
    LegacyCompatible,
    DurableLeaseRequired,
}

type WorkspaceOwnerRegistration = (homeboy_core::workspace_claim::WorkspaceIdentity, String);

const ADMISSION_RECOVERY_WINDOW: Duration = Duration::from_secs(10);
const ADMISSION_RECOVERY_RETRY_INTERVAL: Duration = Duration::from_millis(250);

// Admission is the last pre-provider boundary. Keep recovery ownership here so
// sibling children cannot replace a just-reconnected direct tunnel underneath
// one another after they have independently completed staging.
static ADMISSION_RECOVERY_LOCKS: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<()>>>>> =
    OnceLock::new();
static ADMISSION_RECOVERY_FAILURES: OnceLock<Mutex<BTreeMap<String, (Instant, Error)>>> =
    OnceLock::new();

fn admission_recovery_lock(runner_id: &str) -> Arc<Mutex<()>> {
    ADMISSION_RECOVERY_LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .expect("admission recovery lock registry")
        .entry(runner_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn admission_recovery_key(runner_id: &str, lease_id: &str) -> String {
    format!("{runner_id}\n{lease_id}")
}

#[derive(Debug, Default)]
struct AdmissionRenewalHealth {
    lease_expires_at_ms: Option<u64>,
    failure: Option<String>,
}

/// Token-free proof material a durable dispatcher may retain or serialize.
/// The admission token remains exclusively inside the RAII reservation.
#[derive(Clone, serde::Serialize)]
pub(crate) struct DaemonAdmissionReservationAuthority {
    daemon_lease_id: String,
    reservation_job_id: String,
    token_present: bool,
    lease_expires_at_ms: u64,
    #[serde(skip)]
    renewal_health: Arc<Mutex<AdmissionRenewalHealth>>,
}

impl std::fmt::Debug for DaemonAdmissionReservationAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonAdmissionReservationAuthority")
            .field("daemon_lease_id", &self.daemon_lease_id)
            .field("reservation_job_id", &self.reservation_job_id)
            .field("token_present", &self.token_present)
            .field("lease_expires_at_ms", &self.lease_expires_at_ms)
            .finish()
    }
}

impl DaemonAdmissionReservationAuthority {
    pub(crate) fn daemon_lease_id(&self) -> &str {
        &self.daemon_lease_id
    }

    pub(crate) fn reservation_job_id(&self) -> &str {
        &self.reservation_job_id
    }

    /// When the daemon reclaims this reservation without a controller. Recorded
    /// on the durable run so an operator can see the automatic-reclaim deadline
    /// instead of guessing whether a lost caller stranded capacity (#9163).
    pub(crate) fn lease_expires_at_ms(&self) -> u64 {
        self.lease_expires_at_ms
    }

    /// Proves that the daemon, rather than local Drop cleanup, still owns the
    /// lease expiry/cancellation contract before the dispatcher submits `/exec`.
    pub(crate) fn prove_server_owned_expiry_or_cancellation_authority(&self) -> Result<()> {
        if !self.token_present || self.lease_expires_at_ms == 0 {
            return Err(Error::internal_unexpected(
                "strict daemon admission has no server-owned lease authority",
            ));
        }
        let health = self
            .renewal_health
            .lock()
            .expect("admission renewal health lock");
        if let Some(failure) = &health.failure {
            return Err(Error::internal_unexpected(format!(
                "strict daemon admission renewal failed before dispatch: {failure}"
            )));
        }
        if health
            .lease_expires_at_ms
            .unwrap_or(self.lease_expires_at_ms)
            == 0
        {
            return Err(Error::internal_unexpected(
                "strict daemon admission has no renewable server lease expiry",
            ));
        }
        Ok(())
    }
}

/// Keeps an admitted Lab offload visible in daemon active-job accounting until
/// its staged execution reaches a terminal or detached handoff outcome.
pub(crate) struct DaemonAdmissionReservation {
    local_url: String,
    job_id: String,
    token: Option<String>,
    workspace_owner_lease: Arc<Mutex<Option<homeboy_core::workspace_claim::WorkspaceOwnerLease>>>,
    renewer_stop: Option<Sender<()>>,
    renewer: Option<std::thread::JoinHandle<()>>,
    authority: DaemonAdmissionReservationAuthority,
}

impl DaemonAdmissionReservation {
    pub(crate) fn job_id(&self) -> &str {
        self.authority.reservation_job_id()
    }

    pub(crate) fn authority(&self) -> DaemonAdmissionReservationAuthority {
        self.authority.clone()
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
        let mut payload = json!({ "admission_token": self.token.as_deref() });
        if let Some(lease) = self
            .workspace_owner_lease
            .lock()
            .expect("admission owner lease lock")
            .as_ref()
        {
            payload["workspace_owner_lease"] = json!(lease);
        }
        let _ = daemon_post_json_text(
            &client,
            &self.local_url,
            &format!("/admissions/{}/release", self.job_id),
            &payload,
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
    policy: DaemonAdmissionPolicy,
    workspace_owner_registration: Option<WorkspaceOwnerRegistration>,
) -> Result<DaemonAdmissionReservation> {
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| {
            Error::internal_unexpected(format!("build daemon admission client: {err}"))
        })?;
    if workspace_owner_registration.is_some() {
        require_daemon_workspace_owner_lease_v2(&client, local_url)?;
    }
    let mut payload = json!({
        "runner_id": runner_id,
        "command": command,
        "expected_daemon_lease_id": expected_daemon_lease_id,
        "idempotency_key": idempotency_key,
        "admission_lease_protocol": 1,
    });
    if let Some((workspace, owner_id)) = workspace_owner_registration.as_ref() {
        payload["workspace_owner_request"] = json!({
            "workspace": workspace,
            "owner_id": owner_id,
            "ttl_ms": homeboy_core::workspace_claim::MAX_WORKSPACE_CLAIM_TTL_MS,
        });
    }
    let response = daemon_post_json_text(
        &client,
        local_url,
        "/admissions",
        &payload,
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
    let workspace_owner_lease = body
        .get("workspace_owner_lease")
        .filter(|value| !value.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| {
            Error::validation_invalid_argument(
                "workspace_owner_lease",
                format!("malformed direct daemon owner lease: {err}"),
                None,
                None,
            )
        })?;
    if workspace_owner_registration.is_some() && workspace_owner_lease.is_none() {
        return Err(Error::validation_invalid_argument(
            "workspace_owner_lease",
            "direct daemon admission did not return its durable owner lease",
            Some(runner_id.to_string()),
            None,
        ));
    }
    let token = body
        .get("admission_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    let lease_protocol_confirmed =
        body.get("admission_lease_protocol").and_then(Value::as_u64) == Some(1);
    let lease_expires_at_ms = body.pointer("/lease/expires_at_ms").and_then(Value::as_u64);
    let renewable = body.pointer("/lease/renewable").and_then(Value::as_bool) == Some(true);
    if policy == DaemonAdmissionPolicy::DurableLeaseRequired
        && !strict_admission_response_is_complete(
            lease_protocol_confirmed,
            token.as_deref(),
            renewable,
            lease_expires_at_ms,
        )
    {
        return Err(strict_admission_rejection(
            &client,
            runner_id,
            local_url,
            &job.id.to_string(),
            token.as_deref(),
            lease_protocol_confirmed,
        ));
    }
    let renewal_health = Arc::new(Mutex::new(AdmissionRenewalHealth {
        lease_expires_at_ms,
        failure: None,
    }));
    let token_present = token.is_some();
    let (renewer_stop, renewer) = match token.as_deref() {
        Some(token) => {
            let (stop, renewer) = spawn_admission_renewer(
                local_url.to_string(),
                job.id.to_string(),
                token.to_string(),
                Arc::new(Mutex::new(workspace_owner_lease.clone())),
                renewal_health.clone(),
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
        workspace_owner_lease: Arc::new(Mutex::new(workspace_owner_lease)),
        renewer_stop,
        renewer,
        authority: DaemonAdmissionReservationAuthority {
            daemon_lease_id: daemon_lease_id.to_string(),
            reservation_job_id: job.id.to_string(),
            token_present,
            lease_expires_at_ms: lease_expires_at_ms.unwrap_or_default(),
            renewal_health,
        },
    })
}

/// Retry only the known post-reconnect admission window under one runner-local
/// owner. This is deliberately below all provider work: a child either obtains
/// an authoritative reservation or returns before `/exec` can consume budget.
pub(crate) fn reserve_daemon_admission_with_recovery(
    runner_id: &str,
    local_url: &str,
    command: &str,
    expected_daemon_lease_id: &str,
    idempotency_key: Option<&str>,
    policy: DaemonAdmissionPolicy,
    workspace_owner_registration: Option<WorkspaceOwnerRegistration>,
) -> Result<DaemonAdmissionReservation> {
    reserve_daemon_admission_with_recovery_with(
        runner_id,
        expected_daemon_lease_id,
        Instant::now() + ADMISSION_RECOVERY_WINDOW,
        || {
            reserve_daemon_admission(
                runner_id,
                local_url,
                command,
                expected_daemon_lease_id,
                idempotency_key,
                policy,
                workspace_owner_registration.clone(),
            )
        },
        || std::thread::sleep(ADMISSION_RECOVERY_RETRY_INTERVAL),
    )
}

fn reserve_daemon_admission_with_recovery_with<T, Reserve, Wait>(
    runner_id: &str,
    expected_daemon_lease_id: &str,
    deadline: Instant,
    mut reserve: Reserve,
    mut wait: Wait,
) -> Result<T>
where
    Reserve: FnMut() -> Result<T>,
    Wait: FnMut(),
{
    let lock = admission_recovery_lock(runner_id);
    let _owner = lock.lock().expect("runner admission recovery owner");
    let key = admission_recovery_key(runner_id, expected_daemon_lease_id);
    let failures = ADMISSION_RECOVERY_FAILURES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let cached_failure = {
        let mut failures = failures
            .lock()
            .expect("admission recovery failure registry");
        failures.retain(|_, (recorded_at, _)| recorded_at.elapsed() < ADMISSION_RECOVERY_WINDOW);
        failures.get(&key).cloned()
    };
    if let Some((_, error)) = cached_failure {
        return Err(error);
    }
    loop {
        match reserve() {
            Ok(reservation) => {
                failures
                    .lock()
                    .expect("admission recovery failure registry")
                    .remove(&key);
                return Ok(reservation);
            }
            // A different admitted lease proves another daemon owns this endpoint.
            Err(error) if admission_recovery_failure_is_authoritative(&error) => return Err(error),
            Err(error) if !admission_recovery_failure_is_transient(&error) => return Err(error),
            Err(error) if Instant::now() >= deadline => {
                let error =
                    admission_recovery_timeout_error(runner_id, expected_daemon_lease_id, error);
                failures
                    .lock()
                    .expect("admission recovery failure registry")
                    .insert(key, (Instant::now(), error.clone()));
                return Err(error);
            }
            Err(_) => wait(),
        }
    }
}

fn admission_recovery_failure_is_authoritative(error: &Error) -> bool {
    error.details.get("field").and_then(Value::as_str) == Some("expected_daemon_lease_id")
}

fn admission_recovery_failure_is_transient(error: &Error) -> bool {
    error.retryable == Some(true) || error.message.contains("daemon lease is not fresh")
}

fn admission_recovery_timeout_error(
    runner_id: &str,
    expected_daemon_lease_id: &str,
    source: Error,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "reconnect",
        format!(
            "runner `{runner_id}` did not become ready to admit Lab children against lease `{expected_daemon_lease_id}` within {}s: {}",
            ADMISSION_RECOVERY_WINDOW.as_secs(),
            source.message,
        ),
        Some(runner_id.to_string()),
        Some(vec![format!(
            "Re-run `homeboy runner refresh-homeboy {runner_id} --reconnect` once, then retry the batch."
        )]),
    );
    // The provider never started and the staged workspace remains reusable.
    // Keep the original child lifecycle eligible for bounded retry.
    error.retryable = Some(true);
    error
}

fn strict_admission_response_is_complete(
    lease_protocol_confirmed: bool,
    token: Option<&str>,
    renewable: bool,
    lease_expires_at_ms: Option<u64>,
) -> bool {
    lease_protocol_confirmed
        && token.is_some_and(|token| !token.trim().is_empty())
        && renewable
        && lease_expires_at_ms.is_some_and(|expires_at_ms| expires_at_ms > 0)
}

fn strict_admission_rejection(
    client: &Client,
    runner_id: &str,
    local_url: &str,
    job_id: &str,
    token: Option<&str>,
    lease_protocol_confirmed: bool,
) -> Error {
    let release = daemon_post_json_text(
        client,
        local_url,
        &format!("/admissions/{job_id}/release"),
        &token
            .map(|token| json!({ "admission_token": token }))
            .unwrap_or_else(|| json!({})),
        DaemonPostOptions::default(),
    );
    let released = release
        .ok()
        .and_then(|response| serde_json::from_str::<DaemonEnvelope>(&response.body).ok())
        .and_then(|envelope| envelope.data)
        .and_then(|data| {
            canonical_daemon_body(&data, "daemon admission release response")
                .ok()
                .cloned()
        })
        .and_then(|body| serde_json::from_value::<Job>(body["job"].clone()).ok())
        .is_some_and(|job| job.status.is_terminal());
    let cleanup = if released {
        "the legacy reservation was released and reconciled"
    } else {
        "the reservation could not be proven released; reconcile the daemon admission before retrying"
    };
    let protocol = if lease_protocol_confirmed {
        "the daemon lease response omitted required token or expiry authority"
    } else {
        "the daemon did not confirm admission lease protocol v1"
    };
    Error::validation_invalid_argument(
        "daemon_admission",
        format!("runner `{runner_id}` rejected durable dispatch: {protocol}; {cleanup}"),
        Some(job_id.to_string()),
        None,
    )
}

/// Renew at half the daemon's bounded lease interval while staging keeps the
/// handoff alive. Explicit release remains authoritative when the context ends.
fn spawn_admission_renewer(
    local_url: String,
    job_id: String,
    token: String,
    workspace_owner_lease: Arc<Mutex<Option<homeboy_core::workspace_claim::WorkspaceOwnerLease>>>,
    health: Arc<Mutex<AdmissionRenewalHealth>>,
) -> (Sender<()>, std::thread::JoinHandle<()>) {
    let (stop, shutdown) = mpsc::channel();
    let renewer = std::thread::spawn(move || {
        while shutdown.recv_timeout(Duration::from_secs(15)).is_err() {
            let Ok(client) = Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
            else {
                health
                    .lock()
                    .expect("admission renewal health lock")
                    .failure = Some("build daemon renewal client".to_string());
                return;
            };
            let mut payload = json!({ "admission_token": token });
            if let Some(lease) = workspace_owner_lease
                .lock()
                .expect("admission owner lease lock")
                .as_ref()
            {
                payload["workspace_owner_lease"] = json!(lease);
            }
            let response = daemon_post_json_text(
                &client,
                &local_url,
                &format!("/admissions/{job_id}/renew"),
                &payload,
                DaemonPostOptions::default(),
            );
            let body = response
                .ok()
                .and_then(|response| serde_json::from_str::<DaemonEnvelope>(&response.body).ok())
                .and_then(|envelope| envelope.data)
                .and_then(|data| {
                    canonical_daemon_body(&data, "daemon admission renewal response")
                        .ok()
                        .cloned()
                });
            let expires_at_ms = body
                .as_ref()
                .and_then(|body| body.pointer("/lease/expires_at_ms").and_then(Value::as_u64));
            let renewed_owner = body.as_ref().and_then(|body| {
                serde_json::from_value(body["workspace_owner_lease"].clone()).ok()
            });
            match (expires_at_ms, renewed_owner) {
                (Some(expires_at_ms), owner) if expires_at_ms > 0 => {
                    if workspace_owner_lease
                        .lock()
                        .expect("admission owner lease lock")
                        .is_some()
                        && owner.is_none()
                    {
                        health
                            .lock()
                            .expect("admission renewal health lock")
                            .failure =
                            Some("daemon did not return renewed workspace owner lease".to_string());
                        return;
                    }
                    if let Some(owner) = owner {
                        *workspace_owner_lease
                            .lock()
                            .expect("admission owner lease lock") = Some(owner);
                    }
                    health
                        .lock()
                        .expect("admission renewal health lock")
                        .lease_expires_at_ms = Some(expires_at_ms);
                }
                _ => {
                    health
                        .lock()
                        .expect("admission renewal health lock")
                        .failure =
                        Some("daemon did not confirm admission lease renewal".to_string());
                    return;
                }
            }
        }
    });
    (stop, renewer)
}

#[cfg(test)]
#[expect(
    clippy::items_after_test_module,
    reason = "Production daemon helpers remain below focused admission tests after the staged source split."
)]
mod admission_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn strict_admission_requires_protocol_token_and_server_expiry() {
        assert!(strict_admission_response_is_complete(
            true,
            Some("opaque-token"),
            true,
            Some(42),
        ));
        assert!(!strict_admission_response_is_complete(
            true,
            None,
            true,
            Some(42)
        ));
        assert!(!strict_admission_response_is_complete(
            false,
            Some("opaque-token"),
            true,
            Some(42),
        ));
        assert!(!strict_admission_response_is_complete(
            true,
            Some("opaque-token"),
            false,
            Some(42),
        ));
    }

    #[test]
    fn authority_excludes_token_from_debug_and_serialization() {
        let authority = DaemonAdmissionReservationAuthority {
            daemon_lease_id: "lease-a".to_string(),
            reservation_job_id: "job-a".to_string(),
            token_present: true,
            lease_expires_at_ms: 42,
            renewal_health: Arc::new(Mutex::new(AdmissionRenewalHealth::default())),
        };
        let debug = format!("{authority:?}");
        let serialized = serde_json::to_string(&authority).expect("serialize authority");
        assert!(!debug.contains("opaque-token"));
        assert!(!serialized.contains("token\":\""));
        assert!(authority
            .prove_server_owned_expiry_or_cancellation_authority()
            .is_ok());
    }

    #[test]
    fn renewal_failure_blocks_strict_dispatch_authority() {
        let authority = DaemonAdmissionReservationAuthority {
            daemon_lease_id: "lease-a".to_string(),
            reservation_job_id: "job-a".to_string(),
            token_present: true,
            lease_expires_at_ms: 42,
            renewal_health: Arc::new(Mutex::new(AdmissionRenewalHealth {
                lease_expires_at_ms: Some(42),
                failure: Some("daemon rejected renewal".to_string()),
            })),
        };
        let error = authority
            .prove_server_owned_expiry_or_cancellation_authority()
            .expect_err("renewal failure must be observable before dispatch");
        assert!(error.message.contains("renewal failed"));
        assert!(!error.message.contains("opaque-token"));
    }

    #[test]
    fn renewal_failure_during_exec_is_visible_after_acceptance() {
        let renewal_health = Arc::new(Mutex::new(AdmissionRenewalHealth {
            lease_expires_at_ms: Some(42),
            failure: None,
        }));
        let authority = DaemonAdmissionReservationAuthority {
            daemon_lease_id: "lease-a".to_string(),
            reservation_job_id: "job-a".to_string(),
            token_present: true,
            lease_expires_at_ms: 42,
            renewal_health: Arc::clone(&renewal_health),
        };
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let renewer_barrier = Arc::clone(&barrier);
        let renewer = std::thread::spawn(move || {
            renewer_barrier.wait();
            renewal_health.lock().expect("renewal health lock").failure =
                Some("daemon rejected renewal during exec".to_string());
            renewer_barrier.wait();
        });

        authority
            .prove_server_owned_expiry_or_cancellation_authority()
            .expect("authority before exec");
        barrier.wait();
        barrier.wait();
        renewer.join().expect("renewer");
        let error = authority
            .prove_server_owned_expiry_or_cancellation_authority()
            .expect_err("post-acceptance authority must observe renewal failure");
        assert!(error.message.contains("renewal failed"));
    }

    #[test]
    fn sibling_admissions_share_one_post_reconnect_recovery_and_lease() {
        let runner_id = format!("parallel-admission-{}", uuid::Uuid::new_v4());
        let attempts = Arc::new(AtomicUsize::new(0));
        let recovery_owners = Arc::new(AtomicUsize::new(0));
        let provider_budget = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut children = Vec::new();

        for _ in 0..4 {
            let runner_id = runner_id.clone();
            let attempts = Arc::clone(&attempts);
            let recovery_owners = Arc::clone(&recovery_owners);
            let provider_budget = Arc::clone(&provider_budget);
            let barrier = Arc::clone(&barrier);
            children.push(std::thread::spawn(move || {
                barrier.wait();
                reserve_daemon_admission_with_recovery_with(
                    &runner_id,
                    "lease-reconnected",
                    Instant::now() + Duration::from_secs(1),
                    || {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt < 2 {
                            if attempt == 0 {
                                recovery_owners.fetch_add(1, Ordering::SeqCst);
                            }
                            return Err(Error::validation_invalid_argument(
                                "runner",
                                "daemon lease is not fresh",
                                None,
                                None,
                            ));
                        }
                        Ok("lease-reconnected".to_string())
                    },
                    || {},
                )
                .map(|lease| {
                    assert_eq!(lease, "lease-reconnected");
                    assert_eq!(provider_budget.load(Ordering::SeqCst), 0);
                })
            }));
        }

        for child in children {
            child
                .join()
                .expect("child thread")
                .expect("sibling admission");
        }
        assert_eq!(
            recovery_owners.load(Ordering::SeqCst),
            1,
            "one recovery owner"
        );
        assert_eq!(provider_budget.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn exhausted_sibling_recovery_returns_one_cached_batch_action() {
        let runner_id = format!("exhausted-admission-{}", uuid::Uuid::new_v4());
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut children = Vec::new();
        for _ in 0..4 {
            let runner_id = runner_id.clone();
            let attempts = Arc::clone(&attempts);
            children.push(std::thread::spawn(move || {
                reserve_daemon_admission_with_recovery_with::<(), _, _>(
                    &runner_id,
                    "lease-reconnected",
                    Instant::now(),
                    || {
                        attempts.fetch_add(1, Ordering::SeqCst);
                        Err(Error::validation_invalid_argument(
                            "runner",
                            "daemon lease is not fresh",
                            None,
                            None,
                        ))
                    },
                    || {},
                )
                .expect_err("bounded recovery must fail")
            }));
        }
        let errors = children
            .into_iter()
            .map(|child| child.join().expect("child thread"))
            .collect::<Vec<_>>();
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "only one recovery owner probes"
        );
        assert!(errors.iter().all(|error| error.details["tried"]
            .as_array()
            .is_some_and(|actions| actions.len() == 1)));
        assert!(errors.iter().all(|error| error.retryable == Some(true)));
        assert_eq!(errors[0].details["tried"], errors[3].details["tried"]);
    }

    #[test]
    fn non_transient_admission_failure_is_not_retried() {
        let runner_id = format!("invalid-admission-{}", uuid::Uuid::new_v4());
        let attempts = AtomicUsize::new(0);
        let error = reserve_daemon_admission_with_recovery_with::<(), _, _>(
            &runner_id,
            "lease-reconnected",
            Instant::now() + Duration::from_secs(1),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(Error::validation_invalid_argument(
                    "admission_response",
                    "daemon admission response missing data",
                    None,
                    None,
                ))
            },
            || panic!("non-transient admission failure must not wait"),
        )
        .expect_err("invalid admission response must fail immediately");

        assert_eq!(error.details["field"], "admission_response");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
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
            submit(&recovered_endpoint).map_err(|retry_error| {
                if daemon_submission_connection_was_lost(&retry_error) {
                    recovered_admission_transport_error(
                        &accepted_session.runner_id,
                        "lost the replacement admission tunnel before daemon acceptance",
                    )
                } else {
                    retry_error
                }
            })
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
    if accepted_session.mode != crate::RunnerTunnelMode::DirectSsh {
        return Err(recovered_admission_transport_error(
            runner_id,
            "lost its direct admission tunnel before daemon acceptance",
        ));
    }
    let recovered = crate::connection::status_for_admission(runner_id)?;
    let session = recovered
        .session
        .filter(|_| recovered.connected)
        .ok_or_else(|| {
            recovered_admission_transport_error(
                runner_id,
                "did not recover a healthy daemon admission session",
            )
        })?;
    if session.mode != crate::RunnerTunnelMode::DirectSsh {
        return Err(recovered_admission_transport_error(
            runner_id,
            "recovered a non-direct session for direct Lab admission",
        ));
    }
    session.local_url.ok_or_else(|| {
        recovered_admission_transport_error(runner_id, "recovered without a direct daemon endpoint")
    })
}

fn recovered_admission_transport_error(runner_id: &str, reason: &str) -> Error {
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!("runner `{runner_id}` {reason}"),
        json!({ "runner_id": runner_id, "phase": "lab_handoff" }),
    )
    .with_retryable(true)
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

    // Probe the command and state authority that this job will receive, not
    // merely the runner's persisted configuration.
    let mut effective_runner = runner.clone();
    effective_runner.env = request_env.clone();
    let capabilities = runner_capability_snapshot_for_preflight(&effective_runner, preflight)?;
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

#[expect(
    clippy::too_many_arguments,
    reason = "Detached handoff retains durable job, source, materialization, and mirror identities independently."
)]
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
            .with_artifact_refs([JobArtifactMetadata {
                id: "run_location_index".to_string(),
                name: Some("run location index".to_string()),
                path: Some(run_location_index_path),
                url: None,
                ..Default::default()
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

/// First wait between terminal-status polls. Matches the flat 200ms this loop
/// used before it was bounded, so a job that settles quickly is still observed
/// just as quickly.
const DAEMON_TERMINAL_WAIT_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
/// Ceiling for terminal-wait backoff, so a long-running job is polled at a
/// steady low rate instead of 5 times a second for its whole duration.
const DAEMON_TERMINAL_WAIT_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Sleep up to `interval`, returning `true` if cancellation was observed.
fn sleep_daemon_wait_unless_cancelled(interval: Duration, cancelled: &dyn Fn() -> bool) -> bool {
    crate::cancellable_sleep::sleep_unless_cancelled(interval, || cancelled())
}

fn daemon_terminal_wait_cancelled_error(job_id: &str) -> Error {
    Error::internal_unexpected(format!(
        "cancelled while waiting for runner job `{job_id}` to reach a terminal status"
    ))
}

/// Classified retryable: running out of budget does not prove the job failed,
/// and it is very likely still executing remotely.
fn daemon_terminal_wait_deadline_error(job_id: &str, waited: Duration, budget: Duration) -> Error {
    let mut error = Error::new(
        ErrorCode::InternalUnexpected,
        format!(
            "runner job `{job_id}` did not reach a terminal status within {:.1}s (budget {:.1}s)",
            waited.as_secs_f64(),
            budget.as_secs_f64()
        ),
        json!({
            "daemon_terminal_wait_timeout": {
                "runner_job_id": job_id,
                "waited_ms": waited.as_millis() as u64,
                "budget_ms": budget.as_millis() as u64,
            },
        }),
    );
    error.retryable = Some(true);
    error.with_hint(format!(
        "The remote job is probably still running. Inspect it with `homeboy runner job logs <runner-id> {job_id}`."
    ))
}

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

/// Authoritative terminal state for a known daemon job. This observer never
/// submits work, making it safe for foreground waiting and controller resume.
pub(crate) struct DaemonTerminalObservation {
    pub(crate) job: Job,
    pub(crate) events: Vec<JobEvent>,
}

/// Wait for a daemon job to reach a terminal status.
///
/// `budget` bounds the total wait so a job that never settles cannot pin the
/// caller forever; `cancelled` is polled between sleep slices so a caller with
/// a cancellation token stays responsive while waiting. Both are supplied by
/// the caller because only it knows the lifecycle budget the wait belongs to.
pub(crate) fn observe_daemon_job_until_terminal(
    runner_id: &str,
    runner_job_id: &str,
    accepted_daemon_identity: Option<&str>,
    budget: Duration,
    cancelled: &dyn Fn() -> bool,
) -> Result<DaemonTerminalObservation> {
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            Error::internal_unexpected(format!("build daemon observer client: {error}"))
        })?;
    let status = super::super::status(runner_id)?;
    let session = status.session.filter(|_| status.connected).ok_or_else(|| {
        Error::internal_unexpected(format!(
            "runner `{runner_id}` has no connected daemon session for observation"
        ))
    })?;
    let mut endpoint = session.local_url.clone().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "runner `{runner_id}` has no direct daemon endpoint for observation"
        ))
    })?;
    let started = Instant::now();
    let deadline = started + budget;
    let mut backoff = DAEMON_TERMINAL_WAIT_INITIAL_BACKOFF;
    let job = loop {
        if cancelled() {
            return Err(daemon_terminal_wait_cancelled_error(runner_job_id));
        }
        let (job, refreshed_endpoint) = fetch_daemon_job_resilient_with_endpoint_reload(
            &client,
            &endpoint,
            runner_job_id,
            || refreshed_daemon_endpoint(runner_id, runner_job_id, accepted_daemon_identity),
        )?;
        endpoint = refreshed_endpoint;
        if job.status.is_terminal() {
            break job;
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(daemon_terminal_wait_deadline_error(
                runner_job_id,
                started.elapsed(),
                budget,
            ));
        }
        // Never sleep past the deadline, and never sleep longer than the
        // cancellation slice in one go, so growing backoff cannot delay a
        // cancellation response.
        let interval = backoff.min(deadline - now);
        if sleep_daemon_wait_unless_cancelled(interval, cancelled) {
            return Err(daemon_terminal_wait_cancelled_error(runner_job_id));
        }
        backoff = backoff
            .saturating_mul(2)
            .min(DAEMON_TERMINAL_WAIT_MAX_BACKOFF);
    };
    let events = fetch_daemon_events(&client, &endpoint, runner_job_id)?;
    super::super::generation_store::record_job_artifacts(
        runner_id,
        &session,
        runner_job_id,
        job.artifacts.iter().map(|artifact| artifact.id.clone()),
    )?;
    super::super::generation_store::reconcile(runner_id, Some(&session))?;
    Ok(DaemonTerminalObservation { job, events })
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
                if !daemon_poll_transport_was_lost(&err) {
                    return Err(err);
                }
                // Checked before the endpoint-change fast path below, not
                // after it. An endpoint change is still tolerated — it retries
                // immediately and skips the backoff — but it can no longer
                // extend the grace window without limit. When `reload_endpoint`
                // alternates between two endpoints, every iteration used to
                // `continue` past this check and the loop never terminated.
                if Instant::now() >= transient_deadline {
                    let mut surfaced = err;
                    surfaced.retryable = surfaced.retryable.or(Some(true));
                    return Err(surfaced.with_hint(format!(
                        "Lost contact with the runner daemon while polling job `{job_id}` for longer than {}s; the remote job may still be in flight. Reconnect with `homeboy runner connect <runner-id>` and inspect `homeboy runner job logs <runner-id> {job_id}`.",
                        DAEMON_POLL_TRANSIENT_GRACE.as_secs()
                    )));
                }
                if let Some(refreshed_endpoint) = reload_endpoint()? {
                    if refreshed_endpoint != endpoint {
                        endpoint = refreshed_endpoint;
                        continue;
                    }
                }
                std::thread::sleep(DAEMON_POLL_RETRY_BACKOFF);
            }
        }
    }
}

pub(super) fn daemon_poll_transport_was_lost(error: &Error) -> bool {
    matches!(
        error
            .details
            .pointer("/daemon_transport_error/kind")
            .and_then(Value::as_str),
        Some("connect" | "timeout" | "body_decode")
    ) || (error.details.get("http_status").is_none()
        && super::super::daemon_health::runner_daemon_health_failure(error).is_some())
}

fn refreshed_daemon_endpoint(
    runner_id: &str,
    job_id: &str,
    expected_identity: Option<&str>,
) -> Result<Option<String>> {
    let session = super::super::connection::reconnect_job_owner_for_polling(runner_id, job_id)?;
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
    // A controller-side daemon transport drop is NOT a terminal job failure:
    // the durable runner job is still executing remotely, and reconnecting can
    // resume observing it. Only terminalize when the poll failure is something
    // other than a recoverable transport drop (an authoritative "job gone" /
    // decode error the daemon actually returned). Otherwise keep the run
    // recoverable so a reconnect can pick it back up instead of reporting a
    // still-running job as failed (#7928).
    let transient_transport_drop =
        super::super::daemon_health::runner_daemon_health_failure(&source).is_some();
    let mut error = daemon_job_context_error(&runner.id, &job_id, persisted_run_id, source);
    if transient_transport_drop {
        // `daemon_job_context_error` already marks this recoverable
        // (retryable, status: "recoverable_followup_required") with durable-job
        // resumption guidance; preserve that instead of forcing a terminal
        // failure.
        return error;
    }
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

/// The terminal transport error shape, used by the daemon's own result
/// delivery and by the handoff tests.
///
/// This carried `#[cfg(test)]` between #13140 and this commit, which broke
/// the non-test build. `cargo check --workspace --tests` cannot see that
/// class of mistake: with only test targets requested, the library is
/// compiled once under `cfg(test)` and the ordinary configuration is never
/// checked at all.
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
            job.status.as_str(),
            err.message
        ),
        json!({
            "runner_id": runner.id,
            "job_id": job_id,
            "persisted_run_id": run_id,
            "remote_cwd": cwd,
            "command": redact_argv(command),
            "job_status": job.status.as_str(),
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

pub(super) fn append_agent_task_lifecycle_workload_event(
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

/// The cook/dispatch counterpart of [`append_agent_task_lifecycle_workload_event`].
///
/// Extracts the dispatch handoff on the side that owns the output, gated by the
/// workload's `agent_task.handoff_mirror_policy`, and republishes it as a typed
/// event carrying the workload's run id. The controller mirrors from this event
/// instead of rescanning stdout/stderr (#7530).
///
/// Infallible on purpose: a missing or unrecognized handoff is not a job
/// failure, it just means the controller's retained output fallback runs. This
/// is the opposite of the run-plan path, where a malformed aggregate is a hard
/// error because the controller has no other source for it.
pub(super) fn append_agent_task_dispatch_handoff_workload_event(
    events: &mut Vec<JobEvent>,
    lab_runner_workload: Option<&LabRunnerWorkload>,
    runner_id: &str,
    runner_job_id: &str,
) {
    let Some(result) = result_event_data(events) else {
        return;
    };
    let Some(event) =
        crate::agent_task_handoff_event::agent_task_dispatch_handoff_event_from_workload_result(
            lab_runner_workload,
            runner_id,
            runner_job_id,
            &result,
        )
    else {
        return;
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
        message: Some("agent-task dispatch handoff".to_string()),
        data: Some(
            crate::agent_task_handoff_event::agent_task_dispatch_handoff_workload_event_payload(
                &event,
            ),
        ),
    });
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
