//! Local artifact registration for command steps inside an observed rig run.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path};
use std::process::Command;

use homeboy_core::error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const RIG_ARTIFACT_MANIFEST_ENV: &str = "HOMEBOY_RIG_ARTIFACT_MANIFEST";
const MANIFEST_SCHEMA: &str = "homeboy/rig-command-artifacts/v1";
const MAX_REGISTRATIONS: usize = 128;
const MAX_KIND_LEN: usize = 64;
const MAX_PATH_LEN: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalArtifactRegistration {
    pub run_id: String,
    pub kind: String,
    pub artifact_type: String,
    pub path: String,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RegisteredLocalArtifact {
    pub kind: String,
    pub artifact_type: String,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistrationManifest {
    schema: String,
    run_id: String,
    artifacts: Vec<RegisteredLocalArtifact>,
}

#[derive(Clone)]
struct RegistrationContext {
    run_id: String,
    manifest_path: String,
}

thread_local! {
    static REGISTRATION_CONTEXT: RefCell<Option<RegistrationContext>> = const { RefCell::new(None) };
}

pub(crate) fn with_registration_context<T>(
    run_id: &str,
    manifest_path: &Path,
    operation: impl FnOnce() -> T,
) -> T {
    REGISTRATION_CONTEXT.with(|slot| {
        let previous = slot.replace(Some(RegistrationContext {
            run_id: run_id.to_string(),
            manifest_path: manifest_path.display().to_string(),
        }));
        let _guard = RegistrationContextGuard { slot, previous };
        operation()
    })
}

struct RegistrationContextGuard<'a> {
    slot: &'a RefCell<Option<RegistrationContext>>,
    previous: Option<RegistrationContext>,
}

impl Drop for RegistrationContextGuard<'_> {
    fn drop(&mut self) {
        self.slot.replace(self.previous.take());
    }
}

pub(crate) fn inherit_registration_context(command: &mut Command) {
    REGISTRATION_CONTEXT.with(|slot| {
        if let Some(context) = slot.borrow().as_ref() {
            command.env(
                homeboy_core::observation::ACTIVE_RUN_ID_ENV,
                &context.run_id,
            );
            command.env(RIG_ARTIFACT_MANIFEST_ENV, &context.manifest_path);
        }
    });
}

pub fn register_current_run_artifact(
    kind: &str,
    path: impl AsRef<Path>,
) -> Result<LocalArtifactRegistration> {
    validate_kind(kind)?;
    let run_id = required_env(homeboy_core::observation::ACTIVE_RUN_ID_ENV)?;
    let manifest_path = required_env(RIG_ARTIFACT_MANIFEST_ENV)?;
    let manifest_path = Path::new(&manifest_path);
    if !manifest_path.is_absolute() || !manifest_path.is_file() {
        return Err(Error::validation_invalid_argument(
            "artifact_manifest",
            "rig artifact registration manifest must be an existing absolute file",
            Some(manifest_path.display().to_string()),
            None,
        ));
    }

    let (canonical_path, artifact_type) = safe_artifact_path(path.as_ref())?;
    let path_string = canonical_path.display().to_string();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(manifest_path)
        .map_err(|error| manifest_io_error(manifest_path, error))?;
    lock_manifest(&file)?;
    let result = update_manifest(&mut file, &run_id, kind, &artifact_type, &path_string);
    unlock_manifest(&file);
    let duplicate = result?;

    Ok(LocalArtifactRegistration {
        run_id,
        kind: kind.to_string(),
        artifact_type,
        path: path_string,
        duplicate,
    })
}

pub(crate) fn read_registrations(
    run_id: &str,
    manifest_path: &Path,
) -> Result<Vec<RegisteredLocalArtifact>> {
    let mut file =
        File::open(manifest_path).map_err(|error| manifest_io_error(manifest_path, error))?;
    let manifest = read_manifest(&mut file, run_id)?;
    let mut validated = Vec::with_capacity(manifest.artifacts.len());
    for artifact in manifest.artifacts {
        validate_kind(&artifact.kind)?;
        let (canonical, artifact_type) = safe_artifact_path(Path::new(&artifact.path))?;
        if canonical.display().to_string() != artifact.path
            || artifact_type != artifact.artifact_type
        {
            return Err(Error::validation_invalid_argument(
                "artifact_manifest",
                "rig artifact registration entry does not match its canonical path and type",
                Some(artifact.path),
                None,
            ));
        }
        if validated.iter().any(|existing: &RegisteredLocalArtifact| {
            existing.kind == artifact.kind && existing.path == artifact.path
        }) {
            continue;
        }
        validated.push(artifact);
    }
    Ok(validated)
}

fn update_manifest(
    file: &mut File,
    run_id: &str,
    kind: &str,
    artifact_type: &str,
    path: &str,
) -> Result<bool> {
    let mut manifest = read_manifest(file, run_id)?;
    if manifest
        .artifacts
        .iter()
        .any(|artifact| artifact.kind == kind && artifact.path == path)
    {
        return Ok(true);
    }
    if manifest.artifacts.len() >= MAX_REGISTRATIONS {
        return Err(Error::validation_invalid_argument(
            "artifact_manifest",
            format!("rig command steps may register at most {MAX_REGISTRATIONS} artifacts"),
            Some(manifest.artifacts.len().to_string()),
            None,
        ));
    }
    manifest.artifacts.push(RegisteredLocalArtifact {
        kind: kind.to_string(),
        artifact_type: artifact_type.to_string(),
        path: path.to_string(),
    });
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("seek rig artifact manifest".to_string()),
        )
    })?;
    file.set_len(0).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("truncate rig artifact manifest".to_string()),
        )
    })?;
    serde_json::to_writer(&mut *file, &manifest).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize rig artifact manifest".to_string()),
        )
    })?;
    file.write_all(b"\n").map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write rig artifact manifest".to_string()),
        )
    })?;
    file.sync_data().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("sync rig artifact manifest".to_string()),
        )
    })?;
    Ok(false)
}

fn read_manifest(file: &mut File, run_id: &str) -> Result<RegistrationManifest> {
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("seek rig artifact manifest".to_string()),
        )
    })?;
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read rig artifact manifest".to_string()),
        )
    })?;
    if raw.trim().is_empty() {
        return Ok(RegistrationManifest {
            schema: MANIFEST_SCHEMA.to_string(),
            run_id: run_id.to_string(),
            artifacts: Vec::new(),
        });
    }
    let manifest: RegistrationManifest = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "artifact_manifest",
            format!("invalid rig artifact registration manifest: {error}"),
            None,
            None,
        )
    })?;
    if manifest.schema != MANIFEST_SCHEMA || manifest.run_id != run_id {
        return Err(Error::validation_invalid_argument(
            "artifact_manifest",
            "rig artifact registration manifest does not match the active run",
            Some(manifest.run_id),
            None,
        ));
    }
    if manifest.artifacts.len() > MAX_REGISTRATIONS {
        return Err(Error::validation_invalid_argument(
            "artifact_manifest",
            "rig artifact registration manifest exceeds its entry limit",
            Some(manifest.artifacts.len().to_string()),
            None,
        ));
    }
    Ok(manifest)
}

fn validate_kind(kind: &str) -> Result<()> {
    let valid = !kind.is_empty()
        && kind.len() <= MAX_KIND_LEN
        && kind.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'_' | b'-' | b'.'))
        });
    if valid {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "kind",
        "artifact kind must be 1-64 lowercase ASCII letters, digits, dots, dashes, or underscores and start with a letter or digit",
        Some(kind.to_string()),
        None,
    ))
}

fn safe_artifact_path(path: &Path) -> Result<(std::path::PathBuf, String)> {
    let raw = path.as_os_str().to_string_lossy();
    if raw.is_empty()
        || raw.len() > MAX_PATH_LEN
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(Error::validation_invalid_argument(
            "path",
            "artifact path must be non-empty, bounded, and must not contain parent directory components",
            Some(raw.to_string()),
            None,
        ));
    }
    let link_metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::validation_invalid_argument(
            "path",
            format!("artifact path is missing or unreadable: {error}"),
            Some(raw.to_string()),
            None,
        )
    })?;
    if link_metadata.file_type().is_symlink() {
        return Err(Error::validation_invalid_argument(
            "path",
            "artifact path must not be a symbolic link",
            Some(raw.to_string()),
            None,
        ));
    }
    let artifact_type = if link_metadata.is_file() {
        "file"
    } else if link_metadata.is_dir() {
        "directory"
    } else {
        return Err(Error::validation_invalid_argument(
            "path",
            "artifact path must be a regular file or directory",
            Some(raw.to_string()),
            None,
        ));
    };
    let canonical = fs::canonicalize(path).map_err(|error| {
        Error::validation_invalid_argument(
            "path",
            format!("artifact path could not be canonicalized: {error}"),
            Some(raw.to_string()),
            None,
        )
    })?;
    if artifact_type == "directory" {
        validate_directory_entries(&canonical)?;
    }
    Ok((canonical, artifact_type.to_string()))
}

fn validate_directory_entries(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            Error::validation_invalid_argument(
                "path",
                format!("artifact directory is unreadable: {error}"),
                Some(directory.display().to_string()),
                None,
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                Error::validation_invalid_argument(
                    "path",
                    format!("artifact directory entry is unreadable: {error}"),
                    Some(directory.display().to_string()),
                    None,
                )
            })?;
            seen += 1;
            if seen > 10_000 {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "artifact directory may contain at most 10000 entries",
                    Some(root.display().to_string()),
                    None,
                ));
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                Error::validation_invalid_argument(
                    "path",
                    format!("artifact directory entry is unreadable: {error}"),
                    Some(entry.path().display().to_string()),
                    None,
                )
            })?;
            if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "artifact directories may contain only regular files and directories",
                    Some(entry.path().display().to_string()),
                    None,
                ));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_context",
                "local artifact registration is only available inside an observed rig command step",
                None,
                Some(vec![format!("Missing inherited {name} context.")]),
            )
        })
}

fn manifest_io_error(path: &Path, error: std::io::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("access rig artifact manifest {}", path.display())),
    )
}

#[cfg(unix)]
fn lock_manifest(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some("lock rig artifact manifest".to_string()),
        ))
    }
}

#[cfg(not(unix))]
fn lock_manifest(_file: &File) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn unlock_manifest(file: &File) {
    use std::os::fd::AsRawFd;
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(not(unix))]
fn unlock_manifest(_file: &File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        run_id: Option<String>,
        manifest: Option<String>,
    }

    impl EnvGuard {
        fn set(run_id: Option<&str>, manifest: Option<&Path>) -> Self {
            let guard = Self {
                run_id: std::env::var(homeboy_core::observation::ACTIVE_RUN_ID_ENV).ok(),
                manifest: std::env::var(RIG_ARTIFACT_MANIFEST_ENV).ok(),
            };
            match run_id {
                Some(run_id) => {
                    std::env::set_var(homeboy_core::observation::ACTIVE_RUN_ID_ENV, run_id)
                }
                None => std::env::remove_var(homeboy_core::observation::ACTIVE_RUN_ID_ENV),
            }
            match manifest {
                Some(path) => std::env::set_var(RIG_ARTIFACT_MANIFEST_ENV, path),
                None => std::env::remove_var(RIG_ARTIFACT_MANIFEST_ENV),
            }
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.run_id {
                Some(value) => {
                    std::env::set_var(homeboy_core::observation::ACTIVE_RUN_ID_ENV, value)
                }
                None => std::env::remove_var(homeboy_core::observation::ACTIVE_RUN_ID_ENV),
            }
            match &self.manifest {
                Some(value) => std::env::set_var(RIG_ARTIFACT_MANIFEST_ENV, value),
                None => std::env::remove_var(RIG_ARTIFACT_MANIFEST_ENV),
            }
        }
    }

    #[test]
    fn registers_files_and_directories_and_deduplicates_entries() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("temp dir");
        let manifest = tempfile::NamedTempFile::new().expect("manifest");
        let file = temp.path().join("result.json");
        let directory = temp.path().join("bundle");
        fs::write(&file, "{}").expect("file artifact");
        fs::create_dir(&directory).expect("directory artifact");
        fs::write(directory.join("failure.json"), "{}").expect("bundle content");
        let _env = EnvGuard::set(Some("rig-run-1"), Some(manifest.path()));

        let first =
            register_current_run_artifact("wp_codebox_result", &file).expect("file registration");
        let duplicate = register_current_run_artifact("wp_codebox_result", &file)
            .expect("duplicate registration");
        let bundle = register_current_run_artifact("wp_codebox_bundle", &directory)
            .expect("directory registration");
        let registrations = read_registrations("rig-run-1", manifest.path()).expect("manifest");

        assert_eq!(first.artifact_type, "file");
        assert!(!first.duplicate);
        assert!(duplicate.duplicate);
        assert_eq!(bundle.artifact_type, "directory");
        assert_eq!(registrations.len(), 2);
        assert_eq!(registrations[0].kind, "wp_codebox_result");
        assert_eq!(registrations[1].kind, "wp_codebox_bundle");
    }

    #[test]
    fn rejects_missing_unsafe_and_unstable_artifacts() {
        let temp = tempfile::tempdir().expect("temp dir");
        let missing = temp.path().join("missing.json");
        let unsafe_path = temp.path().join("child").join("..").join("missing.json");

        assert!(safe_artifact_path(&missing).is_err());
        assert!(safe_artifact_path(&unsafe_path).is_err());
        assert!(validate_kind("WP Codebox Bundle").is_err());
        assert!(validate_kind("wp_codebox_bundle").is_ok());

        #[cfg(unix)]
        {
            let directory = temp.path().join("bundle");
            fs::create_dir(&directory).expect("bundle");
            std::os::unix::fs::symlink("/etc/passwd", directory.join("unsafe-link"))
                .expect("symlink");
            assert!(safe_artifact_path(&directory).is_err());
        }
    }

    #[test]
    fn rejects_registration_without_an_enclosing_run() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("result.json");
        fs::write(&artifact, "{}").expect("artifact");
        let _env = EnvGuard::set(None, None);

        let error = register_current_run_artifact("result", artifact)
            .expect_err("registration requires run context");

        assert!(error.to_string().contains("observed rig command step"));
    }
}
