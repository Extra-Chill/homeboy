use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::core::{daemon, paths, Error, Result};

#[derive(Debug, Clone)]
pub struct ArtifactOriginServeSpec {
    pub bind: String,
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactOriginStatus {
    pub command: &'static str,
    pub bind: String,
    pub root: String,
    pub public_base_url_shape: String,
    pub cors: bool,
}

pub fn serve(spec: ArtifactOriginServeSpec) -> Result<ArtifactOriginStatus> {
    let root = spec.root.unwrap_or(paths::artifact_root()?);
    let listener = TcpListener::bind(&spec.bind)
        .map_err(|err| Error::internal_io(err.to_string(), Some(spec.bind.clone())))?;
    eprintln!(
        "homeboy artifact origin serving {} on {}",
        root.display(),
        spec.bind
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.clone();
                std::thread::spawn(move || {
                    if let Err(err) = handle_stream(stream, &root) {
                        eprintln!("homeboy artifact origin connection error: {}", err.message);
                    }
                });
            }
            Err(err) => return Err(Error::internal_io(err.to_string(), Some(spec.bind))),
        }
    }
    Ok(status(spec.bind, root))
}

pub fn status(bind: String, root: PathBuf) -> ArtifactOriginStatus {
    ArtifactOriginStatus {
        command: "tunnel.artifact_origin.serve",
        bind,
        root: root.display().to_string(),
        public_base_url_shape: "https://<public-host>".to_string(),
        cors: true,
    }
}

fn handle_stream(mut stream: TcpStream, root: &Path) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("clone artifact origin stream".to_string()),
        )
    })?);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read request line".to_string()))
    })?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or("/");
    let response = response_for_request(root, method, target)?;
    write!(
        stream,
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    )
    .map_err(|err| Error::internal_io(err.to_string(), Some("write status".to_string())))?;
    for (name, value) in response.headers {
        write!(stream, "{}: {}\r\n", name, value)
            .map_err(|err| Error::internal_io(err.to_string(), Some("write header".to_string())))?;
    }
    write!(stream, "connection: close\r\n\r\n")
        .map_err(|err| Error::internal_io(err.to_string(), Some("write headers".to_string())))?;
    stream
        .write_all(&response.body)
        .map_err(|err| Error::internal_io(err.to_string(), Some("write body".to_string())))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactOriginResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn response_for_request(root: &Path, method: &str, target: &str) -> Result<ArtifactOriginResponse> {
    let mut headers = cors_headers();
    if method.eq_ignore_ascii_case("OPTIONS") {
        headers.push(("content-length".to_string(), "0".to_string()));
        return Ok(ArtifactOriginResponse {
            status: 204,
            headers,
            body: Vec::new(),
        });
    }
    if !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD") {
        let body = br#"{"error":"method_not_allowed"}"#.to_vec();
        headers.push(("content-type".to_string(), "application/json".to_string()));
        headers.push(("content-length".to_string(), body.len().to_string()));
        return Ok(ArtifactOriginResponse {
            status: 405,
            headers,
            body,
        });
    }
    if let Some(response) = artifact_route_response(method, target, headers.clone())? {
        return Ok(response);
    }
    let Some(path) = safe_target_path(root, target) else {
        let body = br#"{"error":"not_found"}"#.to_vec();
        headers.push(("content-type".to_string(), "application/json".to_string()));
        headers.push(("content-length".to_string(), body.len().to_string()));
        return Ok(ArtifactOriginResponse {
            status: 404,
            headers,
            body,
        });
    };
    let body = match std::fs::read(&path) {
        Ok(body) => body,
        Err(_) => br#"{"error":"not_found"}"#.to_vec(),
    };
    let status = if path.is_file() { 200 } else { 404 };
    headers.push((
        "content-type".to_string(),
        crate::core::artifact_metadata::content_type_from_path(&path)
            .unwrap_or_else(|| "application/octet-stream".to_string()),
    ));
    headers.push(("content-length".to_string(), body.len().to_string()));
    Ok(ArtifactOriginResponse {
        status,
        headers,
        body: if method.eq_ignore_ascii_case("HEAD") {
            Vec::new()
        } else {
            body
        },
    })
}

fn cors_headers() -> Vec<(String, String)> {
    vec![
        ("access-control-allow-origin".to_string(), "*".to_string()),
        (
            "access-control-allow-methods".to_string(),
            "GET, HEAD, OPTIONS".to_string(),
        ),
        ("access-control-allow-headers".to_string(), "*".to_string()),
    ]
}

fn safe_target_path(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target
        .split('?')
        .next()
        .unwrap_or(target)
        .trim_start_matches('/');
    let relative = PathBuf::from(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(root.join(relative))
}

fn artifact_route_response(
    method: &str,
    target: &str,
    mut headers: Vec<(String, String)>,
) -> Result<Option<ArtifactOriginResponse>> {
    let Some(response) = daemon::artifact_response_for_path(target) else {
        return Ok(None);
    };
    let Some(artifact) = response.artifact else {
        let body = serde_json::to_vec(&response.body).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize artifact route response".to_string()),
            )
        })?;
        headers.push(("content-type".to_string(), "application/json".to_string()));
        headers.push(("content-length".to_string(), body.len().to_string()));
        return Ok(Some(ArtifactOriginResponse {
            status: response.status_code,
            headers,
            body: if method.eq_ignore_ascii_case("HEAD") {
                Vec::new()
            } else {
                body
            },
        }));
    };

    let body = std::fs::read(&artifact.path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read routed artifact".to_string()))
    })?;
    headers.push(("content-type".to_string(), artifact.content_type));
    headers.push(("content-length".to_string(), body.len().to_string()));
    headers.push((
        "content-disposition".to_string(),
        format!(
            "attachment; filename=\"{}\"",
            sanitize_header_value(&artifact.filename)
        ),
    ));
    headers.push(("x-homeboy-artifact-id".to_string(), artifact.record.id));
    headers.push(("x-homeboy-run-id".to_string(), artifact.record.run_id));
    headers.push((
        "x-homeboy-artifact-kind".to_string(),
        sanitize_header_value(&artifact.record.kind),
    ));
    if let Some(sha256) = artifact.record.sha256 {
        headers.push(("x-homeboy-artifact-sha256".to_string(), sha256));
    }
    Ok(Some(ArtifactOriginResponse {
        status: response.status_code,
        headers,
        body: if method.eq_ignore_ascii_case("HEAD") {
            Vec::new()
        } else {
            body
        },
    }))
}

fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n' | '"'))
        .collect()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        405 => "Method Not Allowed",
        _ => "Not Found",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::{NewRunRecord, ObservationStore};

    #[test]
    fn serves_json_with_cors_headers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp
            .path()
            .join("homeboy/workflow-bench/runs/run/artifacts/blueprint.after.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{}").expect("write artifact");

        let response = response_for_request(
            temp.path(),
            "GET",
            "/homeboy/workflow-bench/runs/run/artifacts/blueprint.after.json",
        )
        .expect("response");

        assert_eq!(response.status, 200);
        assert!(response
            .headers
            .contains(&("access-control-allow-origin".to_string(), "*".to_string())));
        assert!(response
            .headers
            .contains(&("content-type".to_string(), "application/json".to_string())));
    }

    #[test]
    fn handles_playground_preflight_without_file_read() {
        let temp = tempfile::tempdir().expect("tempdir");
        let response = response_for_request(
            temp.path(),
            "OPTIONS",
            "/homeboy/workflow-bench/runs/run/artifacts/blueprint.after.json",
        )
        .expect("response");

        assert_eq!(response.status, 204);
        assert!(response.headers.contains(&(
            "access-control-allow-methods".to_string(),
            "GET, HEAD, OPTIONS".to_string()
        )));
    }

    #[test]
    fn serves_run_scoped_artifact_routes() {
        crate::test_support::with_isolated_home(|home| {
            let artifact_path = home.path().join("report.json");
            std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("write artifact");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let artifact = store
                .record_artifact(&run.id, "report", &artifact_path)
                .expect("artifact");

            let response = response_for_request(
                home.path(),
                "GET",
                &format!("/runs/{}/artifacts/{}", run.id, artifact.id),
            )
            .expect("artifact route response");

            assert_eq!(response.status, 200);
            assert_eq!(response.body, br#"{"ok":true}"#);
            assert!(response
                .headers
                .contains(&("x-homeboy-run-id".to_string(), run.id)));
        });
    }

    #[test]
    fn serves_head_for_run_scoped_artifact_routes() {
        crate::test_support::with_isolated_home(|home| {
            let artifact_path = home.path().join("report.json");
            std::fs::write(&artifact_path, br#"{"ok":true}"#).expect("write artifact");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let artifact = store
                .record_artifact(&run.id, "report", &artifact_path)
                .expect("artifact");

            let response = response_for_request(
                home.path(),
                "HEAD",
                &format!("/runs/{}/artifacts/{}", run.id, artifact.id),
            )
            .expect("artifact route response");

            assert_eq!(response.status, 200);
            assert!(response.body.is_empty());
            assert!(response
                .headers
                .contains(&("content-length".to_string(), "11".to_string())));
        });
    }
}
