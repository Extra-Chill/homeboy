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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    pub public_base_url_shape: String,
    pub mapping: ArtifactOriginMapping,
    pub cors: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactOriginMapping {
    pub request_path: String,
    pub filesystem_path: String,
    pub public_url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactOriginInspect {
    pub command: &'static str,
    pub root: String,
    pub input: String,
    pub request_path: String,
    pub filesystem_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    pub exists: bool,
    pub status_code: u16,
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
    status_with_command("tunnel.artifact_origin.serve", bind, root)
}

pub fn status_with_command(
    command: &'static str,
    bind: String,
    root: PathBuf,
) -> ArtifactOriginStatus {
    let public_base_url = std::env::var(crate::core::artifacts::PUBLIC_ARTIFACT_BASE_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty());
    let public_base_url_shape = public_base_url
        .as_deref()
        .unwrap_or("https://<public-host>")
        .to_string();
    ArtifactOriginStatus {
        command,
        bind,
        root: root.display().to_string(),
        public_base_url,
        public_base_url_shape: format!("{public_base_url_shape}/<artifact-root-relative-path>"),
        mapping: ArtifactOriginMapping {
            request_path: "/workflow-bench/<bundle>/report.html".to_string(),
            filesystem_path: root
                .join("workflow-bench/<bundle>/report.html")
                .display()
                .to_string(),
            public_url: format!("{public_base_url_shape}/workflow-bench/<bundle>/report.html"),
        },
        cors: true,
    }
}

pub fn inspect(root: PathBuf, input: &str) -> ArtifactOriginInspect {
    let request_path = request_path_from_input(&root, input);
    let filesystem_path = safe_target_path(&root, &request_path).unwrap_or_else(|| root.clone());
    let public_base_url = std::env::var(crate::core::artifacts::PUBLIC_ARTIFACT_BASE_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty());
    let public_url = public_base_url.as_deref().and_then(|base| {
        crate::core::artifacts::public_artifact_path_url(&root, base, &filesystem_path)
    });
    let exists = filesystem_path.is_file();
    ArtifactOriginInspect {
        command: "tunnel.artifact_origin.inspect",
        root: root.display().to_string(),
        input: input.to_string(),
        request_path,
        filesystem_path: filesystem_path.display().to_string(),
        public_url,
        exists,
        status_code: if exists { 200 } else { 404 },
    }
}

pub(crate) fn handle_stream(mut stream: TcpStream, root: &Path) -> Result<()> {
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
        return Ok(json_error_response(
            405,
            br#"{"error":"method_not_allowed"}"#.to_vec(),
            headers,
        ));
    }
    if let Some(response) = artifact_route_response(method, target, headers.clone())? {
        return Ok(response);
    }
    let Some(path) = safe_target_path(root, target) else {
        return Ok(json_error_response(
            404,
            br#"{"error":"not_found"}"#.to_vec(),
            headers,
        ));
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

fn json_error_response(
    status: u16,
    body: Vec<u8>,
    mut headers: Vec<(String, String)>,
) -> ArtifactOriginResponse {
    headers.push(("content-type".to_string(), "application/json".to_string()));
    headers.push(("content-length".to_string(), body.len().to_string()));
    ArtifactOriginResponse {
        status,
        headers,
        body,
    }
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

fn request_path_from_input(root: &Path, input: &str) -> String {
    let trimmed = input.trim();
    let path = if let Ok(path) = PathBuf::from(trimmed).strip_prefix(root) {
        path.to_string_lossy().to_string()
    } else if let Ok(url) = reqwest::Url::parse(trimmed) {
        url.path().trim_start_matches('/').to_string()
    } else {
        trimmed
            .split('?')
            .next()
            .unwrap_or(trimmed)
            .trim_start_matches('/')
            .to_string()
    };
    format!("/{}", path.trim_start_matches('/'))
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

    #[test]
    fn serves_workflow_bench_bundle_paths_under_artifact_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp
            .path()
            .join("workflow-bench/studio-web-r15-replay-export/report.html");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"<html>report</html>").expect("write report");

        let response = response_for_request(
            temp.path(),
            "GET",
            "/workflow-bench/studio-web-r15-replay-export/report.html",
        )
        .expect("response");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"<html>report</html>");
    }

    #[test]
    fn inspect_reports_404_for_missing_workflow_bench_bundle_path() {
        let temp = tempfile::tempdir().expect("tempdir");

        let output = inspect(
            temp.path().to_path_buf(),
            "/workflow-bench/missing-replay-export/report.html",
        );

        assert_eq!(output.status_code, 404);
        assert!(!output.exists);
        assert_eq!(
            output.filesystem_path,
            temp.path()
                .join("workflow-bench/missing-replay-export/report.html")
                .display()
                .to_string()
        );
    }
}
