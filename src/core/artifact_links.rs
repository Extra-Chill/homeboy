use serde_json::Value;
use std::path::Path;
use std::time::Duration;

use crate::core::execution_contract::encode_uri_component;
use crate::core::observation::{ArtifactRecord, ArtifactViewerLink};

pub const PUBLIC_ARTIFACT_BASE_URL_ENV: &str = "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL";
const PUBLIC_ARTIFACT_URL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicArtifactUrlValidation {
    pub url: String,
    pub reachable: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
}

pub fn public_artifact_url(artifact: &ArtifactRecord) -> Option<String> {
    if !artifact_is_fetchable(artifact) {
        return None;
    }
    let base = std::env::var(PUBLIC_ARTIFACT_BASE_URL_ENV).ok()?;
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }

    if let Ok(root) = crate::core::artifacts::root() {
        if let Some(url) = public_artifact_path_url(&root, base, Path::new(&artifact.path)) {
            return Some(url);
        }
    }

    Some(format!(
        "{}/runs/{}/artifacts/{}",
        base,
        encode_uri_component(&artifact.run_id),
        encode_uri_component(&artifact.id)
    ))
}

pub fn public_artifact_path_url(root: &Path, base: &str, path: &Path) -> Option<String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    let segments = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(segment) => segment
                .to_str()
                .map(encode_uri_component)
                .filter(|segment| !segment.is_empty()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if segments.is_empty() {
        return None;
    }
    Some(format!("{}/{}", base, segments.join("/")))
}

pub fn viewer_links(
    artifact: &ArtifactRecord,
    public_url: Option<&str>,
) -> Vec<ArtifactViewerLink> {
    let Some(public_url) = public_url else {
        return Vec::new();
    };
    let Some(viewer) = artifact.metadata_json.get("viewer") else {
        return Vec::new();
    };
    let Some(base) = viewer.get("base").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(query) = viewer.get("query") else {
        return Vec::new();
    };
    let Some(parameter) = query.get("parameter").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(value) = query.get("value") else {
        return Vec::new();
    };
    if value.get("source").and_then(Value::as_str) != Some("public-artifact-url") {
        return Vec::new();
    }

    let separator = if base.contains('?') { "&" } else { "?" };
    vec![ArtifactViewerLink {
        kind: viewer
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("artifact-viewer")
            .to_string(),
        url: format!(
            "{}{}{}={}",
            base,
            separator,
            encode_uri_component(parameter),
            encode_uri_component(public_url)
        ),
        replay: viewer.get("replay").cloned(),
    }]
}

pub fn validated_viewer_links(
    artifact: &ArtifactRecord,
    public_url: &str,
) -> (Vec<ArtifactViewerLink>, Option<PublicArtifactUrlValidation>) {
    let links = viewer_links(artifact, Some(public_url));
    if links.is_empty() {
        return (links, None);
    }

    let validation = validate_public_artifact_url(public_url);
    if validation.reachable {
        (links, Some(validation))
    } else {
        (Vec::new(), Some(validation))
    }
}

pub fn cached_validated_viewer_links(
    artifact: &ArtifactRecord,
    public_url: &str,
) -> Vec<ArtifactViewerLink> {
    let links = viewer_links(artifact, Some(public_url));
    if links.is_empty() {
        return links;
    }
    if artifact
        .metadata_json
        .get("public_url_validation")
        .and_then(|validation| validation.get("reachable"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        links
    } else {
        Vec::new()
    }
}

pub fn annotate_public_artifact_url_validation(
    artifact: &mut ArtifactRecord,
) -> Option<PublicArtifactUrlValidation> {
    let public_url = public_artifact_url(artifact)?;
    if viewer_links(artifact, Some(&public_url)).is_empty() {
        return None;
    }
    let validation = validate_public_artifact_url(&public_url);
    artifact.metadata_json["public_url_validation"] =
        public_artifact_url_validation_json(&validation);
    Some(validation)
}

pub fn public_artifact_url_validation_json(
    validation: &PublicArtifactUrlValidation,
) -> serde_json::Value {
    serde_json::json!({
        "url": validation.url,
        "reachable": validation.reachable,
        "status_code": validation.status_code,
        "error": validation.error,
    })
}

pub fn validate_public_artifact_url(public_url: &str) -> PublicArtifactUrlValidation {
    match probe_public_artifact_url(public_url) {
        Ok(status) if status.is_success() => PublicArtifactUrlValidation {
            url: public_url.to_string(),
            reachable: true,
            status_code: Some(status.as_u16()),
            error: None,
        },
        Ok(status) => PublicArtifactUrlValidation {
            url: public_url.to_string(),
            reachable: false,
            status_code: Some(status.as_u16()),
            error: Some(format!(
                "public artifact URL returned HTTP {}",
                status.as_u16()
            )),
        },
        Err(error) => PublicArtifactUrlValidation {
            url: public_url.to_string(),
            reachable: false,
            status_code: None,
            error: Some(error.to_string()),
        },
    }
}

fn artifact_is_fetchable(artifact: &ArtifactRecord) -> bool {
    artifact.artifact_type == "file" || artifact.artifact_type == "remote_file"
}

fn probe_public_artifact_url(public_url: &str) -> Result<reqwest::StatusCode, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .timeout(PUBLIC_ARTIFACT_URL_PROBE_TIMEOUT)
        .build()?
        .get(public_url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .map(|response| response.status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::ArtifactRecord;

    #[test]
    fn derives_viewer_link_from_public_artifact_url_metadata() {
        let artifact = ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "preview-after".to_string(),
            artifact_type: "file".to_string(),
            path: "/tmp/preview.after.json".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: Some("application/json".to_string()),
            metadata_json: serde_json::json!({
                "viewer": {
                    "kind": "artifact-preview",
                    "base": "https://viewer.example.test/",
                    "query": {
                        "parameter": "artifact-url",
                        "value": { "source": "public-artifact-url", "path": "preview.after.json" },
                        "encoding": "url"
                    },
                    "replay": { "status": "partial", "limitations": [] }
                }
            }),
            created_at: "2026-06-12T00:00:00Z".to_string(),
        };

        let links = viewer_links(&artifact, Some("https://artifacts.example.test/a b.json"));

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, "artifact-preview");
        assert_eq!(
            links[0].url,
            "https://viewer.example.test/?artifact-url=https%3A%2F%2Fartifacts.example.test%2Fa%20b.json"
        );
    }

    #[test]
    fn validated_viewer_links_requires_reachable_public_url() {
        let public_url = serve_once(404);
        let artifact = viewer_artifact();

        let (links, validation) = validated_viewer_links(&artifact, &public_url);

        assert!(links.is_empty());
        let validation = validation.expect("validation result");
        assert!(!validation.reachable);
        assert_eq!(validation.status_code, Some(404));
        assert_eq!(
            validation.error.as_deref(),
            Some("public artifact URL returned HTTP 404")
        );
    }

    #[test]
    fn validated_viewer_links_emits_links_for_reachable_public_url() {
        let public_url = serve_once(200);
        let artifact = viewer_artifact();

        let (links, validation) = validated_viewer_links(&artifact, &public_url);

        assert_eq!(links.len(), 1);
        assert!(links[0].url.contains("artifact-url="));
        assert!(validation.expect("validation result").reachable);
    }

    #[test]
    fn public_artifact_path_url_uses_artifact_root_relative_path() {
        let root = tempfile::tempdir().expect("artifact root");
        let path = root
            .path()
            .join("workflow-bench/studio web replay/report.html");

        assert_eq!(
            public_artifact_path_url(root.path(), "https://artifacts.example.test/base/", &path)
                .as_deref(),
            Some("https://artifacts.example.test/base/workflow-bench/studio%20web%20replay/report.html")
        );
    }

    fn viewer_artifact() -> ArtifactRecord {
        ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "preview-after".to_string(),
            artifact_type: "file".to_string(),
            path: "/tmp/preview.after.json".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: Some("application/json".to_string()),
            metadata_json: serde_json::json!({
                "viewer": {
                    "kind": "artifact-preview",
                    "base": "https://viewer.example.test/",
                    "query": {
                        "parameter": "artifact-url",
                        "value": { "source": "public-artifact-url" },
                        "encoding": "url"
                    }
                }
            }),
            created_at: "2026-06-12T00:00:00Z".to_string(),
        }
    }

    fn serve_once(status: u16) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("server address");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let status_text = if status == 200 { "OK" } else { "Not Found" };
            let body = if status == 200 { "ok" } else { "missing" };
            write!(
                stream,
                "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write response");
        });
        format!("http://{addr}/artifact.json")
    }
}
