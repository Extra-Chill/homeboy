//! Runner-exec artifact promotion + persistence.
//!
//! Boundary: the `runner exec` command runs the job via the core runner and
//! adapts the result for output; this module owns promoting declared runner-side
//! outputs into observation-store artifacts — resolving/downloading runner
//! sources, recording file/directory artifacts, deriving typed fuzz artifacts,
//! and building the promoted-output/structured-summary views. It operates on
//! core runner types, not command types.

use std::fs;
use std::path::{Path, PathBuf};

use crate::runners::{
    self as runner, RunnerExecOutput, RunnerExecPromotedOutput, RunnerExecStructuredSummary,
    RunnerKind,
};
use homeboy_core::engine::shell;
use homeboy_core::observation::{ArtifactRecord, ObservationStore};
use homeboy_core::performance_hotspots::PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA;
use homeboy_core::{server, Error};
use homeboy_fuzz::{
    parse_fuzz_observation_set_value, rank_fuzz_observation_set_hotspots, FUZZ_HOTSPOT_SET_SCHEMA,
    FUZZ_OBSERVATION_SET_SCHEMA, FUZZ_RESULT_ENVELOPE_SCHEMA,
};

/// Promote declared file outputs into observation-store artifacts.
pub fn promote_runner_exec_artifacts(
    run_id: &str,
    output: &RunnerExecOutput,
    artifact_outputs: &[String],
) -> homeboy_core::Result<Vec<ArtifactRecord>> {
    promote_runner_exec_outputs(
        run_id,
        output,
        artifact_outputs,
        RunnerExecEvidenceRole::Artifact,
    )
}

/// Promote every child of each declared artifact directory into artifacts.
pub fn promote_runner_exec_artifact_dirs(
    run_id: &str,
    output: &RunnerExecOutput,
    artifact_dir_outputs: &[String],
) -> homeboy_core::Result<Vec<ArtifactRecord>> {
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

/// Promote declared summary outputs into observation-store artifacts.
pub fn promote_runner_exec_summaries(
    run_id: &str,
    output: &RunnerExecOutput,
    summary_outputs: &[String],
) -> homeboy_core::Result<Vec<ArtifactRecord>> {
    promote_runner_exec_outputs(
        run_id,
        output,
        summary_outputs,
        RunnerExecEvidenceRole::Summary,
    )
}

/// Build the promoted-output view of a recorded artifact, if it carries the
/// runner-exec promotion metadata.
pub fn promoted_output(
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

/// Build a structured-summary view of a recorded JSON summary artifact.
pub fn runner_exec_structured_summary(
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

fn promote_runner_exec_outputs(
    run_id: &str,
    output: &RunnerExecOutput,
    output_paths: &[String],
    role: RunnerExecEvidenceRole,
) -> homeboy_core::Result<Vec<ArtifactRecord>> {
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
        .collect::<homeboy_core::Result<Vec<_>>>()
        .map(|records| records.into_iter().flatten().collect())
}

fn record_runner_exec_output(
    store: &ObservationStore,
    run_id: &str,
    kind: &str,
    record_path: &Path,
    metadata: serde_json::Value,
) -> homeboy_core::Result<Vec<ArtifactRecord>> {
    if record_path.is_dir() {
        return store
            .record_directory_artifact_with_metadata(run_id, kind, record_path, metadata)
            .map(|record| vec![record]);
    }

    let (kind, metadata) = typed_artifact_metadata(kind, record_path, metadata);
    let record = store.record_artifact_with_metadata(run_id, &kind, record_path, metadata)?;
    let mut records = vec![record.clone()];
    records.extend(persist_derived_fuzz_artifacts(store, run_id, &record)?);
    Ok(records)
}

fn typed_artifact_metadata(
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
        PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA => Some("performance_hotspots_summary"),
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
) -> homeboy_core::Result<Vec<ArtifactRecord>> {
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
) -> homeboy_core::Result<ArtifactRecord> {
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

fn copy_runner_exec_artifact_source(
    runner: &runner::Runner,
    path: &Path,
) -> homeboy_core::Result<PathBuf> {
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
) -> homeboy_core::Result<RunnerExecArtifactSourceType> {
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

fn copy_dir_all(from: &Path, to: &Path) -> homeboy_core::Result<()> {
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
) -> homeboy_core::Result<()> {
    let roots = allowed_runner_exec_artifact_roots(runner);
    match homeboy_core::paths::authorize_remote_artifact_path(
        path,
        &roots,
        homeboy_core::paths::RemotePathRootContainment::NativePath,
    ) {
        Ok(()) => Ok(()),
        Err(homeboy_core::paths::RemotePathAuthorizationError::NotAbsolute) => {
            Err(Error::validation_invalid_argument(
                "path",
                "runner exec artifact path must resolve to an absolute runner-side path",
                Some(path.display().to_string()),
                None,
            ))
        }
        Err(homeboy_core::paths::RemotePathAuthorizationError::ContainsParentDir) => {
            Err(Error::validation_invalid_argument(
                "path",
                "runner exec artifact path must not contain parent directory components",
                Some(path.display().to_string()),
                None,
            ))
        }
        Err(homeboy_core::paths::RemotePathAuthorizationError::OutsideAllowedRoots) => {
            Err(Error::validation_invalid_argument(
                "path",
                "runner exec artifact path must be under an allowed runner workspace/output root",
                Some(path.display().to_string()),
                Some(vec![format!("Allowed roots: {}", roots.join(", "))]),
            ))
        }
    }
}

fn allowed_runner_exec_artifact_roots(runner: &runner::Runner) -> Vec<String> {
    let mut roots = Vec::new();
    roots.extend(runner.workspace_root.iter().cloned());
    roots.extend(runner.policy.workspace_roots.iter().cloned());
    roots.extend(runner.env.get("HOMEBOY_ARTIFACT_ROOT").cloned());
    roots.retain(|root| Path::new(root).is_absolute());
    roots.sort();
    roots.dedup();
    roots
}

fn runner_exec_attach_download_path(runner_id: &str, path: &Path) -> homeboy_core::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact");
    Ok(homeboy_core::artifact_root()?
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
