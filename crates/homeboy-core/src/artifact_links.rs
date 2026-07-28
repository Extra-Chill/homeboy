use serde_json::Value;
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::execution_contract::encode_uri_component;
use crate::observation::{ArtifactRecord, ArtifactViewerLink};

pub const PUBLIC_ARTIFACT_BASE_URL_ENV: &str = "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL";
const PUBLIC_ARTIFACT_URL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactViewerDescriptor {
    pub kind: &'static str,
    pub base: &'static str,
    pub public_artifact_url_parameter: &'static str,
}

impl ArtifactViewerDescriptor {
    pub const fn new(
        kind: &'static str,
        base: &'static str,
        public_artifact_url_parameter: &'static str,
    ) -> Self {
        Self {
            kind,
            base,
            public_artifact_url_parameter,
        }
    }

    pub fn to_metadata(self, replay: Option<Value>) -> Value {
        let mut viewer = serde_json::json!({
            "kind": self.kind,
            "base": self.base,
            "query": {
                "parameter": self.public_artifact_url_parameter,
                "value": { "source": "public-artifact-url" },
                "encoding": "url"
            }
        });
        if let Some(replay) = replay {
            viewer["replay"] = replay;
        }
        viewer
    }
}

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

    if let Ok(root) = crate::artifacts::root() {
        if let Some(url) = public_artifact_path_url(&root, base, Path::new(&artifact.path)) {
            return Some(url);
        }
    }

    if artifact.artifact_type == "directory" {
        return None;
    }

    Some(format!(
        "{}/runs/{}/artifacts/{}",
        base,
        encode_uri_component(&artifact.run_id),
        encode_uri_component(&artifact.id)
    ))
}

/// Return the controller's stable artifact route rather than a filesystem
/// layout URL. Terminal handoffs use this route because it remains valid when
/// the runner's artifact layout is unavailable after completion.
pub fn controller_artifact_url(artifact: &ArtifactRecord) -> Result<Option<String>> {
    if artifact.artifact_type != "file" {
        return Err(Error::validation_invalid_argument(
            "artifact.type",
            "reviewer artifact URLs require a controller-owned file",
            Some(artifact.id.clone()),
            None,
        ));
    }
    let Some(base) = reviewer_public_artifact_base_url()? else {
        return Ok(None);
    };
    Ok(Some(format!(
        "{}/runs/{}/artifacts/{}",
        base,
        encode_uri_component(&artifact.run_id),
        encode_uri_component(&artifact.id)
    )))
}

/// Resolve the public artifact origin once at the configuration boundary.
/// Terminal handoffs only advertise HTTPS URLs on reviewer-reachable hosts.
pub fn reviewer_public_artifact_base_url() -> Result<Option<String>> {
    let configured = crate::defaults::load_config()
        .artifact_origin
        .public_base_url;
    let value = configured.or_else(|| std::env::var(PUBLIC_ARTIFACT_BASE_URL_ENV).ok());
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(None);
    }
    let url = reqwest::Url::parse(value).map_err(|error| {
        Error::validation_invalid_argument(
            PUBLIC_ARTIFACT_BASE_URL_ENV,
            "public artifact origin must be a valid HTTPS URL",
            Some(error.to_string()),
            None,
        )
    })?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https" || host.is_empty() || non_public_host(host) {
        return Err(Error::validation_invalid_argument(
            PUBLIC_ARTIFACT_BASE_URL_ENV,
            "public artifact origin must use HTTPS with a reviewer-reachable host",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(Some(value.to_string()))
}

fn non_public_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
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
    artifact.artifact_type == "file"
        || artifact.artifact_type == "directory"
        || artifact.artifact_type == "remote_file"
}

fn probe_public_artifact_url(
    public_url: &str,
) -> std::result::Result<reqwest::StatusCode, reqwest::Error> {
    crate::http_probe::blocking_client(PUBLIC_ARTIFACT_URL_PROBE_TIMEOUT)?
        .get(public_url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .map(|response| response.status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::ArtifactRecord;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn derives_viewer_link_from_public_artifact_url_metadata() {
        let artifact = ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "preview-after".to_string(),
            path: "/tmp/preview.after.json".to_string(),
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
            ..Default::default()
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
    fn descriptor_produces_public_artifact_viewer_metadata() {
        let viewer = ArtifactViewerDescriptor::new(
            "example-viewer-kind",
            "https://viewer.example.test/",
            "artifact-url",
        )
        .to_metadata(None);

        assert_eq!(viewer["kind"], "example-viewer-kind");
        assert_eq!(viewer["base"], "https://viewer.example.test/");
        assert_eq!(viewer["query"]["parameter"], "artifact-url");
        assert_eq!(viewer["query"]["value"]["source"], "public-artifact-url");
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

    #[test]
    fn controller_url_requires_a_public_https_origin_and_encodes_ids() {
        let _env = EnvGuard::set(
            PUBLIC_ARTIFACT_BASE_URL_ENV,
            "https://artifacts.example.test/reviewer/",
        );
        let artifact = ArtifactRecord {
            id: "source /?%".to_string(),
            run_id: "run /?%".to_string(),
            artifact_type: "file".to_string(),
            ..Default::default()
        };

        assert_eq!(
            controller_artifact_url(&artifact)
                .expect("valid reviewer origin")
                .as_deref(),
            Some("https://artifacts.example.test/reviewer/runs/run%20%2F%3F%25/artifacts/source%20%2F%3F%25")
        );
    }

    #[test]
    fn reviewer_public_artifact_base_rejects_local_or_non_https_origins() {
        for value in ["http://artifacts.example.test", "https://127.0.0.1:7351"] {
            let _env = EnvGuard::set(PUBLIC_ARTIFACT_BASE_URL_ENV, value);
            assert!(reviewer_public_artifact_base_url().is_err(), "{value}");
        }
    }

    #[test]
    fn controller_origin_uses_typed_config_before_legacy_environment() {
        crate::test_support::with_isolated_home(|_| {
            let _env = EnvGuard::set(PUBLIC_ARTIFACT_BASE_URL_ENV, "https://runner.example.test");
            let mut config = crate::defaults::HomeboyConfig::default();
            config.artifact_origin.public_base_url =
                Some("https://controller.example.test/".to_string());
            crate::defaults::save_config(&config).expect("save controller config");

            assert_eq!(
                reviewer_public_artifact_base_url()
                    .expect("configured origin")
                    .as_deref(),
                Some("https://controller.example.test")
            );
        });
    }

    #[test]
    fn controller_url_is_absent_without_controller_origin() {
        crate::test_support::with_isolated_home(|_| {
            let _env = EnvGuard::unset(PUBLIC_ARTIFACT_BASE_URL_ENV);
            let artifact = ArtifactRecord {
                id: "artifact-1".to_string(),
                run_id: "run-1".to_string(),
                artifact_type: "file".to_string(),
                ..Default::default()
            };

            assert_eq!(
                controller_artifact_url(&artifact).expect("optional URL"),
                None
            );
        });
    }

    #[test]
    fn directory_artifact_public_url_requires_artifact_root_path() {
        let _env = EnvGuard::set(
            PUBLIC_ARTIFACT_BASE_URL_ENV,
            "https://artifacts.example.test/base",
        );
        let root = tempfile::tempdir().expect("artifact root");
        crate::set_artifact_root_override(Some(root.path().to_path_buf()));
        let artifact = ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "fuzz_artifacts".to_string(),
            artifact_type: "directory".to_string(),
            path: tempfile::tempdir()
                .expect("operator local directory")
                .path()
                .display()
                .to_string(),
            metadata_json: serde_json::json!({}),
            created_at: "2026-06-12T00:00:00Z".to_string(),
            ..Default::default()
        };

        assert_eq!(public_artifact_url(&artifact), None);
        crate::set_artifact_root_override(None);
    }

    fn viewer_artifact() -> ArtifactRecord {
        ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "preview-after".to_string(),
            path: "/tmp/preview.after.json".to_string(),
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
            ..Default::default()
        }
    }

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let lock = ENV_LOCK.lock().expect("environment lock");
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self {
                key,
                prior,
                _lock: lock,
            }
        }

        fn unset(key: &'static str) -> Self {
            let lock = ENV_LOCK.lock().expect("environment lock");
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key,
                prior,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
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
