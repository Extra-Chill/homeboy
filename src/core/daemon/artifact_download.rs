use serde_json::json;
use std::fs::{self, File};
use std::io::Write;
use std::net::TcpStream;
use std::path::PathBuf;

use crate::core::artifact_links::{public_artifact_url, viewer_links};
use crate::core::error::{Error, Result};
use crate::core::observation::{ArtifactRecord, ObservationStore};
use crate::core::runner;

use super::{error_response, HttpResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDownload {
    pub record: ArtifactRecord,
    pub path: PathBuf,
    pub content_type: String,
    pub size_bytes: u64,
    pub filename: String,
}

pub(crate) fn route(path: &str) -> Option<HttpResponse> {
    let path_only = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = path_only
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    let result = match segments.as_slice() {
        ["artifacts", artifact_token] => resolve_artifact_download(None, artifact_token),
        ["runs", run_id, "artifacts", "sync"] => artifact_sync_manifest(run_id),
        ["runs", run_id, "artifacts", artifact_token] => {
            resolve_artifact_download(Some(run_id), artifact_token)
        }
        _ => return None,
    };

    Some(match result {
        Ok(ResolvedArtifactResponse::Download(artifact)) => HttpResponse {
            status_code: 200,
            body: artifact_metadata_body(&artifact.record, Some(&artifact)),
            artifact: Some(*artifact),
        },
        Ok(ResolvedArtifactResponse::Manifest(body)) => HttpResponse {
            status_code: 200,
            body,
            artifact: None,
        },
        Err(err) => error_response(404, err),
    })
}

pub(crate) fn write_response(
    mut stream: TcpStream,
    status_code: u16,
    artifact: ArtifactDownload,
) -> std::io::Result<()> {
    let mut file = File::open(&artifact.path)?;
    let status_text = if status_code == 200 {
        "OK"
    } else {
        "Internal Server Error"
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"{}\"\r\nX-Homeboy-Artifact-Id: {}\r\nX-Homeboy-Run-Id: {}\r\nX-Homeboy-Artifact-Kind: {}\r\nConnection: close\r\n",
        status_code,
        status_text,
        artifact.content_type,
        artifact.size_bytes,
        sanitize_header_value(&artifact.filename),
        artifact.record.id,
        artifact.record.run_id,
        sanitize_header_value(&artifact.record.kind),
    )?;
    if let Some(sha256) = &artifact.record.sha256 {
        write!(stream, "X-Homeboy-Artifact-Sha256: {}\r\n", sha256)?;
    }
    write!(stream, "\r\n")?;
    std::io::copy(&mut file, &mut stream)?;
    Ok(())
}

enum ResolvedArtifactResponse {
    Download(Box<ArtifactDownload>),
    Manifest(serde_json::Value),
}

fn resolve_artifact_download(
    expected_run_id: Option<&str>,
    artifact_token: &str,
) -> Result<ResolvedArtifactResponse> {
    let store = ObservationStore::open_initialized()?;
    let artifact = store.get_artifact(artifact_token)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "artifact_id",
            format!("artifact record not found: {artifact_token}"),
            Some(artifact_token.to_string()),
            None,
        )
    })?;

    if let Some(run_id) = expected_run_id {
        if artifact.run_id != run_id {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                "artifact does not belong to requested run",
                Some(artifact_token.to_string()),
                None,
            ));
        }
    }

    if artifact.artifact_type != "file" {
        if artifact.artifact_type == "remote_file"
            || runner::is_remote_runner_artifact_path(&artifact.path)
        {
            let download = runner::download_remote_artifact(&artifact.path, None)?;
            let filename = download
                .output_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&artifact.id)
                .to_string();
            let size_bytes = download
                .size_bytes
                .and_then(|size| u64::try_from(size).ok())
                .or_else(|| {
                    fs::metadata(&download.output_path)
                        .ok()
                        .map(|metadata| metadata.len())
                })
                .unwrap_or(0);
            let content_type = download
                .content_type
                .clone()
                .or_else(|| artifact.mime.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            return Ok(ResolvedArtifactResponse::Download(Box::new(
                ArtifactDownload {
                    record: artifact,
                    path: download.output_path,
                    content_type,
                    size_bytes,
                    filename,
                },
            )));
        }
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a downloadable file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }

    let path = PathBuf::from(&artifact.path);
    let metadata = fs::metadata(&path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read artifact metadata {}", path.display())),
        )
    })?;
    if !metadata.is_file() {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!("registered artifact path is not a file: {}", path.display()),
            Some(artifact.id.clone()),
            None,
        ));
    }

    let content_type = artifact
        .mime
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&artifact.id)
        .to_string();

    Ok(ResolvedArtifactResponse::Download(Box::new(
        ArtifactDownload {
            record: artifact,
            path,
            content_type,
            size_bytes: metadata.len(),
            filename,
        },
    )))
}

fn artifact_sync_manifest(run_id: &str) -> Result<ResolvedArtifactResponse> {
    let store = ObservationStore::open_initialized()?;
    if store.get_run(run_id)?.is_none() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        ));
    }

    let artifacts: Vec<serde_json::Value> = store
        .list_artifacts(run_id)?
        .into_iter()
        .map(|artifact| {
            let public_url = public_artifact_url(&artifact);
            let viewer_links = viewer_links(&artifact, public_url.as_deref());
            json!({
                "id": artifact.id,
                "path_token": artifact.id,
                "run_id": artifact.run_id,
                "kind": artifact.kind,
                "type": artifact.artifact_type,
                "download_path": format!("/runs/{}/artifacts/{}", run_id, artifact.id),
                "public_url": public_url,
                "viewer_links": viewer_links,
                "sha256": artifact.sha256,
                "size_bytes": artifact.size_bytes,
                "mime": artifact.mime,
                "created_at": artifact.created_at,
            })
        })
        .collect();

    Ok(ResolvedArtifactResponse::Manifest(json!({
        "command": "api.runs.artifacts.sync",
        "run_id": run_id,
        "artifacts": artifacts,
    })))
}

fn artifact_metadata_body(
    artifact: &ArtifactRecord,
    download: Option<&ArtifactDownload>,
) -> serde_json::Value {
    json!({
        "command": "api.runs.artifact.download",
        "artifact": artifact,
        "path_token": artifact.id,
        "content_type": download.map(|download| download.content_type.clone()).or_else(|| artifact.mime.clone()),
        "size_bytes": download
            .and_then(|download| i64::try_from(download.size_bytes).ok())
            .or(artifact.size_bytes),
        "sha256": artifact.sha256,
    })
}

fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n' | '"'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::NewRunRecord;
    use crate::test_support::HomeGuard;

    #[test]
    fn test_route() {
        let _home = HomeGuard::new();
        let home_path = std::path::PathBuf::from(std::env::var("HOME").expect("home"));
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                NewRunRecord::builder("runner-exec")
                    .command("homeboy runner exec")
                    .homeboy_version("test-version")
                    .build(),
            )
            .expect("run");
        let artifact_path = home_path.join("artifact.txt");
        fs::write(&artifact_path, "artifact body").expect("artifact file");
        let artifact = store
            .record_artifact(&run.id, "lab_fix_patch", &artifact_path)
            .expect("artifact");

        let response =
            route(&format!("/runs/{}/artifacts/{}", run.id, artifact.id)).expect("artifact route");

        assert_eq!(response.status_code, 200);
        assert!(response.artifact.is_some());
        assert_eq!(response.body["artifact"]["id"], artifact.id);
    }

    #[test]
    fn test_sanitize_header_value_removes_response_splitting_chars() {
        assert_eq!(sanitize_header_value("a\r\nb\"c"), "abc");
    }

    #[test]
    fn test_write_response() {
        let _home = HomeGuard::new();
        let artifact_path = tempfile::NamedTempFile::new().expect("artifact file");
        fs::write(artifact_path.path(), "artifact body").expect("artifact body");
        let artifact = ArtifactDownload {
            record: ArtifactRecord {
                id: "artifact-1".to_string(),
                run_id: "run-1".to_string(),
                kind: "lab_fix_patch".to_string(),
                artifact_type: "file".to_string(),
                path: artifact_path.path().display().to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: Some("abc123".to_string()),
                size_bytes: Some(13),
                mime: Some("text/plain".to_string()),
                metadata_json: serde_json::json!({}),
                created_at: chrono::Utc::now().to_rfc3339(),
            },
            path: artifact_path.path().to_path_buf(),
            content_type: "text/plain".to_string(),
            size_bytes: 13,
            filename: "artifact.txt".to_string(),
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let reader = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(addr).expect("connect");
            let mut body = String::new();
            std::io::Read::read_to_string(&mut stream, &mut body).expect("read response");
            body
        });
        let (stream, _) = listener.accept().expect("accept");

        write_response(stream, 200, artifact).expect("write response");
        let response = reader.join().expect("reader");

        assert!(response.contains("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: text/plain"));
        assert!(response.ends_with("artifact body"));
    }
}

#[cfg(test)]
#[path = "../../../tests/core/daemon/artifact_download_test.rs"]
mod artifact_download_test;
