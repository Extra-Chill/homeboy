//! Runner artifact-attach source acquisition.
//!
//! Boundary: the `runs artifact attach` command validates arguments and records
//! the resulting artifact in the observation store; this module owns the
//! filesystem/transfer orchestration — resolving the runner-side artifact type,
//! downloading remote sources into a temp location, and cleaning up temporary
//! downloads afterward. No CLI types cross this boundary.

use std::fs;
use std::path::{Path, PathBuf};

use crate::transport::RunnerPathKind;
use crate::{Runner, RunnerKind};
use homeboy_core::Error;

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
/// location under an explicitly supplied artifact root.
///
/// The caller supplies the root because the downloaded path is handed straight
/// to `ObservationStore::record_artifact*`. Resolving it here, independently of
/// the store the record lands in, meant the bytes and the row indexing them
/// agreed only by coincidence (#7505).
///
/// Only the download destination is rooted, because only the download
/// destination is Homeboy state: the artifact type probe and the transfer both
/// address the *runner* filesystem through an SSH client. The one remaining
/// ambient reach on this path is `server::load`, which resolves the config root
/// inside `homeboy-core` and is out of this crate's reach (#7505).
pub fn copy_runner_artifact_source(
    artifact_root: &Path,
    runner: &Runner,
    path: &str,
) -> homeboy_core::Result<RunnerAttachSource> {
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
            let transfer = crate::RunnerFileTransfer::for_runner(runner)?;
            let temp_path = attach_download_path_in_roots(artifact_root, &runner.id, path);
            if let Some(parent) = temp_path.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some(format!("create {}", parent.display())),
                    )
                })?;
            }
            let artifact_type = match transfer.path_kind(path) {
                Some(RunnerPathKind::Directory) => RunnerAttachArtifactType::Directory,
                Some(RunnerPathKind::File) => RunnerAttachArtifactType::File,
                None => {
                    return Err(Error::validation_invalid_argument(
                        "path",
                        "runner artifact path is not a file or directory",
                        Some(path.to_string()),
                        None,
                    ))
                }
            };
            let temp_path_string = temp_path.display().to_string();
            match artifact_type {
                RunnerAttachArtifactType::Directory => {
                    transfer.download_recursive(path, &temp_path_string)?
                }
                RunnerAttachArtifactType::File => {
                    transfer.download_file(path, &temp_path_string)?
                }
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

fn attach_download_path_in_roots(artifact_root: &Path, runner_id: &str, path: &str) -> PathBuf {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("artifact");
    artifact_root
        .join("runner-attach")
        .join(runner_id)
        .join(format!("{}-{file_name}", uuid::Uuid::new_v4()))
}
