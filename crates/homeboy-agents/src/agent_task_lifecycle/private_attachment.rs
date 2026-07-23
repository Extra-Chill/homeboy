//! Owner-only immutable payloads associated with an authoritative agent-task run.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use homeboy_core::engine::canonical_json::canonical_json_bytes;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{sanitize_run_id, store, Error, Result};

pub const PRIVATE_RUN_ATTACHMENT_SCHEMA: &str = "homeboy/agent-task-private-attachment/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PrivateRunAttachment<T> {
    pub schema: String,
    pub run_id: String,
    pub kind: String,
    pub payload_digest: String,
    pub payload: T,
}

fn validated_kind(kind: &str) -> Result<&str> {
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(Error::validation_invalid_argument(
            "kind",
            "private attachment kind must use lowercase ASCII letters, digits, and hyphens",
            None,
            None,
        ));
    }
    Ok(kind)
}

fn validated_run_id(run_id: &str) -> Result<String> {
    let sanitized = sanitize_run_id(run_id);
    if run_id.is_empty() || sanitized != run_id || Path::new(run_id).components().count() != 1 {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "private attachment run id must be one safe path component",
            None,
            None,
        ));
    }
    Ok(sanitized)
}

fn attachment_path(run_id: &str, kind: &str) -> Result<PathBuf> {
    let run_id = validated_run_id(run_id)?;
    validated_kind(kind)?;
    Ok(store::run_dir(&run_id)?
        .join("private")
        .join(format!("{kind}.json")))
}

/// Version 1 digests recursively canonicalized JSON: object keys are sorted and
/// all persistence serializes that same canonical envelope.
fn canonical_bytes<T: Serialize>(payload: &T) -> Result<Vec<u8>> {
    canonical_json_bytes(payload)
        .map_err(|_| Error::internal_json("serialize private run attachment", None))
}

fn digest<T: Serialize>(payload: &T) -> Result<String> {
    let bytes = canonical_bytes(payload)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn validate<T: Serialize>(
    attachment: &PrivateRunAttachment<T>,
    run_id: &str,
    kind: &str,
) -> Result<()> {
    if attachment.schema != PRIVATE_RUN_ATTACHMENT_SCHEMA
        || attachment.run_id != validated_run_id(run_id)?
        || attachment.kind != kind
        || attachment.payload_digest != digest(&attachment.payload)?
    {
        return Err(Error::validation_invalid_argument(
            "private_attachment",
            "private run attachment schema, binding, or payload digest is invalid",
            None,
            None,
        ));
    }
    Ok(())
}

fn io_error(operation: &'static str) -> impl FnOnce(std::io::Error) -> Error {
    move |_| Error::internal_io(operation, None)
}

fn ensure_private_directory(run_id: &str) -> Result<PathBuf> {
    let run_dir = store::run_dir(run_id)?;
    ensure_real_owner_directory(&run_dir, "private attachment run directory")?;
    let private_dir = run_dir.join("private");
    ensure_real_owner_directory(&private_dir, "private attachment directory")?;
    Ok(private_dir)
}

fn ensure_real_owner_directory(path: &Path, operation: &'static str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::validation_invalid_argument(
                    "private_attachment",
                    "private attachment storage directory is unsafe",
                    None,
                    None,
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(path).map_err(io_error(operation))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(Error::validation_invalid_argument(
                        "private_attachment",
                        "private attachment storage directory is unsafe",
                        None,
                        None,
                    ));
                }
            }
            Err(error) => return Err(io_error(operation)(error)),
        },
        Err(error) => return Err(io_error(operation)(error)),
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error(operation))?;
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn write_temp_file(directory: &Path, bytes: &[u8]) -> Result<PathBuf> {
    for _ in 0..16 {
        let path = directory.join(format!(".attachment-{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(&path);
                    return Err(io_error("write private attachment temporary file")(error));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create private attachment temporary file")(error)),
        }
    }
    Err(Error::internal_io(
        "create private attachment temporary file",
        None,
    ))
}

fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error("sync private attachment directory"))
}

fn persist_no_replace(path: &Path, bytes: &[u8]) -> Result<bool> {
    let directory = path.parent().expect("attachment has parent");
    let temporary = write_temp_file(directory, bytes)?;
    let installed = match fs::hard_link(&temporary, path) {
        Ok(()) => {
            sync_directory(directory)?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(io_error("install private attachment")(error));
        }
    };
    let _ = fs::remove_file(&temporary);
    if installed {
        sync_directory(directory)?;
    }
    Ok(installed)
}

/// Persist an immutable owner-only attachment. Exact replays are idempotent;
/// a changed payload conflicts rather than replacing the original recipe.
pub fn persist_private_run_attachment<T: Serialize + DeserializeOwned + Clone + PartialEq>(
    run_id: &str,
    kind: &str,
    payload: &T,
) -> Result<PrivateRunAttachment<T>> {
    let run_id = validated_run_id(run_id)?;
    // Prove the authoritative lifecycle exists before creating side data.
    store::read_record(&run_id)?;
    let path = attachment_path(&run_id, kind)?;
    let attachment = PrivateRunAttachment {
        schema: PRIVATE_RUN_ATTACHMENT_SCHEMA.to_string(),
        run_id: run_id.clone(),
        kind: kind.to_string(),
        payload_digest: digest(payload)?,
        payload: payload.clone(),
    };
    ensure_private_directory(&run_id)?;
    let bytes = canonical_bytes(&attachment)?;
    if persist_no_replace(&path, &bytes)? {
        return Ok(attachment);
    }
    let existing = load_private_run_attachment(&attachment.run_id, kind)?;
    if existing.payload_digest == attachment.payload_digest {
        return Ok(existing);
    }
    Err(Error::validation_invalid_argument(
        "private_attachment",
        "private run attachment already exists with a different immutable payload",
        None,
        None,
    ))
}

pub fn load_private_run_attachment<T: Serialize + DeserializeOwned>(
    run_id: &str,
    kind: &str,
) -> Result<PrivateRunAttachment<T>> {
    let run_id = validated_run_id(run_id)?;
    store::read_record(&run_id)?;
    let path = attachment_path(&run_id, kind)?;
    ensure_private_directory(&run_id)?;
    let metadata = fs::symlink_metadata(&path).map_err(io_error("inspect private attachment"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::validation_invalid_argument(
            "private_attachment",
            "private attachment storage file is unsafe",
            None,
            None,
        ));
    }
    let contents = fs::read_to_string(&path).map_err(io_error("read private attachment"))?;
    let attachment: PrivateRunAttachment<T> = serde_json::from_str(&contents)
        .map_err(|_| Error::internal_json("parse private attachment", None))?;
    validate(&attachment, &run_id, kind)?;
    Ok(attachment)
}
