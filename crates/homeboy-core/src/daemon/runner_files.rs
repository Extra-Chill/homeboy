//! Runner-file HTTP operations for the daemon: create workspace file
//! directories, upload/download runner files, and resolve/normalize workspace
//! paths safely within the runner root. Extracted from the `daemon` god file.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};
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

/// Versioned contract advertised before a controller chooses the chunk protocol.
///
/// The upload endpoint is intentionally not an implicit feature probe: callers
/// must see this capability before they materialize a workspace.  That keeps an
/// older reverse broker from failing after it has already created remote state.
pub(super) const CHUNK_UPLOAD_PROTOCOL_VERSION: u64 = 1;

#[derive(Clone, Hash, Eq, PartialEq)]
struct UploadKey {
    runner_id: String,
    workspace_root: Option<String>,
    upload_id: uuid::Uuid,
}

struct PendingUpload {
    temp: PathBuf,
    record: PathBuf,
    destination: PathBuf,
    file: fs::File,
    size_bytes: u64,
    reserved_bytes: u64,
    updated_at: Instant,
}

#[derive(Serialize, Deserialize)]
struct UploadRecord {
    version: u8,
    runner_id: String,
    workspace_root: String,
    destination: String,
    upload_id: String,
    device: u64,
    inode: u64,
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
        let _ = fs::remove_file(&upload.record);
        false
    });
}

pub(super) fn reap_expired_uploads() {
    recover_expired_uploads(SystemTime::now());
    let mut registry = upload_registry().lock().expect("upload registry lock");
    reap_expired_uploads_locked(&mut registry);
}

/// Partial bytes and their durable ownership records live under daemon state,
/// never inside a caller-controlled workspace. Restart recovery treats records
/// as cleanup-only and validates each record before removing its owned payload.
fn recover_expired_uploads(now: SystemTime) {
    let Ok(root) = upload_staging_root() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let record_path = entry.path();
        if record_path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&record_path) else {
            continue;
        };
        if !metadata.file_type().is_file()
            || metadata
                .modified()
                .ok()
                .and_then(|mtime| now.duration_since(mtime).ok())
                .is_none_or(|age| age < UPLOAD_EXPIRY)
        {
            continue;
        }
        let Ok(record) = fs::read(&record_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<UploadRecord>(&bytes).ok())
            .ok_or(())
        else {
            continue;
        };
        let Ok(upload_id) = uuid::Uuid::parse_str(&record.upload_id) else {
            continue;
        };
        if record.version != 1
            || record_path.file_stem().and_then(|name| name.to_str()) != Some(&record.upload_id)
            || !valid_upload_destination(&record)
        {
            continue;
        }
        let payload = root.join(format!("{upload_id}.payload"));
        let Ok(payload_metadata) = fs::symlink_metadata(&payload) else {
            continue;
        };
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        if !payload_metadata.file_type().is_file() || payload_metadata.len() > MAX_UPLOAD_BYTES || {
            #[cfg(unix)]
            {
                payload_metadata.dev() != record.device || payload_metadata.ino() != record.inode
            }
            #[cfg(not(unix))]
            {
                false
            }
        } {
            continue;
        }
        let _ = fs::remove_file(&payload);
        let _ = fs::remove_file(&record_path);
    }
}

fn upload_staging_root() -> Result<PathBuf> {
    let state = crate::paths::daemon_state_file()?;
    let root = state
        .parent()
        .ok_or_else(|| Error::internal_unexpected("daemon state has no parent"))?
        .join("runner-file-uploads");
    fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
    }
    Ok(root)
}

fn valid_upload_destination(record: &UploadRecord) -> bool {
    if record.runner_id.is_empty() || record.workspace_root.is_empty() {
        return false;
    }
    let Ok(root) = fs::canonicalize(&record.workspace_root) else {
        return false;
    };
    let destination = canonicalize_existing_prefix(&normalize_path(Path::new(&record.destination)));
    destination.starts_with(root)
}

pub(super) fn upload_capabilities() -> serde_json::Value {
    json!({
        "protocol_version": CHUNK_UPLOAD_PROTOCOL_VERSION,
        "capabilities": ["private_file_chunk_upload"],
        "max_upload_bytes": MAX_UPLOAD_BYTES,
        "max_chunk_bytes": MAX_CHUNK_BYTES,
    })
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
    // A restart loses the in-memory registry. Recover only daemon-private
    // record-backed uploads before accepting a new chunk upload.
    recover_expired_uploads(SystemTime::now());
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
    let key = UploadKey {
        runner_id: request.runner_id.clone(),
        workspace_root: request.workspace_root.clone(),
        upload_id,
    };
    let mut registry = upload_registry().lock().expect("upload registry lock");
    reap_expired_uploads_locked(&mut registry);
    if let Some(upload) = registry.uploads.get(&key) {
        if upload.destination != path {
            return Err(Error::validation_invalid_argument(
                "path",
                "runner file upload id is already bound to a different destination",
                Some(path.display().to_string()),
                None,
            ));
        }
    }
    let current = registry
        .uploads
        .get(&key)
        .map(|upload| upload.size_bytes)
        .unwrap_or(0);
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
        let (temp, record, file) = create_upload_file(
            &request.runner_id,
            request.workspace_root.as_deref(),
            &path,
            upload_id,
        )?;
        registry.uploads.insert(
            key.clone(),
            PendingUpload {
                temp,
                record,
                destination: path.clone(),
                file,
                size_bytes: current,
                reserved_bytes: request.size_bytes,
                updated_at: Instant::now(),
            },
        );
    }
    let upload = registry.uploads.get_mut(&key).expect("upload was inserted");
    if let Err(error) = upload
        .file
        .write_all(&content)
        .and_then(|_| upload.file.sync_all())
    {
        let temp = upload.temp.clone();
        let _ = fs::remove_file(&temp);
        registry.uploads.remove(&key);
        return Err(Error::internal_io(
            error.to_string(),
            Some(temp.display().to_string()),
        ));
    }
    let size = current + content.len() as u64;
    let upload = registry.uploads.get_mut(&key).expect("upload was inserted");
    upload.size_bytes = size;
    upload.updated_at = Instant::now();
    write_upload_record(
        upload,
        &request.runner_id,
        request.workspace_root.as_deref(),
        upload_id,
    )?;
    if request.final_chunk {
        let expected = request.sha256.as_deref().expect("validated before write");
        let upload = registry.uploads.get_mut(&key).expect("upload was inserted");
        let temp = upload.temp.clone();
        let digest_matches = sha256_open_file(&mut upload.file)
            .map(|actual| actual == expected)
            .unwrap_or(false);
        if size != request.size_bytes || !digest_matches {
            let _ = fs::remove_file(&temp);
            let _ = fs::remove_file(&upload.record);
            registry.uploads.remove(&key);
            return Err(Error::validation_invalid_argument(
                "provider_evidence",
                "runner evidence chunk upload does not match its declared digest or size",
                Some(path.display().to_string()),
                None,
            ));
        }
        if let Err(error) = publish_upload_file(upload) {
            let _ = fs::remove_file(&temp);
            let _ = fs::remove_file(&upload.record);
            registry.uploads.remove(&key);
            return Err(Error::internal_io(
                error.to_string(),
                Some(path.display().to_string()),
            ));
        }
        let _ = fs::remove_file(&upload.record);
        registry.uploads.remove(&key);
    }
    Ok(
        json!({"runner_id": request.runner_id, "path": path.display().to_string(), "size_bytes": size, "final": request.final_chunk}),
    )
}

fn create_upload_file(
    runner_id: &str,
    workspace_root: Option<&str>,
    destination: &Path,
    upload_id: uuid::Uuid,
) -> Result<(PathBuf, PathBuf, fs::File)> {
    let root = upload_staging_root()?;
    let temp = root.join(format!("{upload_id}.payload"));
    let record = root.join(format!("{upload_id}.json"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    let file = options.open(&temp).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", temp.display())),
        )
    })?;
    if !file
        .metadata()
        .map_err(|error| Error::internal_io(error.to_string(), Some(temp.display().to_string())))?
        .is_file()
    {
        let _ = fs::remove_file(&temp);
        return Err(Error::validation_invalid_argument(
            "path",
            "runner upload temporary path is not a regular file",
            Some(temp.display().to_string()),
            None,
        ));
    }
    let pending = PendingUpload {
        temp: temp.clone(),
        record: record.clone(),
        destination: destination.to_path_buf(),
        file: file.try_clone().map_err(|error| {
            Error::internal_io(error.to_string(), Some(temp.display().to_string()))
        })?,
        size_bytes: 0,
        reserved_bytes: 0,
        updated_at: Instant::now(),
    };
    write_upload_record(&pending, runner_id, workspace_root, upload_id)?;
    Ok((temp, record, file))
}

fn write_upload_record(
    upload: &PendingUpload,
    runner_id: &str,
    workspace_root: Option<&str>,
    upload_id: uuid::Uuid,
) -> Result<()> {
    let workspace_root = workspace_root
        .map(PathBuf::from)
        .or_else(|| upload.destination.parent().map(Path::to_path_buf))
        .and_then(|path| fs::canonicalize(path).ok())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace_root",
                "runner upload requires a canonical workspace root",
                None,
                None,
            )
        })?;
    let metadata = upload.file.metadata().map_err(|error| {
        Error::internal_io(error.to_string(), Some(upload.temp.display().to_string()))
    })?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let record = UploadRecord {
        version: 1,
        runner_id: runner_id.to_string(),
        workspace_root: workspace_root.display().to_string(),
        destination: upload.destination.display().to_string(),
        upload_id: upload_id.to_string(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(not(unix))]
        device: 0,
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(not(unix))]
        inode: 0,
    };
    let temporary = upload.record.with_extension("json.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
        })?;
    file.write_all(&serde_json::to_vec(&record).expect("upload record serializes"))
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
        })?;
    fs::rename(&temporary, &upload.record).map_err(|error| {
        Error::internal_io(error.to_string(), Some(upload.record.display().to_string()))
    })
}

fn sha256_open_file(file: &mut fs::File) -> std::io::Result<String> {
    use sha2::Digest;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::End(0))?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn publish_upload_file(upload: &PendingUpload) -> std::io::Result<()> {
    // Copy from the verified daemon-private descriptor into a fresh workspace
    // descriptor. The workspace only sees a publication temporary after the
    // upload is complete; the long-lived/restart-reaped state stays private.
    let metadata = upload.file.metadata()?;
    let staged = fs::symlink_metadata(&upload.temp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !staged.file_type().is_file()
            || staged.dev() != metadata.dev()
            || staged.ino() != metadata.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "upload staging path changed identity",
            ));
        }
    }
    let parent = upload.destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "upload destination has no parent",
        )
    })?;
    let temp = parent.join(format!(
        ".{}.{}.publish",
        upload
            .destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("upload"),
        uuid::Uuid::new_v4()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    let mut published = options.open(&temp)?;
    let mut source = upload.file.try_clone()?;
    source.seek(SeekFrom::Start(0))?;
    std::io::copy(&mut source, &mut published)?;
    published.sync_all()?;
    publish_upload_file_in_workspace(&PendingUpload {
        temp,
        record: upload.record.clone(),
        destination: upload.destination.clone(),
        file: published,
        size_bytes: upload.size_bytes,
        reserved_bytes: upload.reserved_bytes,
        updated_at: upload.updated_at,
    })
}

fn publish_upload_file_in_workspace(upload: &PendingUpload) -> std::io::Result<()> {
    let metadata = upload.file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "upload descriptor is not a regular file",
        ));
    }
    // The path still names the descriptor we wrote and hashed; refuse a swap before publish.
    // On Unix the final rename is performed through a checked parent descriptor.
    // This fails closed when the parent is not owned by this daemon user or is
    // writable by another user. That is the threat boundary for platforms
    // without a no-replace rename primitive: an attacker with write access to
    // the destination directory is outside the supported publication model.
    let path_metadata = fs::symlink_metadata(&upload.temp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if path_metadata.dev() != metadata.dev() || path_metadata.ino() != metadata.ino() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "upload temporary path changed identity",
            ));
        }
    }
    if !path_metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "upload temporary path is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::AsRawFd;

        let parent = upload.destination.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "upload destination has no parent",
            )
        })?;
        let parent_file = fs::File::open(parent)?;
        let parent_metadata = parent_file.metadata()?;
        if parent_metadata.uid() != unsafe { libc::geteuid() }
            || parent_metadata.mode() & 0o022 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "upload parent must be daemon-owned and not group/world writable",
            ));
        }
        let temp_name = CString::new(
            upload
                .temp
                .file_name()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "upload temporary path has no name",
                    )
                })?
                .as_bytes(),
        )
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "upload temporary path contains NUL",
            )
        })?;
        let destination_name = CString::new(
            upload
                .destination
                .file_name()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "upload destination has no name",
                    )
                })?
                .as_bytes(),
        )
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "upload destination contains NUL",
            )
        })?;
        // Recheck through the same directory descriptor used for publication,
        // closing the lookup-then-rename swap window in the path-based flow.
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe {
            libc::fstatat(
                parent_file.as_raw_fd(),
                temp_name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
            || stat.st_dev as u64 != metadata.dev()
            || stat.st_ino as u64 != metadata.ino()
            || (stat.st_mode & libc::S_IFMT) != libc::S_IFREG
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "upload temporary path changed identity before publish",
            ));
        }
        if unsafe {
            libc::renameat(
                parent_file.as_raw_fd(),
                temp_name.as_ptr(),
                parent_file.as_raw_fd(),
                destination_name.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut published = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe {
            libc::fstatat(
                parent_file.as_raw_fd(),
                destination_name.as_ptr(),
                &mut published,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
            || published.st_dev as u64 != metadata.dev()
            || published.st_ino as u64 != metadata.ino()
        {
            // A post-publish mismatch is fail-closed. Do not path-unlink here:
            // an attacker could replace the destination between verification
            // and unlink, so preserving the suspect entry is safer than a
            // rollback that might delete an unrelated file.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "published upload changed identity",
            ));
        }
        return Ok(());
    }
    #[cfg(not(unix))]
    fs::rename(&upload.temp, &upload.destination)
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
        let temp = upload_staging_root()
            .expect("staging")
            .join(format!("{id}.payload"));
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
        let temp = upload_staging_root()
            .expect("staging")
            .join(format!("{id}.payload"));
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

    #[cfg(unix)]
    #[test]
    fn chunk_upload_refuses_a_predictable_temp_symlink() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let runner = format!("chunk-symlink-{}", uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4();
        let victim = workspace.path().join("victim");
        fs::write(&victim, b"protected").expect("victim");
        symlink(
            &victim,
            upload_staging_root()
                .expect("staging")
                .join(format!("{id}.payload")),
        )
        .expect("plant symlink");

        assert!(
            upload_runner_file_chunk(Some(request(workspace.path(), &runner, id)), &trusted())
                .is_err()
        );
        assert_eq!(fs::read(&victim).expect("victim remains"), b"protected");
    }

    #[test]
    fn chunk_upload_binds_its_destination_and_cleans_only_its_recorded_file() {
        let workspace = tempfile::tempdir().expect("workspace");
        let runner = format!("chunk-binding-{}", uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4();
        upload_runner_file_chunk(Some(request(workspace.path(), &runner, id)), &trusted())
            .expect("start upload");
        let temp = upload_staging_root()
            .expect("staging")
            .join(format!("{id}.payload"));
        let other = workspace.path().join("other.bin");
        fs::write(&other, b"unrelated").expect("unrelated file");

        let mut switched = request(workspace.path(), &runner, id);
        switched["path"] = json!("other.bin");
        let error = upload_runner_file_chunk(Some(switched), &trusted())
            .expect_err("upload id cannot switch destinations");
        assert_eq!(error.details["field"], "path");
        abort_runner_file_chunk_upload(
            Some(json!({"runner_id": runner, "workspace_root": workspace.path().display().to_string(), "upload_id": id})),
            &trusted(),
        )
        .expect("abort recorded upload");
        assert!(!temp.exists());
        assert_eq!(fs::read(other).expect("unrelated remains"), b"unrelated");
    }

    #[test]
    fn restart_recovery_reaps_only_private_recorded_uploads() {
        let workspace = tempfile::tempdir().expect("workspace");
        let upload_id = uuid::Uuid::new_v4();
        let (upload, record, _file) = create_upload_file(
            "restart-runner",
            Some(&workspace.path().display().to_string()),
            &workspace.path().join("evidence.bin"),
            upload_id,
        )
        .expect("private staged upload");
        let unrelated = workspace
            .path()
            .join(format!(".evidence.bin.{upload_id}.upload"));
        fs::write(&unrelated, b"keep").expect("unrelated file");
        // Supplying a future observation time models a daemon restart after the
        // idle window without relying on process-global clock mutation.
        recover_expired_uploads(SystemTime::now() + UPLOAD_EXPIRY + Duration::from_secs(1));
        assert!(!upload.exists());
        assert!(!record.exists());
        assert_eq!(fs::read(unrelated).expect("unrelated survives"), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn publish_refuses_a_temp_path_swapped_after_descriptor_validation_begins() {
        let workspace = tempfile::tempdir().expect("workspace");
        let temp = workspace.path().join(".evidence.bin.upload");
        let destination = workspace.path().join("evidence.bin");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .expect("create staged upload");
        fs::remove_file(&temp).expect("remove staged name");
        fs::write(&temp, b"attacker replacement").expect("replace staged name");
        let upload = PendingUpload {
            temp,
            record: workspace.path().join("upload.json"),
            destination: destination.clone(),
            file,
            size_bytes: 0,
            reserved_bytes: 0,
            updated_at: Instant::now(),
        };
        assert!(publish_upload_file(&upload).is_err());
        assert!(!destination.exists());
    }
}
