use serde_json::Value;

use crate::core::execution_contract::encode_uri_component;
use crate::core::observation::{ArtifactRecord, ArtifactViewerLink};

pub const PUBLIC_ARTIFACT_BASE_URL_ENV: &str = "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL";

pub fn public_artifact_url(artifact: &ArtifactRecord) -> Option<String> {
    if !artifact_is_fetchable(artifact) {
        return None;
    }
    let base = std::env::var(PUBLIC_ARTIFACT_BASE_URL_ENV).ok()?;
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }

    Some(format!(
        "{}/runs/{}/artifacts/{}",
        base,
        encode_uri_component(&artifact.run_id),
        encode_uri_component(&artifact.id)
    ))
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

fn artifact_is_fetchable(artifact: &ArtifactRecord) -> bool {
    artifact.artifact_type == "file" || artifact.artifact_type == "remote_file"
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
}
