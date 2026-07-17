use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agent_task::{AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskOutcome};
use homeboy_core::{Error, Result};

/// Identity captured after Homeboy creates the executor root and before the
/// provider starts. The root itself must never be a symlink.
#[derive(Debug, Clone)]
pub(crate) struct ExecutorArtifactRootIdentity {
    path: PathBuf,
    finalized_path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume_serial_number: u64,
    #[cfg(windows)]
    file_id: [u8; 16],
}

impl ExecutorArtifactRootIdentity {
    pub(crate) fn capture(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("inspect executor artifact root {}", path.display())),
            )
        })?;
        if is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
            return Err(Error::validation_invalid_argument(
                "artifacts_path",
                "Homeboy executor artifact root must be a real directory",
                Some(path.display().to_string()),
                None,
            ));
        }
        let path = path.canonicalize().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "canonicalize executor artifact root {}",
                    path.display()
                )),
            )
        })?;
        // Finalized evidence belongs to Homeboy's canonical artifact store, not
        // beside the provider-writable executor directory.
        let finalized_path = homeboy_core::paths::artifact_root()?
            .join("executor-finalized")
            .join(uuid::Uuid::new_v4().to_string());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                path,
                finalized_path,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            let identity = windows_file_identity_from_path(&path, true).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("open executor artifact root {}", path.display())),
                )
            })?;
            Ok(Self {
                path,
                finalized_path,
                volume_serial_number: identity.volume_serial_number,
                file_id: identity.file_id,
            })
        }
        #[cfg(not(unix))]
        #[cfg(not(windows))]
        Ok(Self {
            path,
            finalized_path,
        })
    }

    fn verify(&self) -> Result<()> {
        let current = fs::symlink_metadata(&self.path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "inspect executor artifact root {}",
                    self.path.display()
                )),
            )
        })?;
        if is_link_or_reparse_point(&current) || !current.is_dir() {
            return Err(Error::validation_invalid_argument(
                "artifacts_path",
                "Homeboy executor artifact root changed during provider execution",
                Some(self.path.display().to_string()),
                None,
            ));
        }
        #[cfg(unix)]
        if {
            use std::os::unix::fs::MetadataExt;
            current.dev() != self.device || current.ino() != self.inode
        } {
            return Err(Error::validation_invalid_argument(
                "artifacts_path",
                "Homeboy executor artifact root changed during provider execution",
                Some(self.path.display().to_string()),
                None,
            ));
        }
        #[cfg(windows)]
        {
            let identity = windows_file_identity_from_path(&self.path, true).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "open executor artifact root {}",
                        self.path.display()
                    )),
                )
            })?;
            if identity.volume_serial_number != self.volume_serial_number
                || identity.file_id != self.file_id
            {
                return Err(Error::validation_invalid_argument(
                    "artifacts_path",
                    "Homeboy executor artifact root changed during provider execution",
                    Some(self.path.display().to_string()),
                    None,
                ));
            }
        }
        Ok(())
    }
}

/// Stages regular files into a Homeboy-owned finalized directory. The copied
/// bytes, not provider declarations or source metadata, define recorded size,
/// hash, and extension-based MIME. MIME semantic validation is intentionally
/// outside this transport boundary.
pub(crate) fn finalize_provider_file_artifacts(
    outcome: &mut AgentTaskOutcome,
    root: &ExecutorArtifactRootIdentity,
) -> Result<()> {
    #[cfg(not(any(unix, windows)))]
    {
        let mut diagnostics = Vec::new();
        for artifact in &mut outcome.artifacts {
            if artifact.path.is_some() {
                diagnostics.push(mark_review_only_for_unsupported_platform(artifact));
            }
        }
        outcome.diagnostics.extend(diagnostics);
        return Ok(());
    }

    #[cfg(any(unix, windows))]
    {
        root.verify()?;
        for artifact in &mut outcome.artifacts {
            let Some(declared) = artifact.path.clone() else {
                continue;
            };
            match staged_file(root, artifact, &declared) {
                Ok((path, size, sha256, mime)) => {
                    artifact.path = Some(path.display().to_string());
                    artifact.size_bytes = Some(size);
                    artifact.sha256 = Some(sha256);
                    artifact.mime = mime;
                    ensure_metadata(artifact)
                        .insert("executor_artifact_finalized".to_string(), json!(true));
                    ensure_metadata(artifact)
                        .insert("mime_inference".to_string(), json!("extension_only"));
                }
                Err(error) if is_legacy_outside_root(&error) => {
                    artifact.size_bytes = None;
                    artifact.sha256 = None;
                    artifact.mime = homeboy_core::artifact_metadata::content_type_from_path(
                        Path::new(&declared),
                    );
                    ensure_metadata(artifact)
                        .insert("executor_artifact_finalized".to_string(), json!(false));
                    ensure_metadata(artifact).insert("review_only".to_string(), json!(true));
                    outcome.diagnostics.push(AgentTaskDiagnostic { class: "agent_task.legacy_external_artifact".to_string(), message: "Provider declared an artifact outside the Homeboy executor root; it remains review-only and is not promotable.".to_string(), data: json!({ "path": declared }) });
                }
                Err(error) => return Err(error),
            }
        }
        root.verify()?;
        let finalized = outcome.artifacts.clone();
        for typed in &mut outcome.typed_artifacts {
            if let Some(artifact) = &mut typed.artifact {
                if let Some(value) = finalized.iter().find(|value| value.id == artifact.id) {
                    *artifact = value.clone();
                }
            }
        }
        Ok(())
    }
}

fn staged_file(
    root: &ExecutorArtifactRootIdentity,
    artifact: &AgentTaskArtifact,
    declared: &str,
) -> Result<(PathBuf, u64, String, Option<String>)> {
    validate_token("artifact.id", &artifact.id)?;
    validate_token("artifact.kind", &artifact.kind)?;
    let declared = Path::new(declared);
    let candidate = if declared.is_absolute() {
        declared.to_path_buf()
    } else {
        root.path.join(declared)
    };
    if declared.is_absolute() && !candidate.starts_with(&root.path)
        || declared
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(outside_root_error(declared));
    }
    let link = fs::symlink_metadata(&candidate).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::validation_invalid_argument(
                "artifact.path",
                "executor artifact file was not found",
                Some(declared.display().to_string()),
                None,
            )
        } else {
            Error::internal_io(
                error.to_string(),
                Some(format!("inspect executor artifact {}", candidate.display())),
            )
        }
    })?;
    if is_link_or_reparse_point(&link) {
        return Err(Error::validation_invalid_argument(
            "artifact.path",
            "executor artifact must not be a symlink",
            Some(declared.display().to_string()),
            None,
        ));
    }
    reject_symlink_path_components(&root.path, &candidate)?;
    if !link.is_file() {
        return Err(Error::validation_invalid_argument(
            "artifact.path",
            "executor artifact must reference a file",
            Some(declared.display().to_string()),
            None,
        ));
    }
    root.verify()?;
    let mut token_hash = Sha256::new();
    token_hash.update(artifact.kind.as_bytes());
    token_hash.update([0]);
    token_hash.update(artifact.id.as_bytes());
    let token = format!("artifact-{:x}", token_hash.finalize());
    let destination = root.finalized_path.join(token);
    let temporary = destination.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(destination.parent().expect("finalized parent")).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create finalized artifact directory".to_string()),
        )
    })?;
    // The platform-specific open fixes the leaf we read. Verify that fixed
    // descriptor still names the canonical in-root file before copying bytes.
    let mut input = open_regular_file_no_follow(&candidate).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("open executor artifact {}", candidate.display())),
        )
    })?;
    verify_open_file_canonical_identity(root, &candidate, &input, declared)?;
    let mut output = File::create(&temporary).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("stage executor artifact {}", temporary.display())),
        )
    })?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = input.read(&mut buffer).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read executor artifact".to_string()),
            )
        })?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write finalized artifact".to_string()),
            )
        })?;
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    output.sync_all().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("sync finalized artifact".to_string()),
        )
    })?;
    let sha256 = format!("{:x}", hash.finalize());
    match fs::hard_link(&temporary, &destination) {
        Ok(()) => {
            fs::remove_file(&temporary).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("remove finalized artifact staging file".to_string()),
                )
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing_size = fs::metadata(&destination)
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some("inspect existing finalized artifact".to_string()),
                    )
                })?
                .len();
            let existing_sha256 = homeboy_core::artifact_metadata::sha256_file(&destination)?;
            fs::remove_file(&temporary).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("remove finalized artifact staging file".to_string()),
                )
            })?;
            if existing_size != size || existing_sha256 != sha256 {
                return Err(Error::validation_invalid_argument(
                    "artifact.id",
                    "executor artifact id already publishes different finalized bytes",
                    Some(artifact.id.clone()),
                    None,
                ));
            }
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some("publish finalized artifact".to_string()),
            ))
        }
    }
    Ok((
        destination.clone(),
        size,
        sha256,
        homeboy_core::artifact_metadata::content_type_from_path(&destination),
    ))
}

#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
struct WindowsFileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> std::io::Result<WindowsFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };

    let mut info = FILE_ID_INFO::default();
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(WindowsFileIdentity {
        volume_serial_number: info.VolumeSerialNumber,
        file_id: info.FileId.Identifier,
    })
}

#[cfg(windows)]
fn windows_file_identity_from_path(
    path: &Path,
    directory: bool,
) -> std::io::Result<WindowsFileIdentity> {
    let file = open_regular_file_no_follow_with_directory(path, directory)?;
    windows_file_identity(&file)
}

#[cfg(windows)]
fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn verify_open_file_canonical_identity(
    root: &ExecutorArtifactRootIdentity,
    candidate: &Path,
    input: &File,
    declared: &Path,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    root.verify()?;
    let canonical = candidate.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "canonicalize executor artifact {}",
                candidate.display()
            )),
        )
    })?;
    if !canonical.starts_with(&root.path) {
        return Err(outside_root_error(declared));
    }
    let opened = input.metadata().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("inspect opened executor artifact".to_string()),
        )
    })?;
    let canonical_metadata = fs::metadata(&canonical).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "inspect canonical executor artifact {}",
                canonical.display()
            )),
        )
    })?;
    if !opened.is_file()
        || !canonical_metadata.is_file()
        || opened.dev() != canonical_metadata.dev()
        || opened.ino() != canonical_metadata.ino()
    {
        return Err(Error::validation_invalid_argument(
            "artifact.path",
            "opened executor artifact no longer matches its canonical in-root path",
            Some(declared.display().to_string()),
            None,
        ));
    }
    root.verify()
}

#[cfg(windows)]
fn verify_open_file_canonical_identity(
    root: &ExecutorArtifactRootIdentity,
    candidate: &Path,
    input: &File,
    declared: &Path,
) -> Result<()> {
    root.verify()?;
    let canonical = candidate.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "canonicalize executor artifact {}",
                candidate.display()
            )),
        )
    })?;
    if !canonical.starts_with(&root.path) {
        return Err(outside_root_error(declared));
    }
    let canonical_file = open_regular_file_no_follow(&canonical).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "open canonical executor artifact {}",
                canonical.display()
            )),
        )
    })?;
    let opened = windows_file_identity(input).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("inspect opened executor artifact".to_string()),
        )
    })?;
    let canonical_identity = windows_file_identity(&canonical_file).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "inspect canonical executor artifact {}",
                canonical.display()
            )),
        )
    })?;
    if opened != canonical_identity {
        return Err(Error::validation_invalid_argument(
            "artifact.path",
            "opened executor artifact no longer matches its canonical in-root path",
            Some(declared.display().to_string()),
            None,
        ));
    }
    root.verify()
}

#[cfg(unix)]
fn open_regular_file_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "executor artifact is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_regular_file_no_follow(path: &Path) -> std::io::Result<File> {
    open_regular_file_no_follow_with_directory(path, false)
}

#[cfg(windows)]
fn open_regular_file_no_follow_with_directory(
    path: &Path,
    directory: bool,
) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(
        FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            },
    );
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if is_link_or_reparse_point(&metadata)
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(std::io::Error::other(
            "executor artifact is not a regular file",
        ));
    }
    Ok(file)
}

fn outside_root_error(declared: &Path) -> Error {
    Error::validation_invalid_argument(
        "artifact.path",
        "executor artifact path resolves outside the Homeboy-owned artifact root",
        Some(declared.display().to_string()),
        None,
    )
}

fn reject_symlink_path_components(root: &Path, candidate: &Path) -> Result<()> {
    let Ok(relative) = candidate.strip_prefix(root) else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("inspect executor artifact {}", current.display())),
            )
        })?;
        if is_link_or_reparse_point(&metadata) {
            return Err(Error::validation_invalid_argument(
                "artifact.path",
                "executor artifact path must not traverse a symlink",
                Some(candidate.display().to_string()),
                None,
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_token(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::validation_invalid_argument(
            field,
            "must be a non-empty safe logical token",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(())
}

fn ensure_metadata(
    artifact: &mut AgentTaskArtifact,
) -> &mut serde_json::Map<String, serde_json::Value> {
    if !artifact.metadata.is_object() {
        artifact.metadata = json!({});
    }
    artifact
        .metadata
        .as_object_mut()
        .expect("artifact metadata object")
}

#[cfg(any(not(any(unix, windows)), test))]
fn mark_review_only_for_unsupported_platform(
    artifact: &mut AgentTaskArtifact,
) -> AgentTaskDiagnostic {
    artifact.size_bytes = None;
    artifact.sha256 = None;
    ensure_metadata(artifact).insert("executor_artifact_finalized".to_string(), json!(false));
    ensure_metadata(artifact).insert("review_only".to_string(), json!(true));
    AgentTaskDiagnostic {
        class: "agent_task.artifact_finalization_unavailable".to_string(),
        message: "Verified executor artifact finalization is unavailable on this platform; the declaration remains review-only and is not promotable.".to_string(),
        data: json!({ "path": artifact.path }),
    }
}

fn is_legacy_outside_root(error: &Error) -> bool {
    error
        .message
        .contains("outside the Homeboy-owned artifact root")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
    };
    use homeboy_core::test_support::with_isolated_home;

    fn outcome(path: impl Into<String>) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: Some("patch".to_string()),
                label: None,
                role: None,
                semantic_key: None,
                path: Some(path.into()),
                url: None,
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: serde_json::Value::Null,
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: serde_json::Value::Null,
            workflow: None,
            follow_up: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn finalization_persists_opened_bytes_in_canonical_store_with_hash_parity() {
        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            fs::create_dir(&root_path).expect("executor root");
            let source = root_path.join("result.patch");
            fs::write(&source, b"verified patch bytes").expect("source");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");
            let mut value = outcome("result.patch");

            finalize_provider_file_artifacts(&mut value, &root).expect("finalize");

            let artifact = &value.artifacts[0];
            let persisted = Path::new(artifact.path.as_deref().expect("persisted path"));
            assert!(
                persisted.starts_with(homeboy_core::paths::artifact_root().expect("store root"))
            );
            assert_eq!(
                fs::read(persisted).expect("persisted bytes"),
                b"verified patch bytes"
            );
            assert_eq!(artifact.size_bytes, Some(20));
            assert_eq!(
                artifact.sha256,
                Some(
                    homeboy_core::artifact_metadata::sha256_file(persisted)
                        .expect("persisted hash")
                )
            );
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn finalization_rejects_replaced_executor_root() {
        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            fs::create_dir(&root_path).expect("executor root");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");
            fs::rename(&root_path, home.path().join("executor-original")).expect("replace root");
            fs::create_dir(&root_path).expect("replacement root");
            fs::write(root_path.join("result.patch"), b"replacement").expect("source");

            assert!(finalize_provider_file_artifacts(&mut outcome("result.patch"), &root).is_err());
        });
    }

    #[cfg(unix)]
    #[test]
    fn finalization_rejects_nested_symlink_before_opening_leaf() {
        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            let outside = home.path().join("outside");
            fs::create_dir(&root_path).expect("executor root");
            fs::create_dir(&outside).expect("outside root");
            fs::write(outside.join("result.patch"), b"outside").expect("source");
            std::os::unix::fs::symlink(&outside, root_path.join("nested")).expect("symlink");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");

            assert!(
                finalize_provider_file_artifacts(&mut outcome("nested/result.patch"), &root)
                    .is_err()
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn finalization_rejects_symlink_leaf() {
        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            fs::create_dir(&root_path).expect("executor root");
            let outside = home.path().join("outside.patch");
            fs::write(&outside, b"outside").expect("source");
            std::os::unix::fs::symlink(&outside, root_path.join("result.patch")).expect("symlink");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");

            assert!(finalize_provider_file_artifacts(&mut outcome("result.patch"), &root).is_err());
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn external_patch_remains_visible_without_becoming_an_aggregate_apply_candidate() {
        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            let external = home.path().join("external.patch");
            fs::create_dir(&root_path).expect("executor root");
            fs::write(&external, b"diff --git a/a b/a\n").expect("external patch");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");
            let mut value = outcome(external.display().to_string());
            value.artifacts[0].size_bytes = Some(128);

            finalize_provider_file_artifacts(&mut value, &root).expect("review-only finalization");

            let report = crate::agent_task_aggregate::AgentTaskAggregateReport::from(&[value][..]);
            assert_eq!(report.artifact_inventory.len(), 1);
            assert!(report.apply_candidates.is_empty());
            assert_eq!(report.summary.review_candidates, 1);
        });
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn finalization_rejects_open_descriptor_that_no_longer_matches_canonical_path() {
        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            fs::create_dir(&root_path).expect("executor root");
            let candidate = root_path.join("result.patch");
            fs::write(&candidate, b"original").expect("source");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");
            let input = open_regular_file_no_follow(&candidate).expect("open source");
            let replacement = root_path.join("replacement.patch");
            fs::write(&replacement, b"replacement").expect("replacement");
            fs::rename(&replacement, &candidate).expect("replace leaf");

            assert!(verify_open_file_canonical_identity(
                &root,
                &candidate,
                &input,
                Path::new("result.patch")
            )
            .is_err());
        });
    }

    #[test]
    fn unsupported_platform_helper_retains_artifact_for_review_only() {
        let mut value = outcome("result.patch");

        let diagnostic = mark_review_only_for_unsupported_platform(&mut value.artifacts[0]);

        assert_eq!(value.artifacts[0].metadata["review_only"], true);
        assert_eq!(value.artifacts[0].size_bytes, None);
        assert_eq!(
            diagnostic.class,
            "agent_task.artifact_finalization_unavailable"
        );
    }

    #[cfg(windows)]
    #[test]
    fn finalization_rejects_nested_junction_escape() {
        use std::process::Command;

        with_isolated_home(|home| {
            let root_path = home.path().join("executor");
            let outside = home.path().join("outside");
            let junction = root_path.join("nested");
            fs::create_dir(&root_path).expect("executor root");
            fs::create_dir(&outside).expect("outside root");
            fs::write(outside.join("result.patch"), b"outside").expect("source");
            let status = Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&junction)
                .arg(&outside)
                .status()
                .expect("create junction");
            assert!(status.success(), "create junction");
            let root = ExecutorArtifactRootIdentity::capture(&root_path).expect("capture root");

            assert!(
                finalize_provider_file_artifacts(&mut outcome("nested/result.patch"), &root)
                    .is_err()
            );
        });
    }
}
