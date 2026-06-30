use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use homeboy::core::engine::shell;
use homeboy::core::fuzz::{
    parse_fuzz_observation_set_value, rank_fuzz_observation_set_hotspots, FUZZ_HOTSPOT_SET_SCHEMA,
    FUZZ_OBSERVATION_SET_SCHEMA, FUZZ_RESULT_ENVELOPE_SCHEMA,
};
use homeboy::core::observation::{ArtifactRecord, ObservationStore};
use homeboy::core::runners::{
    self as runner, RunnerExecOutput, RunnerExecPromotedOutput, RunnerExecStructuredSummary,
    RunnerKind,
};
use homeboy::core::source_snapshot::SourceSnapshot;
use homeboy::core::stream_capture::StreamCaptureMetadata;
use homeboy::core::{server, Error};

use super::super::CmdResult;
use super::types::RUNNER_EXEC_SCRIPT_ENV;

#[allow(clippy::too_many_arguments)]
pub(super) fn exec(
    runner_id: &str,
    cwd: Option<String>,
    sync_workspace: Option<String>,
    project_id: Option<String>,
    allow_diagnostic_ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    script_file: Option<String>,
    env: Vec<String>,
    dry_run: bool,
    run_id: Option<String>,
    artifact_outputs: Vec<String>,
    artifact_dir_outputs: Vec<String>,
    summary_outputs: Vec<String>,
    command: Vec<String>,
) -> CmdResult<RunnerExecOutput> {
    let script = script_file
        .as_deref()
        .map(read_runner_exec_script)
        .transpose()?;
    let prepared_command = prepare_runner_exec_command(script.as_ref(), command)?;
    let env = prepare_runner_exec_env(env, script.as_deref())?;
    let required_commands = prepared_command.first().cloned().into_iter().collect();
    let has_declared_outputs = !artifact_outputs.is_empty()
        || !artifact_dir_outputs.is_empty()
        || !summary_outputs.is_empty();

    if !dry_run && has_declared_outputs && run_id.is_none() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "runner exec --artifact/--artifact-dir/--summary requires --run-id so evidence can be attached to a persisted run",
            None,
            None,
        ));
    }

    if dry_run {
        let (cwd, _) = exec_workspace_context(runner_id, cwd, sync_workspace, true)?;
        return runner_exec_dry_run(
            runner_id,
            cwd,
            allow_diagnostic_ssh,
            require_paths,
            prepared_command,
            script.unwrap_or_default(),
        );
    }

    let validated_run_id = validate_runner_exec_run_id(run_id)?;
    let (cwd, source_snapshot) = exec_workspace_context(runner_id, cwd, sync_workspace, false)?;

    let (mut output, exit_code) = runner::exec(
        runner_id,
        runner::RunnerExecOptions {
            cwd,
            project_id,
            allow_diagnostic_ssh,
            command: prepared_command,
            env,
            secret_env_names: script_file
                .is_some()
                .then(|| RUNNER_EXEC_SCRIPT_ENV.to_string())
                .into_iter()
                .collect(),
            capture_patch,
            raw_exec: true,
            source_snapshot,
            capability_preflight: Some(runner::RunnerCapabilityPreflight {
                command: "runner.exec".to_string(),
                required_commands,
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            require_paths,
            runner_workload: None,
            run_id: validated_run_id.clone(),
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;
    if let Some(run_id) = validated_run_id.as_deref() {
        let artifacts = promote_runner_exec_artifacts(run_id, &output, &artifact_outputs)?;
        let promoted_artifacts = artifacts
            .iter()
            .filter_map(|record| promoted_output(&output, record))
            .collect::<Vec<_>>();
        let artifact_dir_records =
            promote_runner_exec_artifact_dirs(run_id, &output, &artifact_dir_outputs)?;
        let promoted_artifact_dir_records = artifact_dir_records
            .iter()
            .filter_map(|record| promoted_output(&output, record))
            .collect::<Vec<_>>();
        let summaries = promote_runner_exec_summaries(run_id, &output, &summary_outputs)?;
        let structured_summaries = summaries
            .iter()
            .filter_map(|summary| runner_exec_structured_summary(&output, summary))
            .collect::<Vec<_>>();
        let promoted_summaries = summaries
            .iter()
            .filter_map(|record| promoted_output(&output, record))
            .collect::<Vec<_>>();
        output.promoted_outputs.extend(promoted_artifacts);
        output
            .promoted_outputs
            .extend(promoted_artifact_dir_records);
        output.structured_summaries.extend(structured_summaries);
        output.promoted_outputs.extend(promoted_summaries);
    }
    Ok((output, exit_code))
}

pub(super) fn exec_workspace_context(
    runner_id: &str,
    cwd: Option<String>,
    sync_workspace: Option<String>,
    dry_run: bool,
) -> homeboy::core::Result<(Option<String>, Option<SourceSnapshot>)> {
    let Some(local_path) = sync_workspace else {
        return Ok((cwd, None));
    };

    if cwd.is_some() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "--cwd and --sync-workspace are mutually exclusive; --sync-workspace executes from the materialized runner path",
            None,
            Some(vec![
                "Use --sync-workspace <local-worktree> when the command should run from that worktree snapshot.".to_string(),
                "Use --cwd <runner-path> when the runner-side path already exists.".to_string(),
            ]),
        ));
    }

    if dry_run {
        return Ok((None, None));
    }

    let (synced, _) = runner::sync_workspace(
        runner_id,
        runner::RunnerWorkspaceSyncOptions {
            path: local_path,
            mode: runner::RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?;
    let source_snapshot = SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&synced.remote_path),
        synced.sync_mode.label(),
    );

    Ok((Some(synced.remote_path), Some(source_snapshot)))
}

fn validate_runner_exec_run_id(run_id: Option<String>) -> homeboy::core::Result<Option<String>> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let trimmed = run_id.trim();
    if trimmed.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "runner exec --run-id must not be empty",
            Some(run_id),
            None,
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Maximum number of bytes retained when reading a runner exec script into
/// memory. The script is executed verbatim, so an oversized script is rejected
/// rather than silently truncated; the cap bounds the retained bytes and the
/// truncation metadata records when the source exceeded the limit (#5238).
pub(super) const RUNNER_EXEC_SCRIPT_LIMIT_BYTES: usize = 1024 * 1024;

/// Read a stream into memory with an explicit retained-byte bound, returning the
/// retained bytes plus truncation metadata. Reads one byte past the limit so an
/// overflow is detectable without retaining the entire (potentially unbounded)
/// source.
pub(super) fn read_bounded(
    mut reader: impl Read,
    limit_bytes: usize,
) -> io::Result<(Vec<u8>, StreamCaptureMetadata)> {
    let mut retained = Vec::new();
    let read = reader
        .by_ref()
        .take((limit_bytes as u64).saturating_add(1))
        .read_to_end(&mut retained)?;
    let truncated = read > limit_bytes;
    if truncated {
        retained.truncate(limit_bytes);
    }
    let metadata = StreamCaptureMetadata {
        limit_bytes,
        seen_bytes: read,
        retained_bytes: retained.len(),
        truncated,
    };
    Ok((retained, metadata))
}

pub(super) fn read_runner_exec_script(path: &str) -> homeboy::core::Result<String> {
    let (bytes, capture) = if path == "-" {
        read_bounded(io::stdin().lock(), RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some("read runner exec script from stdin".to_string()),
            )
        })?
    } else {
        let file = fs::File::open(path).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?;
        read_bounded(file, RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?
    };

    if capture.truncated {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "script_file",
            format!(
                "runner exec script exceeds the {} byte limit (retained {} of {}+ bytes); refusing to execute a truncated script",
                capture.limit_bytes, capture.retained_bytes, capture.seen_bytes
            ),
            Some(path.to_string()),
            None,
        ));
    }

    String::from_utf8(bytes).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("decode runner exec script {path}")),
        )
    })
}

pub(super) fn prepare_runner_exec_command(
    script: Option<&String>,
    command: Vec<String>,
) -> homeboy::core::Result<Vec<String>> {
    match (script.is_some(), command.is_empty()) {
        (true, false) => Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "runner exec accepts either --script-file or a command argv, not both",
            None,
            None,
        )),
        (false, true) => Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after -- or --script-file <path>",
            None,
            None,
        )),
        (true, true) => Ok(vec![
            "bash".to_string(),
            "-c".to_string(),
            "printf '%s' \"$HOMEBOY_RUNNER_EXEC_SCRIPT\" | bash -s".to_string(),
        ]),
        (false, false) => Ok(command),
    }
}

pub(super) fn prepare_runner_exec_env(
    env: Vec<String>,
    script: Option<&str>,
) -> homeboy::core::Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    for assignment in env {
        let Some((key, value)) = assignment.split_once('=') else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "runner exec --env expects KEY=VALUE",
                Some(assignment),
                None,
            ));
        };
        if key.is_empty() || key.contains('=') || key.chars().any(|c| c.is_whitespace()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "runner exec --env key must be a non-empty shell environment name",
                Some(key.to_string()),
                None,
            ));
        }
        values.insert(key.to_string(), value.to_string());
    }
    if let Some(script) = script {
        values.insert(RUNNER_EXEC_SCRIPT_ENV.to_string(), script.to_string());
    }
    Ok(values)
}

pub(super) fn promote_runner_exec_artifacts(
    run_id: &str,
    output: &RunnerExecOutput,
    artifact_outputs: &[String],
) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    promote_runner_exec_outputs(
        run_id,
        output,
        artifact_outputs,
        RunnerExecEvidenceRole::Artifact,
    )
}

pub(super) fn promote_runner_exec_artifact_dirs(
    run_id: &str,
    output: &RunnerExecOutput,
    artifact_dir_outputs: &[String],
) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    if artifact_dir_outputs.is_empty() {
        return Ok(Vec::new());
    }

    let store = ObservationStore::open_initialized()?;
    let runner = match output.mode {
        runner::RunnerExecMode::Local => None,
        runner::RunnerExecMode::Daemon
        | runner::RunnerExecMode::ReverseBroker
        | runner::RunnerExecMode::DiagnosticSsh => Some(runner::load(&output.runner_id)?),
    };
    let mut records = Vec::new();
    for declared_dir in artifact_dir_outputs {
        let runner_dir = resolve_runner_exec_artifact_path(&output.remote_cwd, declared_dir);
        let record_dir = if let Some(runner) = runner.as_ref() {
            copy_runner_exec_artifact_source(runner, &runner_dir)?
        } else {
            runner_dir.clone()
        };
        if !record_dir.is_dir() {
            return Err(Error::validation_invalid_argument(
                "artifact_dir",
                format!(
                    "runner exec artifact directory is not a directory: {}",
                    runner_dir.display()
                ),
                Some(declared_dir.to_string()),
                None,
            ));
        }
        let mut children = fs::read_dir(&record_dir)
            .map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("read {}", record_dir.display())),
                )
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("read {}", record_dir.display())),
                )
            })?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let record_path = child.path();
            let metadata = child.metadata().map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("stat {}", record_path.display())),
                )
            })?;
            if !metadata.is_file() && !metadata.is_dir() {
                continue;
            }
            let name = child.file_name().to_string_lossy().to_string();
            let declared_path = Path::new(declared_dir).join(&name).display().to_string();
            let runner_path = runner_dir.join(&name);
            let mut artifact_metadata = serde_json::json!({
                "artifact_dir": declared_dir,
                "declared_path": declared_path,
                "evidence_role": RunnerExecEvidenceRole::Artifact.as_str(),
                "promoted_by": "runner.exec",
                "runner_path": runner_path.display().to_string(),
            });
            if let Some(runner) = runner.as_ref() {
                artifact_metadata["source"] = serde_json::json!("runner_path_attach");
                artifact_metadata["runner_id"] = serde_json::json!(runner.id.clone());
            }
            records.extend(record_runner_exec_output(
                &store,
                run_id,
                &runner_exec_artifact_kind(&name),
                &record_path,
                artifact_metadata,
            )?);
        }
    }
    Ok(records)
}

pub(super) fn promote_runner_exec_summaries(
    run_id: &str,
    output: &RunnerExecOutput,
    summary_outputs: &[String],
) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    promote_runner_exec_outputs(
        run_id,
        output,
        summary_outputs,
        RunnerExecEvidenceRole::Summary,
    )
}

#[derive(Clone, Copy)]
enum RunnerExecEvidenceRole {
    Artifact,
    Summary,
}

impl RunnerExecEvidenceRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::Summary => "summary",
        }
    }

    fn kind(self, declared: &str) -> String {
        match self {
            Self::Artifact => runner_exec_artifact_kind(declared),
            Self::Summary => runner_exec_summary_kind(declared),
        }
    }
}

fn promote_runner_exec_outputs(
    run_id: &str,
    output: &RunnerExecOutput,
    output_paths: &[String],
    role: RunnerExecEvidenceRole,
) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    if output_paths.is_empty() {
        return Ok(Vec::new());
    }

    let store = ObservationStore::open_initialized()?;
    let runner = match output.mode {
        runner::RunnerExecMode::Local => None,
        runner::RunnerExecMode::Daemon
        | runner::RunnerExecMode::ReverseBroker
        | runner::RunnerExecMode::DiagnosticSsh => Some(runner::load(&output.runner_id)?),
    };
    output_paths
        .iter()
        .map(|declared| {
            let runner_path = resolve_runner_exec_artifact_path(&output.remote_cwd, declared);
            let kind = role.kind(declared);
            let mut metadata = serde_json::json!({
                "declared_path": declared,
                "evidence_role": role.as_str(),
                "promoted_by": "runner.exec",
                "runner_path": runner_path.display().to_string(),
            });
            let record_path = if let Some(runner) = runner.as_ref() {
                metadata["source"] = serde_json::json!("runner_path_attach");
                metadata["runner_id"] = serde_json::json!(runner.id.clone());
                metadata["runner_path"] = serde_json::json!(runner_path.display().to_string());
                copy_runner_exec_artifact_source(runner, &runner_path)?
            } else {
                runner_path.clone()
            };
            record_runner_exec_output(&store, run_id, &kind, &record_path, metadata)
        })
        .collect::<homeboy::core::Result<Vec<_>>>()
        .map(|records| records.into_iter().flatten().collect())
}

fn record_runner_exec_output(
    store: &ObservationStore,
    run_id: &str,
    kind: &str,
    record_path: &Path,
    metadata: serde_json::Value,
) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    if record_path.is_dir() {
        return store
            .record_directory_artifact_with_metadata(run_id, kind, record_path, metadata)
            .map(|record| vec![record]);
    }

    let (kind, metadata) = fuzz_typed_artifact_metadata(kind, record_path, metadata);
    let record = store.record_artifact_with_metadata(run_id, &kind, record_path, metadata)?;
    let mut records = vec![record.clone()];
    records.extend(persist_derived_fuzz_artifacts(store, run_id, &record)?);
    Ok(records)
}

fn fuzz_typed_artifact_metadata(
    kind: &str,
    record_path: &Path,
    mut metadata: serde_json::Value,
) -> (String, serde_json::Value) {
    let Some(json) = read_json_file(record_path) else {
        return (kind.to_string(), metadata);
    };
    let Some(schema) = json.get("schema").and_then(serde_json::Value::as_str) else {
        return (kind.to_string(), metadata);
    };

    let canonical_kind = match schema {
        FUZZ_RESULT_ENVELOPE_SCHEMA => Some("fuzz_result_envelope"),
        FUZZ_OBSERVATION_SET_SCHEMA => Some("fuzz_observation_set"),
        FUZZ_HOTSPOT_SET_SCHEMA => Some("fuzz_hotspot_set"),
        _ => None,
    };
    let Some(canonical_kind) = canonical_kind else {
        return (kind.to_string(), metadata);
    };

    metadata["schema"] = serde_json::json!(schema);
    metadata["typed_artifact_kind"] = serde_json::json!(canonical_kind);
    metadata["promoted_kind"] = serde_json::json!(kind);
    (canonical_kind.to_string(), metadata)
}

fn persist_derived_fuzz_artifacts(
    store: &ObservationStore,
    run_id: &str,
    source_record: &ArtifactRecord,
) -> homeboy::core::Result<Vec<ArtifactRecord>> {
    if source_record.artifact_type != "file" || source_record.kind != "fuzz_result_envelope" {
        return Ok(Vec::new());
    }

    let Some(json) = read_json_file(Path::new(&source_record.path)) else {
        return Ok(Vec::new());
    };
    let Some(observation_set) = parse_fuzz_observation_set_value(&json) else {
        return Ok(Vec::new());
    };

    let mut records = Vec::new();
    let observation_record = persist_json_artifact(
        store,
        run_id,
        "fuzz_observation_set",
        &observation_set,
        serde_json::json!({
            "schema": FUZZ_OBSERVATION_SET_SCHEMA,
            "typed_artifact_kind": "fuzz_observation_set",
            "derived_from_artifact_id": source_record.id,
            "derived_from_artifact_kind": source_record.kind,
        }),
    )?;
    records.push(observation_record);

    let hotspot_set = rank_fuzz_observation_set_hotspots(&observation_set);
    let hotspot_record = persist_json_artifact(
        store,
        run_id,
        "fuzz_hotspot_set",
        &hotspot_set,
        serde_json::json!({
            "schema": FUZZ_HOTSPOT_SET_SCHEMA,
            "typed_artifact_kind": "fuzz_hotspot_set",
            "derived_from_artifact_id": source_record.id,
            "derived_from_artifact_kind": source_record.kind,
            "derived_from_observation_set_id": observation_set.id,
        }),
    )?;
    records.push(hotspot_record);
    Ok(records)
}

fn persist_json_artifact(
    store: &ObservationStore,
    run_id: &str,
    kind: &str,
    value: &impl serde::Serialize,
    metadata: serde_json::Value,
) -> homeboy::core::Result<ArtifactRecord> {
    let file = tempfile::NamedTempFile::new().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("create derived {kind} artifact")),
        )
    })?;
    serde_json::to_writer_pretty(&file, value).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("write derived {kind} artifact")),
        )
    })?;
    store.record_artifact_with_metadata(run_id, kind, file.path(), metadata)
}

fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    if !path.is_file() {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    serde_json::from_reader(file).ok()
}

fn promoted_output(
    output: &RunnerExecOutput,
    record: &ArtifactRecord,
) -> Option<RunnerExecPromotedOutput> {
    let role = record
        .metadata_json
        .get("evidence_role")?
        .as_str()?
        .to_string();
    let declared_path = record
        .metadata_json
        .get("declared_path")?
        .as_str()?
        .to_string();
    let runner_path = record
        .metadata_json
        .get("runner_path")?
        .as_str()?
        .to_string();
    Some(RunnerExecPromotedOutput {
        role,
        run_id: record.run_id.clone(),
        runner_id: output.runner_id.clone(),
        command: output.argv.clone(),
        declared_path,
        runner_path,
        artifact_id: record.id.clone(),
        artifact_kind: record.kind.clone(),
        artifact_path: record.path.clone(),
    })
}

fn runner_exec_structured_summary(
    output: &RunnerExecOutput,
    record: &ArtifactRecord,
) -> Option<RunnerExecStructuredSummary> {
    if record.artifact_type != "file" || record.mime.as_deref() != Some("application/json") {
        return None;
    }
    let summary = fs::read_to_string(&record.path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())?;
    let promoted = promoted_output(output, record)?;
    Some(RunnerExecStructuredSummary {
        run_id: promoted.run_id,
        runner_id: promoted.runner_id,
        command: promoted.command,
        declared_path: promoted.declared_path,
        artifact_id: promoted.artifact_id,
        artifact_path: promoted.artifact_path,
        summary,
    })
}

fn copy_runner_exec_artifact_source(
    runner: &runner::Runner,
    path: &Path,
) -> homeboy::core::Result<PathBuf> {
    let path_string = path.display().to_string();
    match runner.kind {
        RunnerKind::Local => Ok(path.to_path_buf()),
        RunnerKind::Ssh => {
            validate_runner_exec_artifact_path(runner, path)?;
            let server_id = runner.server_id.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runner",
                    "SSH runner is missing server_id",
                    Some(runner.id.clone()),
                    None,
                )
            })?;
            let temp_path = runner_exec_attach_download_path(&runner.id, path)?;
            if let Some(parent) = temp_path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("create {}", parent.display())),
                    )
                })?;
            }
            let server = server::load(server_id)?;
            let client = server::SshClient::from_server(&server, server_id)?;
            if remote_runner_exec_artifact_type(&client, path)?
                == RunnerExecArtifactSourceType::Directory
            {
                if client.is_local {
                    copy_dir_all(path, &temp_path)?;
                } else {
                    let output = server::transfer::transfer(&server::transfer::TransferConfig {
                        source: format!("{server_id}:{path_string}"),
                        destination: temp_path.display().to_string(),
                        recursive: true,
                        compress: true,
                        dry_run: false,
                        exclude: Vec::new(),
                    })?;
                    if output.1 != 0 || !output.0.success {
                        return Err(Error::validation_invalid_argument(
                            "path",
                            format!(
                                "failed to download runner exec artifact directory: {}",
                                output.0.error.unwrap_or_else(|| "scp failed".to_string())
                            ),
                            Some(path_string),
                            None,
                        ));
                    }
                }
                return Ok(temp_path);
            }
            let output = client.download_file(&path_string, &temp_path.display().to_string());
            if !output.success {
                return Err(Error::validation_invalid_argument(
                    "path",
                    format!(
                        "failed to download runner exec artifact: {}",
                        output.stderr.trim()
                    ),
                    Some(path_string),
                    None,
                ));
            }
            Ok(temp_path)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunnerExecArtifactSourceType {
    File,
    Directory,
}

fn remote_runner_exec_artifact_type(
    client: &server::SshClient,
    path: &Path,
) -> homeboy::core::Result<RunnerExecArtifactSourceType> {
    let path_string = path.display().to_string();
    let quoted = shell::quote_path(&path_string);
    if client.execute(&format!("test -d {quoted}")).success {
        return Ok(RunnerExecArtifactSourceType::Directory);
    }
    if client.execute(&format!("test -f {quoted}")).success {
        return Ok(RunnerExecArtifactSourceType::File);
    }
    Err(Error::validation_invalid_argument(
        "path",
        "runner exec artifact path is not a file or directory",
        Some(path_string),
        None,
    ))
}

fn copy_dir_all(from: &Path, to: &Path) -> homeboy::core::Result<()> {
    fs::create_dir_all(to).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("create {}", to.display())))
    })?;
    for entry in fs::read_dir(from).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", from.display())))
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {}", from.display())))
        })?;
        let source = entry.path();
        let destination = to.join(entry.file_name());
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("stat {}", source.display())))
        })?;
        if metadata.is_dir() {
            copy_dir_all(&source, &destination)?;
        } else if metadata.is_file() {
            fs::copy(&source, &destination).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!(
                        "copy {} to {}",
                        source.display(),
                        destination.display()
                    )),
                )
            })?;
        }
    }
    Ok(())
}

fn validate_runner_exec_artifact_path(
    runner: &runner::Runner,
    path: &Path,
) -> homeboy::core::Result<()> {
    if !path.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "path",
            "runner exec artifact path must resolve to an absolute runner-side path",
            Some(path.display().to_string()),
            None,
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::validation_invalid_argument(
            "path",
            "runner exec artifact path must not contain parent directory components",
            Some(path.display().to_string()),
            None,
        ));
    }

    let roots = allowed_runner_exec_artifact_roots(runner);
    if roots.iter().any(|root| path_is_within_root(path, root)) {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "path",
        "runner exec artifact path must be under an allowed runner workspace/output root",
        Some(path.display().to_string()),
        Some(vec![format!(
            "Allowed roots: {}",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )]),
    ))
}

fn allowed_runner_exec_artifact_roots(runner: &runner::Runner) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.extend(runner.workspace_root.as_deref().map(PathBuf::from));
    roots.extend(runner.policy.workspace_roots.iter().map(PathBuf::from));
    roots.extend(runner.env.get("HOMEBOY_ARTIFACT_ROOT").map(PathBuf::from));
    roots.retain(|root| root.is_absolute());
    roots.sort();
    roots.dedup();
    roots
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn runner_exec_attach_download_path(
    runner_id: &str,
    path: &Path,
) -> homeboy::core::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact");
    Ok(homeboy::core::artifact_root()?
        .join("runner-exec-attach")
        .join(runner_id)
        .join(format!("{}-{file_name}", uuid::Uuid::new_v4())))
}

fn resolve_runner_exec_artifact_path(remote_cwd: &str, declared: &str) -> PathBuf {
    let path = Path::new(declared);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(remote_cwd).join(path)
    }
}

fn runner_exec_artifact_kind(declared: &str) -> String {
    let name = Path::new(declared)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runner_exec_artifact");
    sanitize_runner_exec_kind(name, "runner_exec_artifact")
}

fn runner_exec_summary_kind(declared: &str) -> String {
    let name = Path::new(declared)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("runner_exec_summary");
    let kind = sanitize_runner_exec_kind(name, "runner_exec_summary");
    if kind.ends_with("summary") {
        kind
    } else {
        format!("{kind}_summary")
    }
}

fn sanitize_runner_exec_kind(name: &str, fallback: &str) -> String {
    let kind: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if kind.is_empty() {
        fallback.to_string()
    } else {
        kind
    }
}

fn runner_exec_dry_run(
    runner_id: &str,
    cwd: Option<String>,
    allow_diagnostic_ssh: bool,
    require_paths: Vec<String>,
    command: Vec<String>,
    script: String,
) -> CmdResult<RunnerExecOutput> {
    let runner = runner::load(runner_id)?;
    let remote_cwd = cwd
        .or_else(|| runner.workspace_root.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(display_path)
                .unwrap_or_else(|_| ".".to_string())
        });
    let mode = if runner.kind == RunnerKind::Local {
        runner::RunnerExecMode::Local
    } else if allow_diagnostic_ssh {
        runner::RunnerExecMode::DiagnosticSsh
    } else {
        runner::RunnerExecMode::Daemon
    };

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id,
            dry_run: true,
            mode,
            argv: command,
            remote_cwd,
            exit_code: 0,
            stdout: script,
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
            diagnostics: Some(runner::RunnerExecDiagnostics {
                runner_workspace_root: runner.workspace_root,
                source_snapshot_remote_path: None,
                required_paths: require_paths,
                homeboy_binaries: None,
                hints: vec!["dry run only; no runner command was executed".to_string()],
            }),
        },
        0,
    ))
}

fn display_path(path: std::path::PathBuf) -> String {
    path.display().to_string()
}
