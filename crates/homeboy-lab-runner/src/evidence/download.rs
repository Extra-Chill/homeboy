use std::fs;
use std::path::PathBuf;

use base64::Engine;
use reqwest::blocking::Client;
use reqwest::header;
use serde_json::Value;

use homeboy_core::error::{Error, Result};
use homeboy_core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use homeboy_core::paths;
use homeboy_core::runner_download_cache::{
    record_download_intent, resolve_runner_download_target, sanitize_artifact_file_name,
    RunnerDownloadIntent,
};

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

/// Fetch a runner artifact on an operator's behalf.
///
/// The default location is tagged [`RunnerDownloadIntent::OperatorPull`], which
/// is the fail-closed reading of an untyped call: `runs artifact get`,
/// `runs artifacts --pull`, the HTTP artifact endpoint, and every provider-trait
/// consumer land here, and all of them hand the resulting path back to a human.
/// Homeboy's own fetches must say so explicitly via
/// [`download_remote_artifact_with_intent`].
pub fn download_remote_artifact(
    path: &str,
    output: Option<PathBuf>,
) -> Result<RemoteArtifactDownload> {
    download_remote_artifact_with_intent(path, output, RunnerDownloadIntent::OperatorPull)
}

/// Fetch a runner artifact and record why (#10585).
///
/// The intent is persisted beside the bytes so `cleanup --include
/// runner-downloads` can tell an operator's review copy from a transient
/// internal fetch. It is only recorded for the default cache layout: a caller
/// supplying an explicit `output` owns that location and homeboy does not
/// reclaim it.
pub fn download_remote_artifact_with_intent(
    path: &str,
    output: Option<PathBuf>,
    intent: RunnerDownloadIntent,
) -> Result<RemoteArtifactDownload> {
    let token = RemoteArtifactToken::parse(path)?;
    if let Some(download) = download_direct_runner_artifact(&token, output.clone(), intent)? {
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
    let remote_file_name = body
        .get("filename")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty());
    // The remote controls `filename` outright, and `token.runner_id` /
    // `token.run_id` are percent-decoded copies of remote-supplied strings.
    // Nothing below is joined until this has proven all three (#10586).
    let placement = resolve_placement(&token, output, remote_file_name)?;
    let output_path = placement.output_path;
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
    if let Some(cache_dir) = &placement.cache_dir {
        record_download_intent(cache_dir, intent, &token.artifact_id);
    }
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
            name: Some(placement.file_name.clone()),
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

/// Where one download's bytes may be written, and what to tag afterwards.
///
/// Produced only by [`resolve_placement`], so no transport can build an output
/// path without going through the containment check.
pub(super) struct DownloadPlacement {
    pub(super) output_path: PathBuf,
    /// The cache directory to tag with the caller's intent, or `None` when the
    /// caller supplied an explicit `output` and owns that location itself.
    pub(super) cache_dir: Option<PathBuf>,
    /// The name actually written. Reported back as the artifact-ref name so
    /// reported metadata can never disagree with the bytes on disk — a
    /// downstream consumer that rebuilds a path from the name inherits the
    /// sanitized form, not the remote's.
    pub(super) file_name: String,
}

pub(super) fn resolve_placement(
    token: &RemoteArtifactToken,
    output: Option<PathBuf>,
    remote_file_name: Option<&str>,
) -> Result<DownloadPlacement> {
    if let Some(output_path) = output {
        // An explicit destination is the caller's own; it never enters the
        // cache layout and is never tagged.
        let file_name = output_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| sanitize_artifact_file_name(&token.artifact_id));
        return Ok(DownloadPlacement {
            output_path,
            cache_dir: None,
            file_name,
        });
    }
    // Fail closed. This used to be `.unwrap_or_else(|_| PathBuf::from("."))`,
    // which silently relocated the whole cache into the process working
    // directory — neither contained nor predictable — whenever the artifact
    // root could not be resolved.
    let artifact_root = paths::artifact_root()?;
    let target = resolve_runner_download_target(
        &artifact_root,
        &token.runner_id,
        &token.run_id,
        remote_file_name,
        &token.artifact_id,
    )?;
    Ok(DownloadPlacement {
        output_path: target.file_path,
        cache_dir: Some(target.cache_dir),
        file_name: target.file_name,
    })
}

fn download_direct_runner_artifact(
    token: &RemoteArtifactToken,
    output: Option<PathBuf>,
    intent: RunnerDownloadIntent,
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
    // `Content-Disposition` is parsed by splitting on `;` and trimming quotes,
    // so the value reaching here is whatever the daemon sent, `../` included.
    let remote_file_name = content_disposition_filename(&headers).filter(|name| !name.is_empty());
    let placement = resolve_placement(token, output, remote_file_name.as_deref())?;
    let output_path = placement.output_path;
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
    if let Some(cache_dir) = &placement.cache_dir {
        record_download_intent(cache_dir, intent, &token.artifact_id);
    }

    Ok(Some(RemoteArtifactDownload {
        output_path,
        content_type: header_string(&headers, header::CONTENT_TYPE.as_str()),
        size_bytes,
        sha256: header_string(&headers, "x-homeboy-artifact-sha256"),
        artifact_ref: RunnerArtifactRef {
            artifact_id: token.artifact_id.clone(),
            name: Some(placement.file_name.clone()),
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
