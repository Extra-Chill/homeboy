//! Runner artifact-attach source acquisition.
//!
//! Boundary: the `runs artifact attach` command validates arguments and records
//! the resulting artifact in the observation store; this module owns the
//! filesystem/transfer orchestration — resolving the runner-side artifact type,
//! downloading remote sources into a temp location, and cleaning up temporary
//! downloads afterward. No CLI types cross this boundary.

use std::fs;
use std::path::{Path, PathBuf};

use crate::engine::shell;
use crate::runners::{Runner, RunnerKind};
use crate::{server, Error};

/// Whether an attached runner artifact is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerAttachArtifactType {
    File,
    Directory,
}

/// A resolved artifact source ready to record. `temporary` sources were
/// downloaded from a remote runner and should be cleaned up after recording.
pub struct RunnerAttachSource {
    pub path: PathBuf,
    pub artifact_type: RunnerAttachArtifactType,
    pub temporary: bool,
}

/// Resolve and, for SSH runners, download the runner-side artifact into a temp
/// location under the artifact root.
pub fn copy_runner_artifact_source(
    runner: &Runner,
    path: &str,
) -> crate::Result<RunnerAttachSource> {
    match runner.kind {
        RunnerKind::Local => {
            let source = PathBuf::from(path);
            let metadata = fs::metadata(&source).map_err(|err| {
                Error::internal_io(err.to_string(), Some(format!("read artifact {path}")))
            })?;
            let artifact_type = if metadata.is_file() {
                RunnerAttachArtifactType::File
            } else if metadata.is_dir() {
                RunnerAttachArtifactType::Directory
            } else {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "runner artifact attach supports files and directories",
                    Some(path.to_string()),
                    None,
                ));
            };
            Ok(RunnerAttachSource {
                path: source,
                artifact_type,
                temporary: false,
            })
        }
        RunnerKind::Ssh => {
            let server_id = runner.server_id.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runner",
                    "SSH runner is missing server_id",
                    Some(runner.id.clone()),
                    None,
                )
            })?;
            let temp_path = attach_download_path(&runner.id, path)?;
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
            let artifact_type = remote_runner_artifact_type(&client, path)?;
            if artifact_type == RunnerAttachArtifactType::Directory {
                let output = server::transfer::transfer(&server::transfer::TransferConfig {
                    source: format!("{server_id}:{path}"),
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
                            "failed to download runner artifact directory: {}",
                            output.0.error.unwrap_or_else(|| "scp failed".to_string())
                        ),
                        Some(path.to_string()),
                        None,
                    ));
                }
                return Ok(RunnerAttachSource {
                    path: temp_path,
                    artifact_type,
                    temporary: true,
                });
            }
            let output = client.download_file(path, &temp_path.display().to_string());
            if !output.success {
                return Err(Error::validation_invalid_argument(
                    "path",
                    format!(
                        "failed to download runner artifact: {}",
                        output.stderr.trim()
                    ),
                    Some(path.to_string()),
                    None,
                ));
            }
            Ok(RunnerAttachSource {
                path: temp_path,
                artifact_type,
                temporary: true,
            })
        }
    }
}

/// Remove a temporary attach source after it has been recorded. No-op for
/// non-temporary (local) sources.
pub fn cleanup_runner_attach_source(source: &RunnerAttachSource) {
    if !source.temporary {
        return;
    }
    match source.artifact_type {
        RunnerAttachArtifactType::File => {
            let _ = fs::remove_file(&source.path);
        }
        RunnerAttachArtifactType::Directory => {
            let _ = fs::remove_dir_all(&source.path);
        }
    }
}

fn remote_runner_artifact_type(
    client: &server::SshClient,
    path: &str,
) -> crate::Result<RunnerAttachArtifactType> {
    let quoted = shell::quote_path(path);
    if client.execute(&format!("test -d {quoted}")).success {
        return Ok(RunnerAttachArtifactType::Directory);
    }
    if client.execute(&format!("test -f {quoted}")).success {
        return Ok(RunnerAttachArtifactType::File);
    }
    Err(Error::validation_invalid_argument(
        "path",
        "runner artifact path is not a file or directory",
        Some(path.to_string()),
        None,
    ))
}

fn attach_download_path(runner_id: &str, path: &str) -> crate::Result<PathBuf> {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact");
    Ok(crate::artifact_root()?
        .join("runner-attach")
        .join(runner_id)
        .join(format!("{}-{file_name}", uuid::Uuid::new_v4())))
}
