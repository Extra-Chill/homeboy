use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_address::validated_public_url;
use crate::core::observation::ArtifactRecord;

const MAX_DISCOVERED_HTML_ENTRYPOINTS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactPreviewEntrypoint {
    pub artifact_id: String,
    pub artifact_kind: String,
    pub path: String,
    pub label: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

pub fn html_preview_entrypoints(artifact: &ArtifactRecord) -> Vec<ArtifactPreviewEntrypoint> {
    if artifact.artifact_type != "directory" {
        return Vec::new();
    }

    let root = Path::new(&artifact.path);
    let mut entrypoints = declared_html_entrypoints(artifact);
    let mut discovered = discover_html_entrypoints(artifact, root);
    entrypoints.append(&mut discovered);
    dedupe_entrypoints(entrypoints)
}

fn declared_html_entrypoints(artifact: &ArtifactRecord) -> Vec<ArtifactPreviewEntrypoint> {
    artifact
        .metadata_json
        .get("entrypoints")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let path = entry.get("path")?.as_str()?;
            if !is_safe_relative_path(path) || !is_html_path(path) {
                return None;
            }
            let label = entry
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_else(|| default_entrypoint_label(path));
            let mime_type = entry
                .get("mime_type")
                .or_else(|| entry.get("mime"))
                .and_then(Value::as_str)
                .unwrap_or("text/html");
            Some(preview_entrypoint(artifact, path, label, mime_type))
        })
        .collect()
}

fn discover_html_entrypoints(
    artifact: &ArtifactRecord,
    root: &Path,
) -> Vec<ArtifactPreviewEntrypoint> {
    if !root.is_dir() {
        return Vec::new();
    }

    let mut paths = Vec::new();
    collect_html_paths(root, root, &mut paths);
    paths.sort_by(|left, right| {
        entrypoint_rank(left)
            .cmp(&entrypoint_rank(right))
            .then_with(|| left.cmp(right))
    });
    paths.truncate(MAX_DISCOVERED_HTML_ENTRYPOINTS);
    paths
        .into_iter()
        .map(|path| {
            let label = default_entrypoint_label(&path).to_string();
            preview_entrypoint(artifact, &path, &label, "text/html")
        })
        .collect()
}

fn collect_html_paths(root: &Path, current: &Path, paths: &mut Vec<String>) {
    if paths.len() >= MAX_DISCOVERED_HTML_ENTRYPOINTS {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut entries = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if paths.len() >= MAX_DISCOVERED_HTML_ENTRYPOINTS {
            break;
        }
        if path.is_dir() {
            collect_html_paths(root, &path, paths);
            continue;
        }
        if !is_html_path(&path.to_string_lossy()) {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            paths.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn preview_entrypoint(
    artifact: &ArtifactRecord,
    path: &str,
    label: &str,
    mime_type: &str,
) -> ArtifactPreviewEntrypoint {
    ArtifactPreviewEntrypoint {
        artifact_id: artifact.id.clone(),
        artifact_kind: artifact.kind.clone(),
        path: path.to_string(),
        label: label.to_string(),
        mime_type: mime_type.to_string(),
        public_url: entrypoint_public_url(artifact, path),
    }
}

fn entrypoint_public_url(artifact: &ArtifactRecord, path: &str) -> Option<String> {
    let base = artifact
        .public_url
        .as_deref()
        .or(artifact.url.as_deref())
        .or_else(|| {
            artifact
                .metadata_json
                .get("public_url")
                .and_then(Value::as_str)
        })?;
    let base = validated_public_url(base)?;
    reqwest::Url::parse(&base)
        .ok()
        .and_then(|url| url.join(path).ok())
        .map(|url| url.to_string())
        .and_then(|url| validated_public_url(&url))
}

fn dedupe_entrypoints(
    entrypoints: Vec<ArtifactPreviewEntrypoint>,
) -> Vec<ArtifactPreviewEntrypoint> {
    let mut seen = std::collections::BTreeSet::new();
    entrypoints
        .into_iter()
        .filter(|entrypoint| seen.insert(entrypoint.path.clone()))
        .collect()
}

fn entrypoint_rank(path: &str) -> (u8, usize) {
    if path.eq_ignore_ascii_case("index.html") || path.eq_ignore_ascii_case("index.htm") {
        return (0, 0);
    }
    if path.rsplit('/').next().is_some_and(|name| {
        name.eq_ignore_ascii_case("index.html") || name.eq_ignore_ascii_case("index.htm")
    }) {
        return (1, path.matches('/').count());
    }
    (2, path.matches('/').count())
}

fn default_entrypoint_label(path: &str) -> &str {
    if path.eq_ignore_ascii_case("index.html") || path.eq_ignore_ascii_case("index.htm") {
        "Open generated site"
    } else {
        "Open HTML artifact"
    }
}

fn is_html_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

fn is_safe_relative_path(path: &str) -> bool {
    let path = PathBuf::from(path);
    path.components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn artifact(path: &Path, metadata_json: Value) -> ArtifactRecord {
        ArtifactRecord {
            id: "artifact-1".to_string(),
            run_id: "run-1".to_string(),
            kind: "static_site_artifact".to_string(),
            artifact_type: "directory".to_string(),
            path: path.to_string_lossy().to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json,
            created_at: "2026-06-25T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn discovers_index_and_nested_html_entrypoints() {
        tempfile::tempdir()
            .map(|dir| {
                fs::write(dir.path().join("about.html"), b"<html></html>").unwrap();
                fs::write(dir.path().join("index.html"), b"<html></html>").unwrap();
                fs::create_dir_all(dir.path().join("case-a")).unwrap();
                fs::write(dir.path().join("case-a/index.html"), b"<html></html>").unwrap();

                let entrypoints = html_preview_entrypoints(&artifact(dir.path(), Value::Null));

                assert_eq!(entrypoints[0].path, "index.html");
                assert!(entrypoints.iter().any(|entry| entry.path == "about.html"));
                assert!(entrypoints
                    .iter()
                    .any(|entry| entry.path == "case-a/index.html"));
            })
            .unwrap();
    }

    #[test]
    fn preserves_declared_entrypoint_labels_and_dedupes_discovery() {
        tempfile::tempdir()
            .map(|dir| {
                fs::write(dir.path().join("index.html"), b"<html></html>").unwrap();
                let metadata = serde_json::json!({
                    "entrypoints": [{
                        "path": "index.html",
                        "label": "Open fixture homepage",
                        "mime_type": "text/html"
                    }]
                });

                let entrypoints = html_preview_entrypoints(&artifact(dir.path(), metadata));

                assert_eq!(entrypoints.len(), 1);
                assert_eq!(entrypoints[0].label, "Open fixture homepage");
            })
            .unwrap();
    }
}
