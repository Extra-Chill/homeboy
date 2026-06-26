use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use homeboy::core::observation::{ArtifactRecord, ObservationStore};
use homeboy::core::runners::{self as runner, RunnerExecOutput, RunnerKind};
use homeboy::core::stream_capture::StreamCaptureMetadata;
use homeboy::core::{server, Error};

use super::super::CmdResult;
use super::types::RUNNER_EXEC_SCRIPT_ENV;

#[allow(clippy::too_many_arguments)]
pub(super) fn exec(
    runner_id: &str,
    cwd: Option<String>,
    project_id: Option<String>,
    allow_diagnostic_ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    script_file: Option<String>,
    env: Vec<String>,
    dry_run: bool,
    run_id: Option<String>,
    artifact_outputs: Vec<String>,
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
    let has_declared_outputs = !artifact_outputs.is_empty() || !summary_outputs.is_empty();

    if !dry_run && has_declared_outputs && run_id.is_none() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "runner exec --artifact/--summary requires --run-id so evidence can be attached to a persisted run",
            None,
            None,
        ));
    }

    if dry_run {
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

    let (output, exit_code) = runner::exec(
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
            source_snapshot: None,
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
        },
    )?;
    if let Some(run_id) = validated_run_id.as_deref() {
        promote_runner_exec_artifacts(run_id, &output, &artifact_outputs)?;
        promote_runner_exec_summaries(run_id, &output, &summary_outputs)?;
    }
    Ok((output, exit_code))
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
            });
            let record_path = if let Some(runner) = runner.as_ref() {
                metadata["source"] = serde_json::json!("runner_path_attach");
                metadata["runner_id"] = serde_json::json!(runner.id.clone());
                metadata["runner_path"] = serde_json::json!(runner_path.display().to_string());
                copy_runner_exec_artifact_source(runner, &runner_path)?
            } else {
                runner_path.clone()
            };
            if record_path.is_dir() {
                store.record_directory_artifact_with_metadata(run_id, &kind, &record_path, metadata)
            } else {
                store.record_artifact_with_metadata(run_id, &kind, &record_path, metadata)
            }
        })
        .collect()
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
            metrics: None,
            capture: None,
            runner_result: None,
            handoff: None,
            diagnostics: Some(runner::RunnerExecDiagnostics {
                runner_workspace_root: runner.workspace_root,
                source_snapshot_remote_path: None,
                required_paths: require_paths,
                hints: vec!["dry run only; no runner command was executed".to_string()],
            }),
        },
        0,
    ))
}

fn display_path(path: std::path::PathBuf) -> String {
    path.display().to_string()
}
