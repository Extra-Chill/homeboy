//! Runner-file HTTP operations for the daemon: create workspace file
//! directories, upload/download runner files, and resolve/normalize workspace
//! paths safely within the runner root. Extracted from the `daemon` god file.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::json;

use super::remote_runner;
use super::runner_workspace_root;
use super::{FilePathRequest, FileUploadRequest};
use crate::broker_auth::BrokerScope;
use crate::error::{Error, Result};

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
    fs::write(&path, &content).map_err(|err| {
        Error::internal_io(err.to_string(), Some(format!("write {}", path.display())))
    })?;
    Ok(json!({
        "runner_id": request.runner_id,
        "path": path.display().to_string(),
        "size_bytes": content.len(),
    }))
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
