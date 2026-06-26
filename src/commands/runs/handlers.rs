//! Observation-store handlers for the `runs` subcommands.
//!
//! These functions own the local observation-store reads and the mirrored
//! runner-job composition that back `runs list/show/resume-plan/artifacts` and
//! the `runs artifact` retrieval/cleanup subcommands.

use std::path::PathBuf;

use serde_json::Value;

use homeboy::core::observation::runs_service;
use homeboy::core::observation::{FindingListFilter, ObservationStore, RunListFilter, RunRecord};
use homeboy::core::validation_progress::ValidationProgressLedger;
use homeboy::core::Error;
use homeboy::core::{api_jobs, runners as runner};

use super::bench::run_contains_scenario;
use super::common::{run_summaries_with_artifact_indexes, RunSummary};
use super::types::{
    RunDetail, RunsArtifactArgs, RunsArtifactCommand, RunsArtifactGetArgs, RunsArtifactGetOutput,
    RunsArtifactsOutput, RunsEnvKeyOutput, RunsEnvOutput, RunsEnvSourceLayerOutput, RunsEnvSummary,
    RunsListArgs, RunsListOutput, RunsOutput, RunsResumePlanOutput, RunsShowOutput,
};
use super::{reconcile, remote, remote_artifact, CmdResult};

pub fn list_runs(args: RunsListArgs, command: &'static str) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.clone() {
        return remote::list_runner_runs(&runner_id, args, command);
    }

    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    let status_filter = args.status.clone();
    let run_records = store.list_runs(RunListFilter {
        kind: args.kind,
        component_id: args.component_id,
        status: args.status,
        rig_id: args.rig,
        limit: Some(args.limit),
    })?;
    let run_records = run_records
        .into_iter()
        .filter(|run| {
            args.scenario_id
                .as_deref()
                .is_none_or(|scenario| run_contains_scenario(run, scenario))
        })
        .collect::<Vec<_>>();
    let mut runs = run_summaries_with_artifact_indexes(&store, run_records)?;

    if args.include_active_runner_jobs {
        runs.extend(active_runner_job_summaries(status_filter.as_deref()));
    }

    Ok((RunsOutput::List(RunsListOutput { command, runs }), 0))
}

fn active_runner_job_summaries(status: Option<&str>) -> Vec<RunSummary> {
    runner::statuses()
        .unwrap_or_default()
        .into_iter()
        .filter(|report| report.connected)
        .flat_map(|report| report.active_jobs)
        .filter(|job| match status {
            Some(status) => status == job.status.run_status_label(),
            None => true,
        })
        .map(active_runner_job_run_summary)
        .collect()
}

fn active_runner_job_run_summary(job: api_jobs::ActiveRunnerJobSummary) -> RunSummary {
    let summary = api_jobs::active_runner_job_run_summary(job);
    RunSummary {
        id: summary.id,
        kind: summary.kind,
        status: summary.status,
        started_at: summary.started_at,
        finished_at: None,
        component_id: None,
        rig_id: None,
        git_sha: None,
        command: Some(summary.command),
        cwd: summary.cwd,
        status_note: Some(summary.status_note),
        artifact_index: None,
    }
}

pub fn show_run(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    runs_service::require_run(&store, run_id)?;
    runs_service::refresh_mirrored_daemon_evidence_best_effort(run_id);
    let run = runs_service::require_run(&store, run_id)?;
    Ok((
        RunsOutput::Show(RunsShowOutput {
            command: "runs.show",
            run: run_detail(&store, run)?,
        }),
        0,
    ))
}

pub fn resume_plan(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    let run = runs_service::require_run(&store, run_id)?;
    let Some(ledger) = validation_progress_ledger_for_run(&run) else {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` does not contain validation progress metadata"),
            Some(run_id.to_string()),
            Some(vec![
                "Run `homeboy runs show <run-id>` to inspect available metadata.".to_string(),
                "Validation progress is recorded for Homeboy-managed validation command sets with a run directory.".to_string(),
            ]),
        ));
    };

    Ok((
        RunsOutput::ResumePlan(RunsResumePlanOutput {
            command: "runs.resume-plan",
            run_id: run_id.to_string(),
            status: ledger.status.clone(),
            completed_count: ledger.completed_count,
            command_count: ledger.command_count,
            failed_count: ledger.failed_count,
            last_completed_command: ledger.last_completed_command.clone(),
            active_command: ledger.active_command.clone(),
            next_command: ledger.next_command.clone(),
            hints: ledger.resume_hints(),
        }),
        0,
    ))
}

fn validation_progress_ledger_for_run(run: &RunRecord) -> Option<ValidationProgressLedger> {
    ValidationProgressLedger::from_run(run).or_else(|| {
        run.metadata_json
            .get("run_dir")
            .and_then(Value::as_str)
            .and_then(|path| {
                homeboy::core::engine::run_dir::RunDir::from_existing(PathBuf::from(path)).ok()
            })
            .and_then(|run_dir| ValidationProgressLedger::read_from_run_dir(&run_dir))
    })
}

pub fn artifacts(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let artifacts = runs_service::list_artifacts_for_run(&store, run_id)?;
    let preview_entrypoints = artifacts
        .iter()
        .flat_map(homeboy::core::artifacts::html_preview_entrypoints)
        .collect();
    let findings = store.list_findings(FindingListFilter {
        run_id: Some(run_id.to_string()),
        tool: None,
        file: None,
        fingerprint: None,
        limit: Some(10_000),
    })?;
    let matrix_summary =
        homeboy::core::artifacts::summarize_matrix_artifacts(run_id, &artifacts, &findings);
    let fuzz_result_envelopes = artifacts
        .iter()
        .filter_map(homeboy::core::fuzz::inspect_fuzz_result_envelope_artifact)
        .collect();
    Ok((
        RunsOutput::Artifacts(RunsArtifactsOutput {
            command: "runs.artifacts",
            run_id: run_id.to_string(),
            artifacts,
            preview_entrypoints,
            matrix_summary,
            fuzz_result_envelopes,
        }),
        0,
    ))
}

pub fn env(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let run = runs_service::require_run(&store, run_id)?;
    let Some(envelope) = run.metadata_json.get("env_resolution").cloned() else {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` does not contain Lab environment provenance metadata"),
            Some(run_id.to_string()),
            Some(vec![
                "Environment provenance is recorded for Lab-offloaded runs that include `homeboy/env-resolution/v1` metadata.".to_string(),
                "Run `homeboy runs show <run-id> --json` to inspect available metadata keys.".to_string(),
            ]),
        ));
    };

    let schema = envelope
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != "homeboy/env-resolution/v1" {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` contains unsupported environment provenance schema `{schema}`"),
            Some(run_id.to_string()),
            Some(vec![
                "Expected `homeboy/env-resolution/v1`.".to_string(),
                "Run `homeboy runs show <run-id> --json` to inspect the raw metadata shape."
                    .to_string(),
            ]),
        ));
    }

    let values_redacted = envelope
        .get("values_redacted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !values_redacted {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` environment provenance is not marked redacted"),
            Some(run_id.to_string()),
            Some(vec![
                "Homeboy refuses to print unredacted environment provenance.".to_string(),
                "Capture a fresh Lab run with `homeboy/env-resolution/v1` redacted provenance metadata.".to_string(),
            ]),
        ));
    }
    let keys = envelope
        .get("keys")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(env_key_output)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let summary = RunsEnvSummary {
        key_count: keys.len(),
        secret_key_count: keys
            .iter()
            .filter(|entry| entry.classification == "secret")
            .count(),
        public_key_count: keys
            .iter()
            .filter(|entry| entry.classification == "public")
            .count(),
        shadowed_key_count: keys
            .iter()
            .filter(|entry| !entry.shadowed_source_layers.is_empty())
            .count(),
    };

    Ok((
        RunsOutput::Env(RunsEnvOutput {
            command: "runs.env",
            run_id: run_id.to_string(),
            schema: schema.to_string(),
            values_redacted,
            summary,
            keys,
        }),
        0,
    ))
}

fn env_key_output(value: &Value) -> Option<RunsEnvKeyOutput> {
    Some(RunsEnvKeyOutput {
        key: value.get("key")?.as_str()?.to_string(),
        classification: value
            .get("classification")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value_status: value
            .get("value_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value_preview: value
            .get("value_preview")
            .and_then(Value::as_str)
            .unwrap_or("<redacted>")
            .to_string(),
        winning_source_layer: value
            .get("winning_source_layer")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        shadowed_source_layers: value
            .get("shadowed_source_layers")
            .and_then(Value::as_array)
            .map(|layers| {
                layers
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        source_layers: value
            .get("source_layers")
            .and_then(Value::as_array)
            .map(|layers| layers.iter().filter_map(env_source_layer_output).collect())
            .unwrap_or_default(),
    })
}

fn env_source_layer_output(value: &Value) -> Option<RunsEnvSourceLayerOutput> {
    Some(RunsEnvSourceLayerOutput {
        source: value.get("source")?.as_str()?.to_string(),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        classification: value
            .get("classification")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value_status: value
            .get("value_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

pub fn artifact_command(args: RunsArtifactArgs) -> CmdResult<RunsOutput> {
    match args.command {
        RunsArtifactCommand::Attach(args) => remote_artifact::attach(args),
        RunsArtifactCommand::Get(args) => artifact_get(args),
        RunsArtifactCommand::Preview(args) => remote_artifact::preview(args),
        RunsArtifactCommand::Capture(args) => remote_artifact::capture(args),
        RunsArtifactCommand::CleanupDownloads(args) => remote_artifact::cleanup_downloads(args),
        RunsArtifactCommand::CleanupPersisted(args) => remote_artifact::cleanup_persisted(args),
    }
}

pub(crate) fn artifact_get(args: RunsArtifactGetArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let artifact = runs_service::resolve_artifact_for_run(&store, &args.run_id, &args.artifact_id)?;

    match runs_service::classify_artifact_storage(&artifact) {
        runs_service::ArtifactStorage::LocalFile => {
            let outcome = runs_service::copy_local_file_artifact(artifact, args.output)?;
            Ok((
                RunsOutput::ArtifactGet(RunsArtifactGetOutput {
                    command: "runs.artifact.get",
                    run_id: outcome.run_id,
                    artifact_id: outcome.artifact_id,
                    output_path: outcome.output_path.display().to_string(),
                    content_type: outcome.content_type,
                    size_bytes: outcome.size_bytes,
                    sha256: outcome.sha256,
                    artifact_ref: None,
                }),
                0,
            ))
        }
        runs_service::ArtifactStorage::Remote => remote_artifact::get(artifact, args.output),
        runs_service::ArtifactStorage::MetadataOnly => Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} was imported as metadata only; artifact bytes are not available in this bundle",
                artifact.id
            ),
            Some(artifact.id),
            None,
        )),
        runs_service::ArtifactStorage::Other => Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a downloadable file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id.clone()),
            None,
        )),
    }
}

pub(super) fn require_run(
    store: &ObservationStore,
    run_id: &str,
) -> homeboy::core::Result<RunRecord> {
    runs_service::require_run(store, run_id)
}

pub(super) fn run_detail(
    store: &ObservationStore,
    run: RunRecord,
) -> homeboy::core::Result<RunDetail> {
    let artifacts = runs_service::enrich_artifact_links(store.list_artifacts(&run.id)?);
    Ok(RunDetail {
        summary: run_summary(run.clone()),
        homeboy_version: run.homeboy_version,
        metadata: run.metadata_json,
        artifacts,
    })
}

pub(crate) fn run_summary(run: RunRecord) -> RunSummary {
    let status_note = reconcile::running_status_note(&run);
    RunSummary {
        id: run.id,
        kind: run.kind,
        status: run.status,
        started_at: run.started_at,
        finished_at: run.finished_at,
        component_id: run.component_id,
        rig_id: run.rig_id,
        git_sha: run.git_sha,
        command: run.command,
        cwd: run.cwd,
        status_note,
        artifact_index: None,
    }
}
