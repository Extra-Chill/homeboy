use std::fs;
use std::path::PathBuf;

use base64::Engine;
use reqwest::blocking::Client;
use reqwest::header;
use serde_json::Value;

use crate::core::error::{Error, Result};
use crate::core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use crate::core::paths;

use super::super::execution::{canonical_daemon_body, daemon_api_get};
use super::super::{load, status, RunnerArtifactRef, RunnerTunnelMode};
use super::tokens::RemoteArtifactToken;

#[derive(Debug)]
pub struct RemoteArtifactDownload {
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub artifact_ref: RunnerArtifactRef,
}

pub fn download_remote_artifact(
    path: &str,
    output: Option<PathBuf>,
) -> Result<RemoteArtifactDownload> {
    let token = RemoteArtifactToken::parse(path)?;
    if let Some(download) = download_direct_runner_artifact(&token, output.clone())? {
        return Ok(download);
    }

    let data = daemon_api_get(
        &token.runner_id,
        &format!(
            "/runs/{}/artifacts/{}/content",
            encode_uri_component(&token.run_id),
            encode_uri_component(&token.artifact_id)
        ),
    )?;
    let body = canonical_daemon_body(&data, "runner artifact response")?;
    let content_base64 = body
        .get("content_base64")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::internal_unexpected("runner artifact response missing content"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64)
        .map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("decode runner artifact content".to_string()),
            )
        })?;
    let file_name = body
        .get("filename")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(&token.artifact_id);
    let output_path = output.unwrap_or_else(|| {
        paths::artifact_root()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("runner")
            .join(&token.runner_id)
            .join(&token.run_id)
            .join(file_name)
    });
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    fs::write(&output_path, bytes).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("write runner artifact {}", output_path.display())),
        )
    })?;
    Ok(RemoteArtifactDownload {
        output_path,
        content_type: body.get("mime").and_then(Value::as_str).map(str::to_string),
        size_bytes: body.get("size_bytes").and_then(Value::as_i64),
        sha256: body
            .get("sha256")
            .and_then(Value::as_str)
            .map(str::to_string),
        artifact_ref: RunnerArtifactRef {
            artifact_id: token.artifact_id.clone(),
            name: Some(file_name.to_string()),
            path: Some(EXECUTION_CONTRACT.artifacts.runner_artifact_ref(
                &token.runner_id,
                &token.run_id,
                &token.artifact_id,
            )),
            url: None,
            mime: body.get("mime").and_then(Value::as_str).map(str::to_string),
            size_bytes: body.get("size_bytes").and_then(Value::as_u64),
            sha256: body
                .get("sha256")
                .and_then(Value::as_str)
                .map(str::to_string),
            transport: Some("daemon".to_string()),
        },
    })
}

fn download_direct_runner_artifact(
    token: &RemoteArtifactToken,
    output: Option<PathBuf>,
) -> Result<Option<RemoteArtifactDownload>> {
    let runner = load(&token.runner_id)?;
    let connected = status(&token.runner_id)?;
    let Some(session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        ));
    };

    if session.mode != RunnerTunnelMode::DirectSsh {
        return Ok(None);
    }

    let Some(local_url) = session.local_url.as_deref() else {
        return Ok(None);
    };

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    let path = format!(
        "/runs/{}/artifacts/{}/content",
        encode_uri_component(&token.run_id),
        encode_uri_component(&token.artifact_id)
    );
    let response = client
        .get(format!("{}{}", local_url.trim_end_matches('/'), path))
        .send()
        .map_err(|err| Error::internal_unexpected(format!("query runner daemon: {err}")))?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "runner artifact fetch failed with HTTP {}: {}",
                status.as_u16(),
                body
            ),
            Some(token.artifact_id.clone()),
            None,
        ));
    }

    let bytes = response.bytes().map_err(|err| {
        Error::internal_unexpected(format!("read runner artifact response: {err}"))
    })?;
    let filename = content_disposition_filename(&headers)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| token.artifact_id.clone());
    let output_path = output.unwrap_or_else(|| {
        paths::artifact_root()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("runner")
            .join(&token.runner_id)
            .join(&token.run_id)
            .join(&filename)
    });
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("create {}", parent.display())),
            )
        })?;
    }
    let size_bytes = i64::try_from(bytes.len()).ok();
    fs::write(&output_path, bytes).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("write runner artifact {}", output_path.display())),
        )
    })?;

    Ok(Some(RemoteArtifactDownload {
        output_path,
        content_type: header_string(&headers, header::CONTENT_TYPE.as_str()),
        size_bytes,
        sha256: header_string(&headers, "x-homeboy-artifact-sha256"),
        artifact_ref: RunnerArtifactRef {
            artifact_id: token.artifact_id.clone(),
            name: Some(filename),
            path: Some(EXECUTION_CONTRACT.artifacts.runner_artifact_ref(
                &token.runner_id,
                &token.run_id,
                &token.artifact_id,
            )),
            url: None,
            mime: header_string(&headers, header::CONTENT_TYPE.as_str()),
            size_bytes: size_bytes.and_then(|value| u64::try_from(value).ok()),
            sha256: header_string(&headers, "x-homeboy-artifact-sha256"),
            transport: Some("direct_daemon".to_string()),
        },
    }))
}

fn header_string(headers: &header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

pub(super) fn content_disposition_filename(headers: &header::HeaderMap) -> Option<String> {
    let value = header_string(headers, header::CONTENT_DISPOSITION.as_str())?;
    value.split(';').find_map(|part| {
        let part = part.trim();
        let filename = part.strip_prefix("filename=")?;
        Some(filename.trim_matches('"').to_string())
    })
}
