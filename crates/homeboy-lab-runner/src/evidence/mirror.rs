use std::io::Write;
use std::ops::Deref;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::agent_task_lifecycle_event::{
    agent_task_run_plan_lifecycle_event_from_job_events,
    agent_task_run_plan_lifecycle_event_from_value,
};
use homeboy_core::api_jobs::{Job, JobArtifactMetadata, JobEvent, JobStatus, RunnerJobLogSnapshot};
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::execution_contract::encode_uri_component;
use homeboy_core::notification_route::NotificationRoute;
use homeboy_core::observation::{
    ArtifactPublication, ArtifactRecord, ObservationStore, RunRecord, RunStatus,
};
use homeboy_core::redaction::redact_argv_shell_display;
use homeboy_core::runner_download_cache::RunnerDownloadIntent;

use super::super::execution::{canonical_daemon_body, daemon_api_get, result_event_data};
use super::super::{load, Runner};
use super::detail::{
    explicit_observation_run_ids, remote_detail_artifacts, remote_detail_to_run_record,
};
use super::tokens::RemoteArtifactToken;
use super::util::{
    job_status_as_run_status, local_job_run_id, ms_to_rfc3339, result_summary,
    runner_exec_run_label, runner_metadata, source_snapshot_from_result,
};

pub(super) const MIRRORED_REMOTE_EVENT_LIMIT: usize = 32;
pub(super) const MIRRORED_REMOTE_EVENT_MESSAGE_LIMIT: usize = 1_024;

#[derive(Debug)]
pub struct MirroredDaemonEvidence {
    pub run: RunRecord,
    pub runs: Vec<RunRecord>,
    pub patch: Option<Value>,
}

#[derive(Debug, Clone)]
struct SyntheticRunOwnership {
    run_id: String,
    publication_token: String,
}

#[derive(Debug, Clone)]
pub(super) struct MirroredJobRun {
    pub(super) run: RunRecord,
    synthetic_ownership: Option<SyntheticRunOwnership>,
}

impl Deref for MirroredJobRun {
    type Target = RunRecord;

    fn deref(&self) -> &Self::Target {
        &self.run
    }
}

fn discard_owned_synthetic_run(store: &ObservationStore, run: &MirroredJobRun) {
    if let Some(ownership) = &run.synthetic_ownership {
        store
            .discard_synthetic_run(&ownership.run_id, &ownership.publication_token)
            .ok();
    }
}

/// Terminal responses expose only controller-owned records, never the runner's
/// original artifact metadata or paths.
pub fn controller_artifact_metadata(runs: &[RunRecord]) -> Result<Vec<JobArtifactMetadata>> {
    let store = ObservationStore::open_initialized()?;
    let artifacts = runs.iter().try_fold(Vec::new(), |mut artifacts, run| {
        artifacts.extend(store.list_artifacts(&run.id)?);
        Ok::<_, Error>(artifacts)
    })?;
    artifacts
        .into_iter()
        .map(|artifact| {
            validate_controller_artifact(&artifact)?;
            let controller_run_id = artifact.run_id.clone();
            let url = homeboy_core::artifact_links::controller_artifact_url(&artifact)?;
            let fetch_command = format!(
                "homeboy runs artifact get {} {} -o <path>",
                controller_run_id, artifact.id
            );
            Ok(JobArtifactMetadata {
                id: artifact.id,
                name: None,
                path: Some(artifact.path),
                url,
                mime: artifact.mime,
                size_bytes: artifact
                    .size_bytes
                    .and_then(|size| u64::try_from(size).ok()),
                sha256: artifact.sha256,
                content_base64: None,
                metadata: Some(json!({
                    "controller_run_id": controller_run_id,
                    "controller_owned": true,
                    "fetch_command": fetch_command,
                })),
            })
        })
        .collect()
}

fn validate_controller_artifact(artifact: &ArtifactRecord) -> Result<()> {
    if artifact.artifact_type != "file" {
        return Err(Error::validation_invalid_argument(
            "artifact.type",
            "terminal artifact must be a controller-owned file",
            Some(artifact.id.clone()),
            None,
        ));
    }
    let expected_size = artifact.size_bytes.ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact.size_bytes",
            "terminal artifact is missing controller size metadata",
            Some(artifact.id.clone()),
            None,
        )
    })?;
    let expected_sha256 = artifact
        .sha256
        .as_deref()
        .filter(|sha| !sha.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact.sha256",
                "terminal artifact is missing controller checksum metadata",
                Some(artifact.id.clone()),
                None,
            )
        })?;
    if artifact.mime.as_deref().is_none_or(str::is_empty) {
        return Err(Error::validation_invalid_argument(
            "artifact.mime",
            "terminal artifact is missing controller media type metadata",
            Some(artifact.id.clone()),
            None,
        ));
    }
    let path = std::path::Path::new(&artifact.path);
    let size_matches = std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .and_then(|metadata| i64::try_from(metadata.len()).ok())
        == Some(expected_size);
    if !size_matches {
        return Err(Error::validation_invalid_argument(
            "artifact.size_bytes",
            "terminal artifact bytes are missing or do not match controller metadata",
            Some(artifact.id.clone()),
            None,
        ));
    }
    if homeboy_core::artifact_metadata::sha256_file(path)? != expected_sha256 {
        return Err(Error::validation_invalid_argument(
            "artifact.sha256",
            "terminal artifact bytes do not match controller checksum metadata",
            Some(artifact.id.clone()),
            None,
        ));
    }
    Ok(())
}

pub fn mirror_daemon_evidence(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_id: Option<&str>,
    notification_route: Option<&NotificationRoute>,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let local_job_run = mirror_job_run(
        &store,
        runner,
        cwd,
        command,
        job,
        events,
        result,
        run_id,
        notification_route,
    )?;
    let result = (|| {
        let remote_runs =
            mirror_remote_observation_runs(&store, runner, job, result, notification_route)?;
        if remote_runs.is_empty() || job.status == JobStatus::Failed {
            mirror_terminal_job_artifacts(&store, runner, job, &local_job_run)?;
        }
        let patch = mirrored_patch_result(&store, runner, job, result.get("patch"))?;
        let mut runs = if remote_runs.is_empty() {
            vec![local_job_run.run.clone()]
        } else {
            remote_runs
        };
        if job.status == JobStatus::Failed && !runs.iter().any(|run| run.id == local_job_run.run.id)
        {
            // Remote observations describe workload output; retain the local
            // runner-exec record that owns terminal command diagnostics.
            runs.push(local_job_run.run.clone());
        }
        let runs = runs
            .into_iter()
            .map(|run| attach_controller_failure_artifact_refs(&store, run))
            .collect::<Result<Vec<_>>>()?;
        let primary_run = primary_mirrored_run(&runs).unwrap_or_else(|| runs[0].clone());
        Ok(Some(MirroredDaemonEvidence {
            run: primary_run,
            runs,
            patch,
        }))
    })();
    if result.is_err() {
        discard_owned_synthetic_run(&store, &local_job_run);
    }
    result
}

pub fn mirror_reverse_broker_evidence(
    runner: &Runner,
    broker_url: &str,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_id: Option<&str>,
    notification_route: Option<&NotificationRoute>,
) -> Result<Option<MirroredDaemonEvidence>> {
    let store = ObservationStore::open_initialized()?;
    let local_job_run = mirror_job_run(
        &store,
        runner,
        cwd,
        command,
        job,
        events,
        result,
        run_id,
        notification_route,
    )?;
    let result = (|| {
        let declared_run_ids = explicit_observation_run_ids(result, job);
        let terminal_result: homeboy_core::api_jobs::RemoteRunnerJobResult =
            serde_json::from_value(result.clone()).map_err(|error| {
                Error::validation_invalid_argument(
                    "result.observation_run_details",
                    format!(
                        "reverse runner terminal result is not a valid typed observation detail contract: {error}"
                    ),
                    None,
                    None,
                )
            })?;
        let runs;
        let local_artifacts = if job.status == JobStatus::Failed {
            Some(mirror_reverse_terminal_artifacts(
                &store,
                runner,
                broker_url,
                job,
                &local_job_run,
            )?)
        } else {
            None
        };
        if declared_run_ids.is_empty() {
            let artifacts = local_artifacts.unwrap_or(mirror_reverse_terminal_artifacts(
                &store,
                runner,
                broker_url,
                job,
                &local_job_run,
            )?);
            runs = vec![record_reverse_broker_metadata(
                &store,
                local_job_run.run.clone(),
                runner,
                broker_url,
                job,
                events,
                result,
                artifacts,
                None,
            )?];
        } else if terminal_result.observation_run_details.is_empty() {
            // Older workers declared remote run identities without retaining
            // typed details. Their job artifacts remain durable controller
            // evidence, while the declared identities are explicitly retained
            // as unresolved provenance rather than fabricated run records.
            let artifacts = local_artifacts.unwrap_or(mirror_reverse_terminal_artifacts(
                &store,
                runner,
                broker_url,
                job,
                &local_job_run,
            )?);
            runs = vec![record_reverse_broker_metadata(
                &store,
                local_job_run.run.clone(),
                runner,
                broker_url,
                job,
                events,
                result,
                artifacts,
                Some(&declared_run_ids),
            )?];
        } else {
            terminal_result.validate_observation_run_details()?;
            runs = mirror_remote_observation_runs_by_id_with_downloader(
                &store,
                runner,
                job,
                &declared_run_ids,
                notification_route,
                |declared_run_id| {
                    terminal_result
                        .observation_run_details
                        .iter()
                        .find(|detail| detail.run.id == declared_run_id)
                        .map(|detail| {
                            let mut run =
                                serde_json::to_value(&detail.run).expect("run record serializes");
                            run["artifacts"] = serde_json::to_value(&detail.artifacts)
                                .expect("artifact records serialize");
                            run
                        })
                        .map(Some)
                        .ok_or_else(|| {
                            missing_required_run_projection(runner, job, declared_run_id, None)
                        })
                },
                |artifact_path| {
                    let token = RemoteArtifactToken::parse(artifact_path).map_err(|_| {
                        Error::validation_invalid_argument(
                            "artifact.path",
                            "reverse declared artifact is not a runner artifact token",
                            Some(artifact_path.to_string()),
                            None,
                        )
                    })?;
                    if token.runner_id != runner.id || token.run_id != job.id.to_string() {
                        return Err(Error::validation_invalid_argument(
                            "artifact.path",
                            "reverse declared artifact token is not bound to this runner job",
                            Some(artifact_path.to_string()),
                            None,
                        ));
                    }
                    let bytes = terminal_artifact_bytes(
                        crate::connection::reverse_broker_artifact_content_at(
                            broker_url,
                            &runner.id,
                            &job.id.to_string(),
                            &token.artifact_id,
                        )?,
                        &token.artifact_id,
                    )?;
                    let mut file = tempfile::NamedTempFile::new().map_err(|error| {
                        Error::internal_io(
                            error.to_string(),
                            Some("stage reverse artifact".to_string()),
                        )
                    })?;
                    file.write_all(&bytes).map_err(|error| {
                        Error::internal_io(
                            error.to_string(),
                            Some("write reverse artifact".to_string()),
                        )
                    })?;
                    let (_, path) = file.keep().map_err(|error| {
                        Error::internal_io(
                            error.error.to_string(),
                            Some("persist reverse artifact stage".to_string()),
                        )
                    })?;
                    Ok(path)
                },
            )?;
        }
        let mut runs = runs
            .into_iter()
            .map(|run| attach_controller_failure_artifact_refs(&store, run))
            .collect::<Result<Vec<_>>>()?;
        let patch = mirrored_patch_result(
            &store,
            runner,
            job,
            result
                .get("patch")
                .or_else(|| result.pointer("/data/patch")),
        )?;
        if job.status == JobStatus::Failed && !runs.iter().any(|run| run.id == local_job_run.run.id)
        {
            runs.push(local_job_run.run.clone());
        }
        let runs = runs
            .into_iter()
            .map(|run| {
                record_reverse_broker_metadata(
                    &store,
                    run,
                    runner,
                    broker_url,
                    job,
                    events,
                    result,
                    Vec::new(),
                    None,
                )
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|run| attach_controller_failure_artifact_refs(&store, run))
            .collect::<Result<Vec<_>>>()?;
        let primary_run = primary_mirrored_run(&runs).unwrap_or_else(|| runs[0].clone());
        Ok(Some(MirroredDaemonEvidence {
            run: primary_run,
            runs,
            patch,
        }))
    })();
    if result.is_err() {
        discard_owned_synthetic_run(&store, &local_job_run);
    }
    result
}

fn mirror_reverse_terminal_artifacts(
    store: &ObservationStore,
    runner: &Runner,
    broker_url: &str,
    job: &Job,
    run: &RunRecord,
) -> Result<Vec<ArtifactRecord>> {
    mirror_terminal_job_artifacts_with(store, runner, job, run, |artifact_id| {
        terminal_artifact_bytes(
            crate::connection::reverse_broker_artifact_content_at(
                broker_url,
                &runner.id,
                &job.id.to_string(),
                artifact_id,
            )?,
            artifact_id,
        )
    })
}

fn record_reverse_broker_metadata(
    store: &ObservationStore,
    run: RunRecord,
    runner: &Runner,
    broker_url: &str,
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    artifacts: Vec<ArtifactRecord>,
    unresolved_run_refs: Option<&[String]>,
) -> Result<RunRecord> {
    let mut metadata = run.metadata_json.clone();
    let unresolved_run_refs = unresolved_run_refs.map(|refs| refs.to_vec()).or_else(|| {
        metadata
            .pointer("/lab/reverse_broker/unresolved_run_refs")
            .and_then(Value::as_array)
            .map(|refs| {
                refs.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|refs| !refs.is_empty())
    });
    metadata["lab"]["reverse_broker"] = json!({
        "runner_id": runner.id.clone(), "job_id": job.id.to_string(), "broker_url": broker_url,
        "status": job.status, "events": bounded_remote_events(events), "event_count": events.len(),
        "result_summary": result_summary(result), "artifacts": artifacts,
        "observation_run_details": if unresolved_run_refs.is_some() { json!("unavailable_legacy") } else { Value::Null },
        "unresolved_run_refs": unresolved_run_refs,
    });
    store.update_run_metadata(&run.id, metadata)
}

pub fn mirror_daemon_job_progress(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    run_id: Option<&str>,
) -> Result<RunRecord> {
    let store = ObservationStore::open_initialized()?;
    mirror_job_run(
        &store,
        runner,
        cwd,
        command,
        job,
        events,
        &json!({}),
        run_id,
        None,
    )
    .map(|run| run.run)
}

pub fn mirror_reverse_broker_job_progress(
    runner: &Runner,
    broker_url: &str,
    cwd: &str,
    command: &[String],
    job: &Job,
    run_id: Option<&str>,
) -> Result<RunRecord> {
    let store = ObservationStore::open_initialized()?;
    let run = mirror_job_run(
        &store,
        runner,
        cwd,
        command,
        job,
        &[],
        &json!({}),
        run_id,
        None,
    )?
    .run;
    record_reverse_broker_metadata(
        &store,
        run,
        runner,
        broker_url,
        job,
        &[],
        &json!({}),
        Vec::new(),
        None,
    )
}

/// Records that the controller can no longer observe an accepted runner job.
/// The remote job may still exist, but the controller-side lifecycle is terminal
/// and includes the polling diagnostic instead of leaving a stale running mirror.
pub fn terminalize_mirrored_daemon_job(
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    run_id: Option<&str>,
    diagnostic: &Value,
) -> Result<RunRecord> {
    let store = ObservationStore::open_initialized()?;
    let mut terminal_job = job.clone();
    terminal_job.status = JobStatus::Failed;
    terminal_job.finished_at_ms = Some(terminal_job.updated_at_ms.max(terminal_job.created_at_ms));
    let run = mirror_job_run(
        &store,
        runner,
        cwd,
        command,
        &terminal_job,
        &[],
        &json!({}),
        run_id,
        None,
    )?;
    let mut metadata = run.metadata_json.clone();
    metadata["lab"]["controller_terminal"] = json!({
        "status": "failed",
        "reason": "runner_job_unobservable",
        "diagnostic": diagnostic,
    });
    store.finish_run(&run.id, RunStatus::Fail, Some(metadata))
}

pub fn refresh_mirrored_daemon_evidence(run_id: &str) -> Result<Option<Vec<RunRecord>>> {
    let store = ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Ok(None);
    };
    let Some((runner_id, job_id)) = mirrored_runner_job_identity(&run) else {
        return Ok(None);
    };
    let runner = load(&runner_id)?;
    let reverse_broker_url = run
        .metadata_json
        .pointer("/lab/reverse_broker/broker_url")
        .and_then(Value::as_str);
    let (job, events) = match reverse_broker_url {
        Some(broker_url) => {
            crate::connection::reverse_broker_job_snapshot_at(broker_url, &runner_id, &job_id)?
        }
        None => (
            fetch_daemon_job(&runner_id, &job_id)?,
            fetch_daemon_events(&runner_id, &job_id)?,
        ),
    };
    let result = result_event_data(&events).unwrap_or_else(|| json!({}));
    let cwd = run.cwd.as_deref().unwrap_or("");
    let command = run
        .command
        .as_ref()
        .map(|command| vec![command.clone()])
        .unwrap_or_default();
    if let Some(broker_url) = reverse_broker_url {
        let mirrored = mirror_reverse_broker_evidence(
            &runner,
            broker_url,
            cwd,
            &command,
            &job,
            &events,
            &result,
            (run.kind == "runner-exec").then_some(run_id),
            None,
        )?;
        if matches!(
            job.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        ) {
            crate::connection::reverse_broker_reconcile_at(&runner_id, broker_url)?;
        }
        return Ok(mirrored.map(|evidence| evidence.runs));
    }
    refresh_mirrored_daemon_evidence_with(
        &store,
        &runner,
        cwd,
        &command,
        &job,
        &events,
        &result,
        (run.kind == "runner-exec").then_some(run_id),
        || {
            if matches!(
                job.status,
                JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
            ) {
                let report = super::super::status(&runner_id)?;
                super::super::generation_store::reconcile(&runner_id, report.session.as_ref())?;
            }
            Ok(())
        },
    )
}

pub(super) fn refresh_mirrored_daemon_evidence_with<F>(
    store: &ObservationStore,
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_id: Option<&str>,
    reconcile: F,
) -> Result<Option<Vec<RunRecord>>>
where
    F: FnOnce() -> Result<()>,
{
    let local_job_run = mirror_job_run(
        store, runner, cwd, command, job, events, result, run_id, None,
    )?;
    let result = (|| {
        reconcile()?;
        let remote_runs = mirror_remote_observation_runs(store, runner, job, result, None)?;
        let terminal = matches!(
            job.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        );
        if terminal && (remote_runs.is_empty() || job.status == JobStatus::Failed) {
            mirror_terminal_job_artifacts(store, runner, job, &local_job_run)?;
        }
        let mut runs = if remote_runs.is_empty() {
            vec![local_job_run.run.clone()]
        } else {
            remote_runs
        };
        if job.status == JobStatus::Failed && !runs.iter().any(|run| run.id == local_job_run.run.id)
        {
            runs.push(local_job_run.run.clone());
        }
        Ok(Some(
            runs.into_iter()
                .map(|run| attach_controller_failure_artifact_refs(store, run))
                .collect::<Result<Vec<_>>>()?,
        ))
    })();
    if result.is_err() {
        discard_owned_synthetic_run(store, &local_job_run);
    }
    result
}

pub fn mirror_connected_runner_run(run_id: &str) -> Result<Option<RunRecord>> {
    let store = ObservationStore::open_initialized()?;
    for report in super::super::connection::statuses()? {
        if !report.connected {
            continue;
        }
        let runner_id = report.runner_id;
        let runner = load(&runner_id)?;
        let Ok(data) = daemon_api_get(
            &runner_id,
            &format!("/runs/{}", encode_uri_component(run_id)),
        ) else {
            continue;
        };
        let body = canonical_daemon_body(&data, "runner run detail response")?;
        let Some(detail) = body.get("run") else {
            continue;
        };
        let run = remote_detail_to_run_record(detail, &runner, None)?;
        import_run_if_absent(&store, &run)?;
        for artifact in remote_detail_artifacts(detail, &runner, &run.id)? {
            import_mirrored_artifact(&store, &artifact)?;
        }
        return Ok(Some(run));
    }
    Ok(None)
}

pub fn runner_job_log_snapshot(runner_id: &str, job_id: &str) -> Result<RunnerJobLogSnapshot> {
    Ok(RunnerJobLogSnapshot {
        job: fetch_daemon_job(runner_id, job_id)?,
        events: fetch_daemon_events(runner_id, job_id)?,
    })
}

pub fn runner_job_log_snapshot_for_session(
    session: &crate::RunnerSession,
    job_id: &str,
) -> Result<RunnerJobLogSnapshot> {
    let job_data =
        crate::execution::daemon_api_get_for_session(session, &format!("/jobs/{job_id}"))?;
    let events_data =
        crate::execution::daemon_api_get_for_session(session, &format!("/jobs/{job_id}/events"))?;
    let job_body = canonical_daemon_body(&job_data, "daemon job response")?;
    let events_body = canonical_daemon_body(&events_data, "daemon job events response")?;
    Ok(RunnerJobLogSnapshot {
        job: serde_json::from_value(job_body["job"].clone()).map_err(|error| {
            Error::internal_json(error.to_string(), Some("parse daemon job".to_string()))
        })?,
        events: serde_json::from_value(events_body["events"].clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse daemon job events".to_string()),
            )
        })?,
    })
}

pub fn mirrored_runner_job_identity(run: &RunRecord) -> Option<(String, String)> {
    let lab = run.metadata_json.get("lab")?;
    let runner_id = lab
        .pointer("/runner/id")
        .or_else(|| lab.get("runner_id"))
        .and_then(Value::as_str)?;
    let job_id = lab
        .pointer("/remote_job/id")
        .or_else(|| lab.get("remote_job_id"))
        .and_then(Value::as_str)?;
    Some((runner_id.to_string(), job_id.to_string()))
}

fn fetch_daemon_job(runner_id: &str, job_id: &str) -> Result<Job> {
    let data = daemon_api_get(runner_id, &format!("/jobs/{job_id}"))?;
    let body = canonical_daemon_body(&data, "daemon job response")?;
    serde_json::from_value(body["job"].clone())
        .map_err(|err| Error::internal_json(err.to_string(), Some("parse daemon job".to_string())))
}

fn fetch_daemon_events(runner_id: &str, job_id: &str) -> Result<Vec<JobEvent>> {
    let data = daemon_api_get(runner_id, &format!("/jobs/{job_id}/events"))?;
    let body = canonical_daemon_body(&data, "daemon job events response")?;
    serde_json::from_value(body["events"].clone()).map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon job events".to_string()))
    })
}

pub(super) fn mirrored_patch_result(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    patch: Option<&Value>,
) -> Result<Option<Value>> {
    let Some(patch) = patch.filter(|patch| !patch.is_null()) else {
        return Ok(None);
    };
    let Some(artifact_id) = patch.get("patch_artifact_id").and_then(Value::as_str) else {
        return Ok(Some(patch.clone()));
    };
    if artifact_id.is_empty() {
        return Ok(Some(patch.clone()));
    }

    let artifact = store
        .get_artifact(artifact_id)?
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "runner capture-patch artifact {artifact_id} was reported by job '{}', but no controller-owned artifact record is available",
                job.id
            ))
        })?;
    let advertised = job
        .artifacts
        .iter()
        .any(|candidate| candidate.id == artifact_id);
    let owned_by_job = store.get_run(&artifact.run_id)?.is_some_and(|run| {
        run.metadata_json
            .pointer("/lab/runner/id")
            .or_else(|| run.metadata_json.pointer("/lab/runner_id"))
            .and_then(Value::as_str)
            == Some(runner.id.as_str())
            && run
                .metadata_json
                .pointer("/lab/remote_job/id")
                .or_else(|| run.metadata_json.pointer("/lab/remote_job_id"))
                .and_then(Value::as_str)
                == Some(job.id.to_string().as_str())
    });
    if !advertised || !owned_by_job || artifact.artifact_type != "file" {
        return Err(Error::validation_invalid_argument(
            "patch.patch_artifact_id",
            "patch artifact must be a controller-owned local file advertised by this exact runner job",
            Some(artifact_id.to_string()),
            None,
        ));
    }

    let mut patched = patch.clone();
    if let Some(object) = patched.as_object_mut() {
        object.insert(
            "patch_artifact_path".to_string(),
            Value::String(artifact.path),
        );
    }
    Ok(Some(patched))
}

pub(super) fn mirror_job_run(
    store: &ObservationStore,
    runner: &Runner,
    cwd: &str,
    command: &[String],
    job: &Job,
    events: &[JobEvent],
    result: &Value,
    run_id: Option<&str>,
    notification_route: Option<&NotificationRoute>,
) -> Result<MirroredJobRun> {
    let inferred_label = runner_exec_run_label(command);
    let synthetic_token = if run_id.is_none() {
        let synthetic_id = local_job_run_id(&runner.id, &job.id.to_string(), &inferred_label);
        store
            .get_run(&synthetic_id)?
            .and_then(|run| {
                run.metadata_json
                    .pointer("/lab/synthetic_publication_token")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| Uuid::new_v4().to_string())
    } else {
        String::new()
    };
    let agent_task_lifecycle_event = agent_task_run_plan_lifecycle_event_from_value(result)
        .or_else(|| agent_task_run_plan_lifecycle_event_from_job_events(Some(events)))
        .and_then(|event| serde_json::to_value(event).ok());
    let mut lab = json!({
        "runner": runner_metadata(runner),
        "remote_job": job,
        // Events can contain streamed executor output. Keep recent lifecycle
        // evidence in the run row; full runner logs remain retrievable from
        // the daemon and artifact references remain in the job/result summary.
        "remote_events": bounded_remote_events(events),
        "remote_event_count": events.len(),
        "result_summary": result_summary(result),
        "source_snapshot": source_snapshot_from_result(result),
        "run_label": inferred_label,
        "explicit_run_id": run_id,
    });
    if let Some(failure) = runner_failure_projection(runner, job, result) {
        lab["failure"] = failure;
    }
    if let Some(event) = agent_task_lifecycle_event {
        lab["agent_task_lifecycle_event"] = event;
    }
    if run_id.is_none() {
        lab["synthetic_publication_token"] = Value::String(synthetic_token.clone());
    }
    // Every runner-controlled value crosses this boundary through recursive
    // redaction; numeric/status provenance remains typed and intact.
    lab = homeboy_core::redaction::redact_json(&lab);
    if run_id.is_none() {
        // Controller-generated ownership capability, never runner input.
        lab["synthetic_publication_token"] = Value::String(synthetic_token.clone());
    }
    if let Some(run_id) = run_id {
        if store
            .get_run(run_id)?
            .is_some_and(|existing| existing.metadata_json.get("agent_task_run").is_some())
        {
            homeboy_agents::agent_task_lifecycle::record_detached_lab_run(
                homeboy_agents::agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: &runner.id,
                    runner_job_id: &job.id.to_string(),
                    remote_workspace: cwd,
                    remote_command: command,
                },
            )?;
            let mut metadata_json = store
                .get_run(run_id)?
                .ok_or_else(|| {
                    Error::internal_unexpected(format!(
                        "agent-task run {run_id} disappeared while mirroring Lab evidence"
                    ))
                })?
                .metadata_json;
            metadata_json["lab"] = lab;
            if let Some(notification_route) = notification_route {
                notification_route.insert_into_metadata(&mut metadata_json);
            }
            return store
                .update_run_metadata(run_id, metadata_json)
                .map(|run| MirroredJobRun {
                    run,
                    synthetic_ownership: None,
                });
        }
    }
    let run = RunRecord {
        id: run_id
            .map(str::to_string)
            .unwrap_or_else(|| local_job_run_id(&runner.id, &job.id.to_string(), &inferred_label)),
        kind: "runner-exec".to_string(),
        component_id: None,
        started_at: ms_to_rfc3339(job.started_at_ms.unwrap_or(job.created_at_ms)),
        finished_at: job.finished_at_ms.map(ms_to_rfc3339),
        status: job_status_as_run_status(job.status).to_string(),
        command: Some(redact_argv_shell_display(command)),
        cwd: Some(cwd.to_string()),
        homeboy_version: result
            .pointer("/data/execution_record/orchestration_provenance/job_command_binary/version")
            .or_else(|| {
                result.pointer(
                    "/data/execution_record/orchestration_provenance/configured_job_binary/version",
                )
            })
            .and_then(Value::as_str)
            .map(str::to_string),
        git_sha: None,
        rig_id: None,
        metadata_json: json!({
            "exit_code": runner_terminal_exit_code(job, result),
            "lab": lab,
        }),
    };
    let mut run = run;
    if let Some(notification_route) = notification_route {
        notification_route.insert_into_metadata(&mut run.metadata_json);
    }
    let synthetic_ownership = if run_id.is_none() {
        let inserted = store.import_synthetic_run(&run, &synthetic_token)?;
        if !inserted && job.status.is_terminal() {
            // A progress mirror owns the same deterministic synthetic ID. Upsert
            // its terminal result so restart/reconcile cannot leave it running.
            store.upsert_imported_run_preserving_terminal(&run)?;
        }
        inserted.then(|| SyntheticRunOwnership {
            run_id: run.id.clone(),
            publication_token: synthetic_token,
        })
    } else {
        import_run_if_absent(store, &run)?;
        None
    };
    store
        .get_run(&run.id)?
        .map(|run| MirroredJobRun {
            run,
            synthetic_ownership,
        })
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "mirrored runner run {} but could not read it back",
                run.id
            ))
        })
}

pub(super) fn bounded_remote_events(events: &[JobEvent]) -> Vec<Value> {
    events
        .iter()
        .rev()
        .take(MIRRORED_REMOTE_EVENT_LIMIT)
        .rev()
        .map(|event| {
            json!({
                "sequence": event.sequence,
                "job_id": event.job_id,
                "kind": event.kind,
                "timestamp_ms": event.timestamp_ms,
                "message": event.message.as_deref().map(|message| {
                    message
                        .chars()
                        .take(MIRRORED_REMOTE_EVENT_MESSAGE_LIMIT)
                        .collect::<String>()
                }),
            })
        })
        .collect()
}

/// Preserve the terminal diagnosis the runner already emitted without copying
/// its unbounded event stream into the controller-owned observation row.
fn runner_failure_projection(runner: &Runner, job: &Job, result: &Value) -> Option<Value> {
    let exit_code = runner_terminal_exit_code(job, result)?;
    if exit_code == 0 {
        return None;
    }
    // Hash exactly the original terminal stream bytes; display only its
    // recursively redacted bounded tail so the digest remains reproducible.
    let stderr_original = result
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = homeboy_core::redaction::redact_string(stderr_original);
    let error = [
        result.get("error"),
        result.pointer("/data/error"),
        result.pointer("/data/outcome/result/error"),
        result.pointer("/outcome/result/error"),
    ]
    .into_iter()
    .flatten()
    .find(|value| value.is_object())
    .map(homeboy_core::redaction::redact_json);
    let artifact_refs = [result.get("artifacts"), result.get("artifact_refs")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|artifact| {
            let id = artifact.get("id").and_then(Value::as_str)?;
            Some(json!({
                "id": id,
                "name": artifact.get("name"),
                "path": artifact.get("path"),
                "url": artifact.get("url"),
            }))
        })
        .collect::<Vec<_>>();
    Some(json!({
        "schema": "homeboy/runner-exec-failure-projection/v1",
        "failure_code": error.as_ref().and_then(|value| value.get("code")).cloned(),
        "message": error.as_ref().and_then(|value| value.get("message")).cloned()
            .or_else(|| result.get("error").filter(|value| value.is_string()).map(homeboy_core::redaction::redact_json)),
        "details": error.as_ref().and_then(|value| value.get("details")).cloned(),
        "phase": result.get("phase").cloned().or_else(|| result.pointer("/data/phase").cloned()),
        "exit_code": exit_code,
        "signal": result.get("signal").cloned(),
        "stderr_tail": stderr.chars().rev().take(4_096).collect::<String>().chars().rev().collect::<String>(),
        "stderr_sha256": format!("{:x}", Sha256::digest(stderr_original.as_bytes())),
        "runner_id": runner.id.clone(),
        "runner_job_id": job.id.to_string(),
        "runner_job_logs_command": format!("homeboy runner job logs {} {}", runner.id, job.id),
        "remote_command_result_command": format!("homeboy runner job logs {} {} --json", runner.id, job.id),
        "source_snapshot": job.source_snapshot.clone(),
        "path_materialization_plan": job.path_materialization_plan.clone(),
        "runner_job_projection": job.runner_job_projection.clone(),
        "execution_record": result.pointer("/data/execution_record").cloned(),
        "orchestration_provenance": result.pointer("/data/orchestration_provenance").cloned(),
        "artifact_refs": artifact_refs,
    }))
}

fn runner_terminal_exit_code(job: &Job, result: &Value) -> Option<i64> {
    result
        .get("exit_code")
        .and_then(Value::as_i64)
        .or((job.status == JobStatus::Failed).then_some(1))
}

/// Failure evidence never exposes a runner artifact token. Only artifacts the
/// controller already verified and persisted are made resolvable from evidence.
fn attach_controller_failure_artifact_refs(
    store: &ObservationStore,
    mut run: RunRecord,
) -> Result<RunRecord> {
    let Some(failure) = run.metadata_json.pointer_mut("/lab/failure") else {
        return Ok(run);
    };
    let artifacts = store.list_artifacts(&run.id)?;
    let refs = artifacts
        .into_iter()
        .filter_map(|artifact| {
            Some(json!({
                "id": artifact.id,
                "kind": artifact.kind,
                "path": format!("homeboy://run/{}/artifact/{}", run.id, artifact.id),
                "sha256": artifact.sha256?,
                "size_bytes": artifact.size_bytes?,
            }))
        })
        .collect::<Vec<_>>();
    failure["artifact_refs"] = Value::Array(refs);
    run = store.update_run_metadata(&run.id, run.metadata_json)?;
    Ok(run)
}

fn mirror_remote_observation_runs(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    result: &Value,
    notification_route: Option<&NotificationRoute>,
) -> Result<Vec<RunRecord>> {
    let explicit_run_ids = explicit_observation_run_ids(result, job);
    if !explicit_run_ids.is_empty() {
        return mirror_remote_observation_runs_by_id(
            store,
            runner,
            job,
            &explicit_run_ids,
            notification_route,
        );
    }

    // A timestamp window can contain unrelated concurrent runner work. Without
    // a result, artifact, or submitted durable-run reference, the job mirror is
    // the only evidence whose ownership is proven.
    Ok(Vec::new())
}

/// Legacy terminal producers may advertise artifacts without a durable
/// observation run. Their controller-owned runner-exec mirror is the stable
/// terminal identity, and every advertised job artifact must be materialized
/// before that terminal response can be returned.
fn mirror_terminal_job_artifacts(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    run: &RunRecord,
) -> Result<Vec<ArtifactRecord>> {
    mirror_terminal_job_artifacts_with(store, runner, job, run, |artifact_id| {
        terminal_artifact_bytes(
            crate::runner_artifact_content(&runner.id, &job.id.to_string(), artifact_id)?,
            artifact_id,
        )
    })
}

fn terminal_artifact_bytes(response: Value, artifact_id: &str) -> Result<Vec<u8>> {
    let content = response
        .get("content_base64")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "artifact.content_base64",
                "terminal artifact retrieval did not return bytes",
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    base64::engine::general_purpose::STANDARD
        .decode(content)
        .map_err(|error| {
            Error::validation_invalid_argument(
                "artifact.content_base64",
                format!("terminal artifact bytes are not valid base64: {error}"),
                Some(artifact_id.to_string()),
                None,
            )
        })
}

pub(super) fn mirror_terminal_job_artifacts_with<F>(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    run: &RunRecord,
    mut fetch: F,
) -> Result<Vec<ArtifactRecord>>
where
    F: FnMut(&str) -> Result<Vec<u8>>,
{
    let mut staged = Vec::with_capacity(job.artifacts.len());
    let mut publications = Vec::with_capacity(job.artifacts.len());
    for artifact in &job.artifacts {
        let size_bytes = artifact
            .size_bytes
            .and_then(|size| i64::try_from(size).ok());
        let sha256 = artifact.sha256.clone().filter(|sha| !sha.is_empty());
        if size_bytes.is_none() || sha256.is_none() {
            return Err(artifact_projection_error(
                runner,
                job,
                &run.id,
                Error::validation_invalid_argument(
                    "artifact.provenance",
                    "terminal artifact is missing required size_bytes or sha256 provenance",
                    Some(artifact.id.clone()),
                    None,
                ),
            ));
        }
        let mut file = tempfile::NamedTempFile::new().map_err(|error| {
            artifact_projection_error(
                runner,
                job,
                &run.id,
                Error::internal_io(
                    error.to_string(),
                    Some("stage terminal artifact".to_string()),
                ),
            )
        })?;
        file.write_all(
            &fetch(&artifact.id)
                .map_err(|error| artifact_projection_error(runner, job, &run.id, error))?,
        )
        .map_err(|error| {
            artifact_projection_error(
                runner,
                job,
                &run.id,
                Error::internal_io(
                    error.to_string(),
                    Some("write terminal artifact".to_string()),
                ),
            )
        })?;
        publications.push(ArtifactPublication {
            id: artifact.id.clone(),
            kind: artifact
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("runner_job_artifact")
                .to_string(),
            source_path: file.path().to_path_buf(),
            mime: artifact.mime.clone(),
            metadata_json: json!({
                "runner_id": runner.id,
                "runner_job_id": job.id.to_string(),
                "name": artifact.name,
                "terminal_artifact": true,
            }),
            expected_size_bytes: size_bytes,
            expected_sha256: sha256,
        });
        staged.push(file);
    }
    store
        .publish_run_artifacts_atomically(run, &publications)
        .map_err(|error| artifact_projection_error(runner, job, &run.id, error))
}

fn mirror_remote_observation_runs_by_id(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    run_ids: &[String],
    notification_route: Option<&NotificationRoute>,
) -> Result<Vec<RunRecord>> {
    mirror_remote_observation_runs_by_id_with(
        store,
        runner,
        job,
        run_ids,
        notification_route,
        |run_id| {
            let detail_data = daemon_api_get(
                &runner.id,
                &format!("/runs/{}", encode_uri_component(run_id)),
            )?;
            let detail_body = canonical_daemon_body(&detail_data, "runner run detail response")?;
            Ok(detail_body.get("run").cloned())
        },
    )
}

/// Project terminal runner observations and their artifacts into controller
/// storage before terminal command output can expose their references.
///
/// A runner result that declares an observation run ID has made that run part
/// of its reviewer-facing contract. Treating a missing `/runs/<id>` response as
/// optional leaves callers with URLs that only existed on the disconnected
/// runner, so reconciliation must fail closed instead.
pub(super) fn mirror_remote_observation_runs_by_id_with<F>(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    run_ids: &[String],
    notification_route: Option<&NotificationRoute>,
    fetch_detail: F,
) -> Result<Vec<RunRecord>>
where
    F: FnMut(&str) -> Result<Option<Value>>,
{
    mirror_remote_observation_runs_by_id_with_downloader(
        store,
        runner,
        job,
        run_ids,
        notification_route,
        fetch_detail,
        |path| {
            // Internal: mirroring reads these bytes once to build the local
            // record and never returns the path to an operator (#10585).
            Ok(super::download::download_remote_artifact_with_intent(
                path,
                None,
                RunnerDownloadIntent::InternalFetch,
            )?
            .output_path)
        },
    )
}

pub(super) fn mirror_remote_observation_runs_by_id_with_downloader<F, D>(
    store: &ObservationStore,
    runner: &Runner,
    job: &Job,
    run_ids: &[String],
    notification_route: Option<&NotificationRoute>,
    mut fetch_detail: F,
    mut download: D,
) -> Result<Vec<RunRecord>>
where
    F: FnMut(&str) -> Result<Option<Value>>,
    D: FnMut(&str) -> Result<std::path::PathBuf>,
{
    let mut mirrored = Vec::new();
    for run_id in run_ids {
        let detail = match fetch_detail(run_id) {
            Ok(Some(detail)) => detail,
            Ok(None) => return Err(missing_required_run_projection(runner, job, run_id, None)),
            Err(error) if missing_optional_run_projection(&error, run_id) => {
                return Err(missing_required_run_projection(
                    runner,
                    job,
                    run_id,
                    Some(error),
                ))
            }
            Err(error) => return Err(error),
        };
        let mut run = remote_detail_to_run_record(&detail, runner, Some(job))?;
        if let Some(notification_route) = notification_route {
            notification_route.insert_into_metadata(&mut run.metadata_json);
        }
        let publications = remote_detail_artifacts(&detail, runner, &run.id)?
            .into_iter()
            .map(|artifact| {
                if artifact.size_bytes.is_none()
                    || artifact.sha256.as_deref().is_none_or(str::is_empty)
                {
                    return Err(Error::validation_invalid_argument(
                        "artifact.provenance",
                        "declared terminal artifact is missing required size_bytes or sha256 provenance",
                        Some(artifact.id),
                        None,
                    ));
                }
                let source_path = download(&artifact.path)?;
                Ok(ArtifactPublication {
                    id: artifact.id,
                    kind: artifact.kind,
                    source_path,
                    mime: artifact.mime,
                    metadata_json: artifact.metadata_json,
                    expected_size_bytes: artifact.size_bytes,
                    expected_sha256: artifact.sha256,
                })
            })
            .collect::<Result<Vec<_>>>()
            .map_err(|error| artifact_projection_error(runner, job, run_id, error))?;
        store
            .publish_run_artifacts_atomically(&run, &publications)
            .map_err(|error| artifact_projection_error(runner, job, run_id, error))?;
        mirrored.push(
            store.get_run(&run.id)?.ok_or_else(|| {
                Error::internal_unexpected("published controller run is unreadable")
            })?,
        );
    }
    Ok(mirrored)
}

fn artifact_projection_error(runner: &Runner, job: &Job, run_id: &str, source: Error) -> Error {
    let retryable = !permanent_artifact_provenance_error(&source);
    Error::new(
        source.code,
        format!(
            "controller could not durably project all artifacts for Lab runner job '{}' run '{}'; terminal output is withheld",
            job.id, run_id
        ),
        json!({
            "runner_id": runner.id,
            "runner_job_id": job.id.to_string(),
            "run_id": run_id,
            "source_error": { "code": source.code.as_str(), "message": source.message, "details": source.details },
            "reconciliation_command": format!("homeboy runs show {run_id}"),
            "artifact_reconciliation_command": format!("homeboy runs artifacts {run_id}"),
        }),
    )
    .with_retryable(retryable)
    .with_hint(if retryable {
        format!("Reconnect runner '{}' and retry job '{}' to reconcile its complete artifact set.", runner.id, job.id)
    } else {
        format!("Repair the malformed artifact provenance reported for job '{}' before retrying.", job.id)
    })
}

/// Only invalid producer provenance is terminal. Transport/session errors may
/// use validation-shaped public errors (for example an absent live session),
/// so code alone cannot turn every invalid-argument into a permanent result.
fn permanent_artifact_provenance_error(error: &Error) -> bool {
    matches!(error.code, ErrorCode::ValidationInvalidArgument)
        && matches!(
            error.details.get("field").and_then(Value::as_str),
            Some("artifact.provenance")
                | Some("artifact.size_bytes")
                | Some("artifact.sha256")
                | Some("artifact.content_base64")
        )
}

fn missing_required_run_projection(
    runner: &Runner,
    job: &Job,
    run_id: &str,
    source: Option<Error>,
) -> Error {
    let permanently_missing = source
        .as_ref()
        .is_some_and(|error| missing_optional_run_projection(error, run_id));
    let mut details = json!({
        "runner_id": runner.id,
        "runner_job_id": job.id.to_string(),
        "run_id": run_id,
        "reconciliation_command": format!("homeboy runs show {run_id}"),
        "artifact_reconciliation_command": format!("homeboy runs artifacts {run_id}"),
    });
    if let Some(source) = source {
        details["source_error"] = json!({
            "code": source.code.as_str(),
            "message": source.message,
            "details": source.details,
        });
    }
    Error::new(
        ErrorCode::InternalUnexpected,
        format!(
            "Lab runner job '{}' completed, but controller durability projection for declared run '{}' is unavailable; terminal output is withheld until its run and artifacts are persisted",
            job.id, run_id
        ),
        details,
    )
    .with_retryable(!permanently_missing)
    .with_hint(if permanently_missing {
        format!(
            "The runner no longer has declared run '{run_id}'. Repair the producer provenance for job '{}' and rerun it before sharing artifact URLs.",
            job.id
        )
    } else {
        format!(
            "Reconnect runner '{}' and retry the command to reconcile job '{}' before sharing artifact URLs.",
            runner.id, job.id
        )
    })
}

fn missing_optional_run_projection(error: &Error, run_id: &str) -> bool {
    error.details.get("http_status").and_then(Value::as_u64) == Some(404)
        && error.details.get("path").and_then(Value::as_str)
            == Some(&format!("/runs/{}", encode_uri_component(run_id)))
}

fn import_run_if_absent(store: &ObservationStore, run: &RunRecord) -> Result<()> {
    store.upsert_imported_run(run)
}

fn import_artifact_if_absent(store: &ObservationStore, artifact: &ArtifactRecord) -> Result<()> {
    if store.get_artifact(&artifact.id)?.is_some() {
        return Ok(());
    }
    store.import_artifact(artifact)
}

/// A completed remote observation has already published immutable artifact
/// identity and integrity metadata. Materialize those bytes in the controller
/// store before terminal daemon-job retention can remove the remote lookup.
fn import_mirrored_artifact(store: &ObservationStore, artifact: &ArtifactRecord) -> Result<()> {
    import_mirrored_artifact_with_downloader(store, artifact, |path| {
        // Internal: materializing mirrored bytes for the controller store, not
        // an operator pull (#10585).
        Ok(super::download::download_remote_artifact_with_intent(
            path,
            None,
            RunnerDownloadIntent::InternalFetch,
        )?
        .output_path)
    })
}

pub(super) fn import_mirrored_artifact_with_downloader<F>(
    store: &ObservationStore,
    artifact: &ArtifactRecord,
    download: F,
) -> Result<()>
where
    F: FnOnce(&str) -> Result<std::path::PathBuf>,
{
    // Only a fully identified remote artifact has a durable-byte contract.
    // References without integrity metadata remain on the live-fetch path.
    if artifact.artifact_type != "remote_file"
        || artifact.size_bytes.is_none()
        || artifact.sha256.as_deref().is_none_or(str::is_empty)
    {
        return import_artifact_if_absent(store, artifact);
    }
    if let Some(existing) = store.get_artifact(&artifact.id)? {
        if existing.run_id == artifact.run_id
            && existing.kind == artifact.kind
            && existing.size_bytes == artifact.size_bytes
            && existing.sha256 == artifact.sha256
            && existing.artifact_type == "file"
        {
            return Ok(());
        }
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "mirrored artifact `{}` conflicts with existing controller ownership or integrity metadata",
                artifact.id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    let downloaded_path = download(&artifact.path)?;
    store.record_verified_artifact_with_id(
        &artifact.run_id,
        &artifact.kind,
        downloaded_path,
        &artifact.id,
        artifact.size_bytes,
        artifact.sha256.as_deref(),
        artifact.metadata_json.clone(),
    )?;
    Ok(())
}

pub(super) fn primary_mirrored_run(remote_runs: &[RunRecord]) -> Option<RunRecord> {
    remote_runs.iter().find(|run| run.kind == "fuzz").cloned()
}
