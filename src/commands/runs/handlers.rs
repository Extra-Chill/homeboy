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
    RunsArtifactPathGuide, RunsArtifactPullEntry, RunsArtifactPullSummary, RunsArtifactsArgs,
    RunsArtifactsOutput, RunsEnvKeyOutput, RunsEnvOutput, RunsEnvSourceLayerOutput, RunsEnvSummary,
    RunsFieldSelectionOutput, RunsListArgs, RunsListOutput, RunsOutput, RunsResumePlanOutput,
    RunsSelectedField, RunsShowOutput,
};
use super::{reconcile, remote, remote_artifact, CmdResult};

pub fn list_runs(args: RunsListArgs, command: &'static str) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.clone() {
        return remote::list_runner_runs(&runner_id, args, command);
    }

    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    // `--running` is shorthand for `--status running`; the two are mutually
    // exclusive at the CLI layer so this never overrides an explicit status.
    let status = if args.running {
        Some("running".to_string())
    } else {
        args.status
    };
    let status_filter = status.clone();
    let run_records = store.list_runs(RunListFilter {
        kind: args.kind,
        component_id: args.component_id,
        status,
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

#[cfg(test)]
pub fn artifacts(run_id: &str) -> CmdResult<RunsOutput> {
    artifacts_from_args(RunsArtifactsArgs {
        run_id: run_id.to_string(),
        runner: None,
        pull: false,
        pull_dir: None,
    })
}

pub fn artifacts_from_args(args: RunsArtifactsArgs) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.as_deref() {
        if args.pull {
            return Err(Error::validation_invalid_argument(
                "pull",
                "`--pull` operates on the local mirrored observation store; drop `--runner` to retrieve runner artifact bytes to the operator-local artifact root",
                Some(runner_id.to_string()),
                Some(vec![
                    format!("Run `homeboy runs artifacts {} --pull` (without --runner) to pull mirrored runner artifacts locally.", args.run_id),
                ]),
            ));
        }
        return remote::runner_artifacts(runner_id, &args.run_id);
    }

    let store = ObservationStore::open_initialized()?;
    let artifacts = runs_service::list_artifacts_for_run(&store, &args.run_id)?;
    let preview_entrypoints = artifacts
        .iter()
        .flat_map(homeboy::core::artifacts::html_preview_entrypoints)
        .collect();
    let findings = store.list_findings(FindingListFilter {
        run_id: Some(args.run_id.to_string()),
        tool: None,
        file: None,
        fingerprint: None,
        limit: Some(10_000),
    })?;
    let matrix_summary =
        homeboy::core::artifacts::summarize_matrix_artifacts(&args.run_id, &artifacts, &findings);
    let fuzz_result_envelopes = artifacts
        .iter()
        .filter_map(homeboy::core::fuzz::inspect_fuzz_result_envelope_artifact)
        .collect();
    let pull = if args.pull {
        Some(pull_artifacts_to_local(&artifacts, args.pull_dir.as_deref())?)
    } else {
        None
    };
    Ok((
        RunsOutput::Artifacts(RunsArtifactsOutput {
            command: "runs.artifacts",
            run_id: args.run_id.clone(),
            runner_id: None,
            path_guide: RunsArtifactPathGuide::for_listing(&args.run_id, None),
            artifacts,
            preview_entrypoints,
            matrix_summary,
            fuzz_result_envelopes,
            pull,
        }),
        0,
    ))
}

/// Best-effort retrieval of each artifact's bytes to the operator-local
/// artifact root so a completed run is self-contained.
///
/// Local-file (and locally-present directory) artifacts are already operator
/// readable and reported as `already_local`. Remote runner artifacts are
/// downloaded; metadata-only / url artifacts are skipped with a reason. A
/// single artifact's failure never aborts the pass — it is recorded so the
/// operator sees exactly which diagnostics are unreachable and why.
fn pull_artifacts_to_local(
    artifacts: &[homeboy::core::observation::ArtifactRecord],
    pull_dir: Option<&std::path::Path>,
) -> homeboy::core::Result<RunsArtifactPullSummary> {
    let pull_root = match pull_dir {
        Some(dir) => dir.display().to_string(),
        None => homeboy::core::artifact_root()?.display().to_string(),
    };
    let mut entries = Vec::with_capacity(artifacts.len());
    let (mut pulled_count, mut already_local_count, mut skipped_count, mut failed_count) =
        (0, 0, 0, 0);

    for artifact in artifacts {
        let entry = match runs_service::classify_artifact_storage(artifact) {
            runs_service::ArtifactStorage::LocalFile => {
                already_local_count += 1;
                RunsArtifactPullEntry {
                    artifact_id: artifact.id.clone(),
                    storage: "local_file",
                    status: "already_local",
                    output_path: Some(artifact.path.clone()),
                    size_bytes: artifact.size_bytes,
                    content_type: artifact.mime.clone(),
                    sha256: artifact.sha256.clone(),
                    error: None,
                }
            }
            runs_service::ArtifactStorage::Remote => {
                let output = pull_dir.map(|dir| dir.join(sanitize_artifact_filename(&artifact.id)));
                match runs_service::download_remote_artifact(artifact.clone(), output) {
                    Ok(outcome) => {
                        pulled_count += 1;
                        RunsArtifactPullEntry {
                            artifact_id: artifact.id.clone(),
                            storage: "remote",
                            status: "pulled",
                            output_path: Some(outcome.output_path.display().to_string()),
                            size_bytes: outcome.size_bytes,
                            content_type: outcome.content_type,
                            sha256: outcome.sha256,
                            error: None,
                        }
                    }
                    Err(err) => {
                        failed_count += 1;
                        RunsArtifactPullEntry {
                            artifact_id: artifact.id.clone(),
                            storage: "remote",
                            status: "failed",
                            output_path: None,
                            size_bytes: None,
                            content_type: None,
                            sha256: None,
                            error: Some(err.message),
                        }
                    }
                }
            }
            runs_service::ArtifactStorage::MetadataOnly => {
                skipped_count += 1;
                RunsArtifactPullEntry {
                    artifact_id: artifact.id.clone(),
                    storage: "metadata_only",
                    status: "skipped",
                    output_path: None,
                    size_bytes: None,
                    content_type: None,
                    sha256: None,
                    error: Some(
                        "artifact was imported as metadata only; bytes are not available"
                            .to_string(),
                    ),
                }
            }
            runs_service::ArtifactStorage::Other => {
                // A locally-present directory artifact is already self-contained.
                if artifact.artifact_type == "directory"
                    && std::path::Path::new(&artifact.path).is_dir()
                {
                    already_local_count += 1;
                    RunsArtifactPullEntry {
                        artifact_id: artifact.id.clone(),
                        storage: "other",
                        status: "already_local",
                        output_path: Some(artifact.path.clone()),
                        size_bytes: artifact.size_bytes,
                        content_type: artifact.mime.clone(),
                        sha256: artifact.sha256.clone(),
                        error: None,
                    }
                } else {
                    skipped_count += 1;
                    RunsArtifactPullEntry {
                        artifact_id: artifact.id.clone(),
                        storage: "other",
                        status: "skipped",
                        output_path: None,
                        size_bytes: None,
                        content_type: None,
                        sha256: None,
                        error: Some(format!(
                            "artifact type `{}` is not a pullable file",
                            artifact.artifact_type
                        )),
                    }
                }
            }
        };
        entries.push(entry);
    }

    Ok(RunsArtifactPullSummary {
        pull_root,
        pulled_count,
        already_local_count,
        skipped_count,
        failed_count,
        entries,
    })
}

/// Derive a filesystem-safe filename from an artifact id for `--pull-dir`
/// targets. Mirrors the conservative substitution used elsewhere for derived
/// artifact paths so unusual ids cannot escape the target directory.
fn sanitize_artifact_filename(artifact_id: &str) -> String {
    let sanitized = artifact_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches(['.', '_']).to_string();
    if trimmed.is_empty() {
        "artifact".to_string()
    } else {
        trimmed
    }
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
    let fields = args.field.clone();
    let (output, exit_code) = artifact_get_inner(args)?;
    if fields.is_empty() {
        return Ok((output, exit_code));
    }
    apply_field_selection(output, &fields)
}

fn artifact_get_inner(args: RunsArtifactGetArgs) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.clone() {
        return remote::runner_artifact_get(&runner_id, args);
    }

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
                    runner_id: None,
                    source_content_url: None,
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

/// Project `--field`/`-q` selectors over a `show` or `artifact get` result,
/// returning a compact [`RunsOutput::FieldSelection`] carrying only the
/// requested fields. Show selectors are rooted at the run detail; artifact-get
/// selectors at the artifact-get result. Unsupported variants are returned
/// unchanged so the selector never silently swallows other output.
pub(super) fn apply_field_selection(
    output: RunsOutput,
    fields: &[String],
) -> CmdResult<RunsOutput> {
    let value = serde_json::to_value(&output).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("serialize runs output for field selection".to_string()),
        )
    })?;
    let variant = value.get("variant").and_then(Value::as_str).unwrap_or_default();
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let (root, run_id, artifact_id) = match variant {
        "show" => {
            let run = payload.get("run").cloned().unwrap_or(Value::Null);
            let run_id = run
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (run, run_id, None)
        }
        "artifact_get" => {
            let run_id = payload
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let artifact_id = payload
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            (payload.clone(), run_id, artifact_id)
        }
        _ => return Ok((output, 0)),
    };

    let selected = super::common::select_fields(&root, fields)?
        .into_iter()
        .map(|(field, value)| RunsSelectedField { field, value })
        .collect();

    Ok((
        RunsOutput::FieldSelection(RunsFieldSelectionOutput {
            command: "runs.field",
            run_id,
            artifact_id,
            fields: selected,
        }),
        0,
    ))
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

#[cfg(test)]
mod pull_tests {
    use std::fs;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy::core::observation::{ArtifactRecord, NewRunRecord, ObservationStore};
    use homeboy::test_support::with_isolated_home;

    use super::*;

    fn artifact_root_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn metadata_only_record(run_id: &str, id: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: id.to_string(),
            run_id: run_id.to_string(),
            kind: "finding-packets".to_string(),
            artifact_type: "metadata-only".to_string(),
            path: format!("metadata://{id}"),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn sanitize_artifact_filename_is_path_safe() {
        assert_eq!(sanitize_artifact_filename("finding-packets.json"), "finding-packets.json");
        assert_eq!(sanitize_artifact_filename("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_artifact_filename("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_artifact_filename("..."), "artifact");
        assert_eq!(sanitize_artifact_filename(""), "artifact");
    }

    #[test]
    fn pull_reports_local_file_as_already_local() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let source = home.path().join("finding-packets.json");
            fs::write(&source, br#"{"findings":[]}"#).expect("source");
            let artifact = store
                .record_artifact(&run.id, "finding-packets", &source)
                .expect("artifact");

            let summary = pull_artifacts_to_local(&[artifact.clone()], None).expect("pull summary");

            assert_eq!(summary.already_local_count, 1);
            assert_eq!(summary.pulled_count, 0);
            assert_eq!(summary.failed_count, 0);
            assert_eq!(summary.entries.len(), 1);
            assert_eq!(summary.entries[0].status, "already_local");
            assert_eq!(summary.entries[0].storage, "local_file");
            assert_eq!(summary.entries[0].output_path.as_deref(), Some(artifact.path.as_str()));
        });
    }

    #[test]
    fn pull_skips_metadata_only_artifacts_with_reason() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let record = metadata_only_record("run-1", "finding-packets");
            let summary = pull_artifacts_to_local(&[record], None).expect("pull summary");

            assert_eq!(summary.skipped_count, 1);
            assert_eq!(summary.entries[0].status, "skipped");
            assert_eq!(summary.entries[0].storage, "metadata_only");
            assert!(summary.entries[0]
                .error
                .as_deref()
                .unwrap()
                .contains("metadata only"));
        });
    }

    #[test]
    fn pull_records_per_artifact_failure_without_aborting() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            // A remote runner-artifact ref for a runner that does not exist must
            // be reported as a failed entry, not panic or abort the pass.
            let remote = ArtifactRecord {
                id: "matrix-json".to_string(),
                run_id: "run-1".to_string(),
                kind: "matrix".to_string(),
                artifact_type: "remote_file".to_string(),
                path: "runner-artifact://does-not-exist/run-1/matrix-json".to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: None,
                size_bytes: None,
                mime: None,
                metadata_json: serde_json::json!({}),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let local_source = home.path().join("summary.json");
            fs::write(&local_source, b"{}").expect("source");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let local = store
                .record_artifact(&run.id, "summary", &local_source)
                .expect("artifact");

            let summary =
                pull_artifacts_to_local(&[remote, local], None).expect("pull summary");

            assert_eq!(summary.entries.len(), 2);
            assert_eq!(summary.failed_count, 1);
            assert_eq!(summary.already_local_count, 1);
            let remote_entry = summary
                .entries
                .iter()
                .find(|entry| entry.artifact_id == "matrix-json")
                .expect("remote entry");
            assert_eq!(remote_entry.status, "failed");
            assert!(remote_entry.error.is_some());
        });
    }

    #[test]
    fn artifacts_from_args_pull_with_runner_is_rejected() {
        let result = artifacts_from_args(RunsArtifactsArgs {
            run_id: "run-1".to_string(),
            runner: Some("lab".to_string()),
            pull: true,
            pull_dir: None,
        });
        let Err(err) = result else {
            panic!("--pull with --runner should fail");
        };
        assert!(err.to_string().contains("local mirrored observation store"));
    }
}
