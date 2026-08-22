//! Runner-file HTTP operations for the daemon: create workspace file
//! directories, upload/download runner files, and resolve/normalize workspace
//! paths safely within the runner root. Extracted from the `daemon` god file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use homeboy_engine_primitives::content_hash;
use serde_json::json;

use super::remote_runner;
use super::runner_workspace_root;
use super::{FilePathRequest, FileUploadAbortRequest, FileUploadChunkRequest, FileUploadRequest};
use crate::broker_auth::BrokerScope;
use crate::error::{Error, Result};

const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024;
const MAX_UNFINISHED_UPLOADS_PER_SCOPE: usize = 8;
const UPLOAD_EXPIRY: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Hash, Eq, PartialEq)]
struct UploadKey {
    runner_id: String,
    workspace_root: Option<String>,
    upload_id: uuid::Uuid,
}

struct PendingUpload {
    temp: PathBuf,
    size_bytes: u64,
    reserved_bytes: u64,
    updated_at: Instant,
}

#[derive(Default)]
struct UploadRegistry {
    uploads: std::collections::HashMap<UploadKey, PendingUpload>,
}

fn upload_registry() -> &'static Mutex<UploadRegistry> {
    static REGISTRY: OnceLock<Mutex<UploadRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(UploadRegistry::default()))
}

fn reap_expired_uploads_locked(registry: &mut UploadRegistry) {
    registry.uploads.retain(|_, upload| {
        if upload.updated_at.elapsed() < UPLOAD_EXPIRY {
            return true;
        }
        let _ = fs::remove_file(&upload.temp);
        false
    });
}

pub(super) fn reap_expired_uploads() {
    let mut registry = upload_registry().lock().expect("upload registry lock");
    reap_expired_uploads_locked(&mut registry);
}

pub(super) fn create_runner_file_directory(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FilePathRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse file mkdir request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    fs::create_dir_all(&path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("create {}", path.display())))
    })?;
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
    }))
}

pub(super) fn upload_runner_file(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FileUploadRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse file upload request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let content = base64::engine::general_purpose::STANDARD
        .decode(&request.content_base64)
        .map_err(|err| {
            Error::validation_invalid_argument(
                "content_base64",
                format!("runner file upload content is not valid base64: {err}"),
                None,
                None,
            )
        })?;
    if let Some(expected) = request.sha256.as_deref() {
        let actual = content_hash::sha256_hex(&content);
        if actual != expected {
            return Err(Error::validation_invalid_argument(
                "sha256",
                "runner file upload content does not match its declared SHA-256",
                Some(path.display().to_string()),
                None,
            ));
        }
    }
    if request.private || request.atomic {
        let temp = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("upload"),
            uuid::Uuid::new_v4()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("create {}", temp.display())))
        })?;
        let write = file.write_all(&content).and_then(|_| file.sync_all());
        if let Err(err) = write {
            let _ = fs::remove_file(&temp);
            return Err(Error::internal_io(
                err.to_string(),
                Some(format!("write {}", temp.display())),
            ));
        }
        #[cfg(unix)]
        if request.private {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).map_err(|err| {
                Error::internal_io(err.to_string(), Some(format!("chmod {}", temp.display())))
            })?;
        }
        fs::rename(&temp, &path).map_err(|err| {
            let _ = fs::remove_file(&temp);
            Error::internal_io(err.to_string(), Some(format!("publish {}", path.display())))
        })?;
    } else {
        fs::write(&path, &content).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
        })?;
    }
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
        "size_bytes": content.len(),
    }))
}

pub(super) fn upload_runner_file_chunk(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FileUploadChunkRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse file upload chunk request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let upload_id = uuid::Uuid::parse_str(&request.upload_id).map_err(|_| {
        Error::validation_invalid_argument(
            "upload_id",
            "runner file upload chunk requires a UUID upload id",
            Some(request.upload_id.clone()),
            None,
        )
    })?;
    if request.size_bytes > MAX_UPLOAD_BYTES {
        return Err(Error::validation_invalid_argument(
            "size_bytes",
            "runner file upload exceeds the 67108864-byte limit",
            Some(request.size_bytes.to_string()),
            None,
        ));
    }
    // A final request must be complete before opening the temporary file.
    if request.final_chunk && request.sha256.as_deref().is_none() {
        return Err(Error::invalid_argument(
            "sha256",
            "final runner file upload chunk requires a SHA-256",
        ));
    }
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    // Base64 expands 64 KiB to at most 87384 bytes. Reject before decoding so a
    // JSON client cannot make decoding allocate an arbitrary buffer.
    if request.content_base64.len() > 4 * ((MAX_CHUNK_BYTES + 2) / 3) {
        return Err(Error::validation_invalid_argument(
            "content_base64",
            "runner file upload chunk encoded body exceeds the limit",
            Some(path.display().to_string()),
            None,
        ));
    }
    let content = base64::engine::general_purpose::STANDARD
        .decode(&request.content_base64)
        .map_err(|error| {
            Error::validation_invalid_argument(
                "content_base64",
                format!("runner file upload chunk content is not valid base64: {error}"),
                None,
                None,
            )
        })?;
    if content.len() > MAX_CHUNK_BYTES {
        return Err(Error::validation_invalid_argument(
            "content_base64",
            "runner file upload chunk exceeds the 65536-byte limit",
            Some(path.display().to_string()),
            None,
        ));
    }
    let temp = path.with_file_name(format!(
        ".{}.{}.upload",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upload"),
        upload_id
    ));
    let key = UploadKey {
        runner_id: request.runner_id.clone(),
        workspace_root: request.workspace_root.clone(),
        upload_id,
    };
    let mut registry = upload_registry().lock().expect("upload registry lock");
    reap_expired_uploads_locked(&mut registry);
    let current = registry
        .uploads
        .get(&key)
        .map(|upload| upload.size_bytes)
        .unwrap_or_else(|| {
            fs::metadata(&temp)
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        });
    if current != request.offset || current + content.len() as u64 > request.size_bytes {
        return Err(Error::validation_invalid_argument(
            "provider_evidence",
            "runner evidence chunk does not match its declared offset or size",
            Some(path.display().to_string()),
            None,
        ));
    }
    if !registry.uploads.contains_key(&key) {
        let runner_uploads = registry
            .uploads
            .keys()
            .filter(|existing| existing.runner_id == key.runner_id)
            .count();
        let runner_bytes = registry
            .uploads
            .iter()
            .filter(|(existing, _)| existing.runner_id == key.runner_id)
            .map(|(_, upload)| upload.reserved_bytes)
            .sum::<u64>();
        let scope_uploads = registry
            .uploads
            .keys()
            .filter(|existing| {
                existing.runner_id == key.runner_id && existing.workspace_root == key.workspace_root
            })
            .count();
        let scope_bytes = registry
            .uploads
            .iter()
            .filter(|(existing, _)| {
                existing.runner_id == key.runner_id && existing.workspace_root == key.workspace_root
            })
            .map(|(_, upload)| upload.reserved_bytes)
            .sum::<u64>();
        if runner_uploads >= MAX_UNFINISHED_UPLOADS_PER_SCOPE
            || runner_bytes.saturating_add(request.size_bytes) > MAX_UPLOAD_BYTES
            || scope_uploads >= MAX_UNFINISHED_UPLOADS_PER_SCOPE
            || scope_bytes.saturating_add(request.size_bytes) > MAX_UPLOAD_BYTES
        {
            return Err(Error::validation_invalid_argument(
                "upload_id",
                "runner or runner workspace has reached its unfinished upload quota",
                Some(path.display().to_string()),
                None,
            ));
        }
        registry.uploads.insert(
            key.clone(),
            PendingUpload {
                temp: temp.clone(),
                size_bytes: current,
                reserved_bytes: request.size_bytes,
                updated_at: Instant::now(),
            },
        );
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if request.private { 0o600 } else { 0o644 });
    }
    let mut file = match options.open(&temp) {
        Ok(file) => file,
        Err(error) => {
            registry.uploads.remove(&key);
            return Err(Error::internal_io(
                error.to_string(),
                Some(temp.display().to_string()),
            ));
        }
    };
    if let Err(error) = file.write_all(&content).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        registry.uploads.remove(&key);
        return Err(Error::internal_io(
            error.to_string(),
            Some(temp.display().to_string()),
        ));
    }
    let size = current + content.len() as u64;
    if let Some(upload) = registry.uploads.get_mut(&key) {
        upload.size_bytes = size;
        upload.updated_at = Instant::now();
    }
    if request.final_chunk {
        let expected = request.sha256.as_deref().expect("validated before write");
        let digest_matches = crate::artifact_metadata::sha256_file(&temp)
            .map(|actual| actual == expected)
            .unwrap_or(false);
        if size != request.size_bytes || !digest_matches {
            let _ = fs::remove_file(&temp);
            registry.uploads.remove(&key);
            return Err(Error::validation_invalid_argument(
                "provider_evidence",
                "runner evidence chunk upload does not match its declared digest or size",
                Some(path.display().to_string()),
                None,
            ));
        }
        if let Err(error) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            registry.uploads.remove(&key);
            return Err(Error::internal_io(
                error.to_string(),
                Some(path.display().to_string()),
            ));
        }
        registry.uploads.remove(&key);
    }
    Ok(
        json!({"runner_id": request.runner_id, "path": path.display().to_string(), "size_bytes": size, "final": request.final_chunk}),
    )
}

pub(super) fn abort_runner_file_chunk_upload(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FileUploadAbortRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse file upload abort request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let upload_id = uuid::Uuid::parse_str(&request.upload_id).map_err(|_| {
        Error::validation_invalid_argument(
            "upload_id",
            "runner file upload abort requires a UUID upload id",
            Some(request.upload_id.clone()),
            None,
        )
    })?;
    let key = UploadKey {
        runner_id: request.runner_id.clone(),
        workspace_root: request.workspace_root,
        upload_id,
    };
    let mut registry = upload_registry().lock().expect("upload registry lock");
    reap_expired_uploads_locked(&mut registry);
    if let Some(upload) = registry.uploads.remove(&key) {
        fs::remove_file(&upload.temp).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("remove {}", upload.temp.display())),
            )
        })?;
    }
    Ok(json!({ "runner_id": request.runner_id, "upload_id": request.upload_id, "aborted": true }))
}

pub(super) fn download_runner_file(
    body: Option<serde_json::Value>,
    broker_auth: &remote_runner::BrokerAuthContext,
) -> Result<serde_json::Value> {
    let request: FilePathRequest = serde_json::from_value(body.unwrap_or_else(|| json!({})))
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("parse file download request".to_string()),
            )
        })?;
    broker_auth.authorize(BrokerScope::Submit, Some(&request.runner_id))?;
    let path = resolve_runner_workspace_path(
        &request.runner_id,
        &request.path,
        request.workspace_root.as_deref(),
    )?;
    let content = fs::read(&path).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("read {}", path.display())))
    })?;
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
        "size_bytes": content.len(),
        "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
    }))
}

fn resolve_runner_workspace_path(
    runner_id: &str,
    requested_path: &str,
    request_workspace_root: Option<&str>,
) -> Result<PathBuf> {
    let resolved_root;
    let workspace_root = match request_workspace_root.filter(|root| !root.trim().is_empty()) {
        Some(root) => root,
        None => {
            resolved_root = runner_workspace_root::runner_workspace_root(runner_id);
            resolved_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace_root",
                    format!("runner `{runner_id}` file API requires workspace_root"),
                    Some(runner_id.to_string()),
                    Some(vec![
                        "Configure the runner workspace_root before using daemon file transfer."
                            .to_string(),
                    ]),
                )
            })?
        }
    };
    let root = fs::canonicalize(workspace_root).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "canonicalize runner workspace_root {workspace_root}"
            )),
        )
    })?;
    let requested = Path::new(requested_path);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let normalized = canonicalize_existing_prefix(&normalize_path(&candidate));
    if !normalized.starts_with(&root) {
        return Err(Error::validation_invalid_argument(
            "path",
            "runner file path must stay inside the runner workspace_root",
            Some(requested_path.to_string()),
            Some(vec![format!(
                "Runner `{runner_id}` workspace_root is {}.",
                root.display()
            )]),
        ));
    }
    Ok(normalized)
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn canonicalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }

    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = fs::canonicalize(current) {
            let mut resolved = canonical;
            for component in missing.iter().rev() {
                resolved.push(component);
            }
            return resolved;
        }
        let Some(file_name) = current.file_name() else {
            return path.to_path_buf();
        };
        missing.push(file_name.to_os_string());
        let Some(parent) = current.parent() else {
            return path.to_path_buf();
        };
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(workspace: &Path, runner_id: &str, upload_id: uuid::Uuid) -> serde_json::Value {
        json!({
            "runner_id": runner_id,
            "workspace_root": workspace.display().to_string(),
            "path": "evidence.bin",
            "upload_id": upload_id.to_string(),
            "offset": 0,
            "content_base64": "",
            "final": false,
            "size_bytes": 1,
            "private": true,
        })
    }

    fn trusted() -> remote_runner::BrokerAuthContext {
        remote_runner::BrokerAuthContext::trusted_local()
    }

    #[test]
    fn chunk_upload_rejects_oversized_declarations_and_encoded_or_decoded_chunks() {
        let workspace = tempfile::tempdir().expect("workspace");
        let runner = format!("chunk-limits-{}", uuid::Uuid::new_v4());
        let mut oversized = request(workspace.path(), &runner, uuid::Uuid::new_v4());
        oversized["size_bytes"] = json!(MAX_UPLOAD_BYTES + 1);
        assert!(upload_runner_file_chunk(Some(oversized), &trusted()).is_err());

        let mut encoded = request(workspace.path(), &runner, uuid::Uuid::new_v4());
        encoded["content_base64"] = json!("A".repeat(4 * ((MAX_CHUNK_BYTES + 2) / 3) + 1));
        assert!(upload_runner_file_chunk(Some(encoded), &trusted()).is_err());

        let mut decoded = request(workspace.path(), &runner, uuid::Uuid::new_v4());
        decoded["content_base64"] =
            json!(base64::engine::general_purpose::STANDARD.encode(vec![0; MAX_CHUNK_BYTES + 1]));
        decoded["size_bytes"] = json!((MAX_CHUNK_BYTES + 1) as u64);
        assert!(upload_runner_file_chunk(Some(decoded), &trusted()).is_err());
    }

    #[test]
    fn final_chunk_requires_digest_before_writing() {
        let workspace = tempfile::tempdir().expect("workspace");
        let runner = format!("chunk-digest-{}", uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4();
        let mut body = request(workspace.path(), &runner, id);
        body["content_base64"] = json!(base64::engine::general_purpose::STANDARD.encode(b"x"));
        body["size_bytes"] = json!(1);
        body["final"] = json!(true);
        assert!(upload_runner_file_chunk(Some(body), &trusted()).is_err());
        assert!(!workspace
            .path()
            .join(format!(".evidence.bin.{id}.upload"))
            .exists());
    }

    #[test]
    fn unfinished_uploads_reserve_runner_workspace_quota_across_ids() {
        let workspace = tempfile::tempdir().expect("workspace");
        let runner = format!("chunk-quota-{}", uuid::Uuid::new_v4());
        for _ in 0..MAX_UNFINISHED_UPLOADS_PER_SCOPE {
            let body = request(workspace.path(), &runner, uuid::Uuid::new_v4());
            upload_runner_file_chunk(Some(body), &trusted()).expect("reserve upload");
        }
        let error = upload_runner_file_chunk(
            Some(request(workspace.path(), &runner, uuid::Uuid::new_v4())),
            &trusted(),
        )
        .expect_err("ninth unique upload exceeds quota");
        assert!(error.message.contains("unfinished upload quota"));

        let byte_runner = format!("chunk-byte-quota-{}", uuid::Uuid::new_v4());
        let first_id = uuid::Uuid::new_v4();
        let mut first = request(workspace.path(), &byte_runner, first_id);
        first["size_bytes"] = json!(MAX_UPLOAD_BYTES / 2 + 1);
        upload_runner_file_chunk(Some(first), &trusted()).expect("reserve byte quota");
        let mut second = request(workspace.path(), &byte_runner, uuid::Uuid::new_v4());
        second["size_bytes"] = json!(MAX_UPLOAD_BYTES / 2 + 1);
        assert!(upload_runner_file_chunk(Some(second), &trusted()).is_err());
        abort_runner_file_chunk_upload(
            Some(json!({"runner_id": byte_runner, "workspace_root": workspace.path().display().to_string(), "upload_id": first_id})),
            &trusted(),
        )
        .expect("release byte quota");
    }

    #[test]
    fn abort_and_expiry_remove_partial_uploads() {
        let workspace = tempfile::tempdir().expect("workspace");
        let runner = format!("chunk-cleanup-{}", uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4();
        let body = request(workspace.path(), &runner, id);
        upload_runner_file_chunk(Some(body), &trusted()).expect("create partial upload");
        let temp = workspace.path().join(format!(".evidence.bin.{id}.upload"));
        assert!(temp.exists());
        abort_runner_file_chunk_upload(
            Some(json!({"runner_id": runner, "workspace_root": workspace.path().display().to_string(), "upload_id": id})),
            &trusted(),
        )
        .expect("abort upload");
        assert!(!temp.exists());

        let id = uuid::Uuid::new_v4();
        upload_runner_file_chunk(
            Some(request(workspace.path(), "expiry-runner", id)),
            &trusted(),
        )
        .expect("create expiry candidate");
        let temp = workspace.path().join(format!(".evidence.bin.{id}.upload"));
        upload_registry()
            .lock()
            .expect("registry")
            .uploads
            .values_mut()
            .find(|upload| upload.temp == temp)
            .expect("expiry candidate")
            .updated_at = Instant::now() - UPLOAD_EXPIRY;
        upload_runner_file_chunk(
            Some(request(
                workspace.path(),
                "expiry-trigger",
                uuid::Uuid::new_v4(),
            )),
            &trusted(),
        )
        .expect("reap expired upload");
        assert!(!temp.exists());
    }
}
