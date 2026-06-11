use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::execution_contract::{decode_uri_component, EXECUTION_CONTRACT};
use crate::core::observation::{ArtifactRecord, ObservationStore};
use crate::core::{paths, runner, Result};

pub(crate) fn index_published_artifact_refs(
    store: &ObservationStore,
    source: &ArtifactRecord,
    source_path: Option<&Path>,
) -> Result<()> {
    if source.artifact_type != "file" || !json_artifact(source) {
        return Ok(());
    }

    let content = match std::fs::read_to_string(&source.path) {
        Ok(content) => content,
        Err(_) => return Ok(()),
    };
    let manifest: Value = match serde_json::from_str(&content) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(()),
    };

    let artifact_root = paths::artifact_root()?;
    let source_bases = source_path
        .and_then(Path::parent)
        .into_iter()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    index_manifest_refs(store, source, &manifest, |locator| {
        let Some(path) = artifact_store_path(&artifact_root, locator) else {
            return Ok(None);
        };
        if path.is_file() || materialize_artifact_store_path(&path, locator, &source_bases)? {
            Ok(Some(IndexedArtifactPath::Local(path)))
        } else {
            Ok(Some(IndexedArtifactPath::Missing))
        }
    })
}

pub fn index_remote_published_artifact_refs_for_run(
    store: &ObservationStore,
    run_id: &str,
) -> Result<()> {
    for artifact in store.list_artifacts(run_id)? {
        if !is_remote_publication_manifest_candidate(&artifact) {
            continue;
        }
        let Ok(download) = runner::download_remote_artifact(&artifact.path, None) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&download.output_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        let Some((runner_id, remote_run_id)) = remote_artifact_runner_context(&artifact.path)
        else {
            continue;
        };
        index_manifest_refs(store, &artifact, &manifest, |locator| {
            Ok(Some(IndexedArtifactPath::Remote(
                runner::runner_artifact_store_token(&runner_id, &remote_run_id, locator),
            )))
        })?;
    }

    Ok(())
}

enum IndexedArtifactPath {
    Local(PathBuf),
    Remote(String),
    Missing,
}

fn index_manifest_refs(
    store: &ObservationStore,
    source: &ArtifactRecord,
    manifest: &Value,
    mut resolve_path: impl FnMut(&str) -> Result<Option<IndexedArtifactPath>>,
) -> Result<()> {
    let manifest_id = manifest
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut refs = Vec::new();
    collect_artifact_store_refs(manifest, &mut refs);

    for reference in refs {
        let Some(locator) = artifact_store_locator(reference) else {
            continue;
        };
        let Some(indexed_path) = resolve_path(locator)? else {
            continue;
        };
        let (artifact_type, path) = match indexed_path {
            IndexedArtifactPath::Local(path) => {
                ("file".to_string(), path.to_string_lossy().to_string())
            }
            IndexedArtifactPath::Remote(path) => ("remote_file".to_string(), path),
            IndexedArtifactPath::Missing => continue,
        };
        let original_manifest_id = reference
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(locator);
        let id = published_artifact_id(&source.id, original_manifest_id, locator);
        if store.get_artifact(&id)?.is_some() {
            continue;
        }
        let kind = reference
            .get("kind")
            .and_then(Value::as_str)
            .or_else(|| reference.get("role").and_then(Value::as_str))
            .unwrap_or("published_artifact")
            .to_string();
        let media_type = reference
            .get("media_type")
            .or_else(|| reference.get("mime"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| crate::core::artifact_metadata::content_type_from_path(Path::new(&path)));
        let size_bytes = reference
            .get("bytes")
            .or_else(|| reference.get("size_bytes"))
            .and_then(Value::as_i64);
        let sha256 = reference
            .get("sha256")
            .and_then(Value::as_str)
            .map(str::to_string);
        let created_at = reference
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or(&source.created_at)
            .to_string();
        let name = artifact_name(original_manifest_id);
        let metadata_json = json!({
            "source": "publication_manifest",
            "source_manifest_artifact_id": source.id,
            "source_manifest_kind": source.kind,
            "manifest_id": manifest_id,
            "original_manifest_id": original_manifest_id,
            "name": name,
            "role": reference.get("role").cloned().unwrap_or(Value::Null),
            "locator": {
                "type": "artifact-store",
                "value": locator,
            },
            "media_type": media_type,
            "bytes": size_bytes,
            "sha256": sha256,
            "description": reference.get("description").cloned().unwrap_or(Value::Null),
            "metadata": reference.get("metadata").cloned().unwrap_or(Value::Null),
        });

        store.import_artifact(&ArtifactRecord {
            id,
            run_id: source.run_id.clone(),
            kind,
            artifact_type,
            path,
            url: None,
            sha256,
            size_bytes,
            mime: media_type,
            metadata_json,
            created_at,
        })?;
    }

    Ok(())
}

fn collect_artifact_store_refs<'a>(value: &'a Value, refs: &mut Vec<&'a Value>) {
    match value {
        Value::Object(object) => {
            if artifact_store_locator(value).is_some() {
                refs.push(value);
            }
            for child in object.values() {
                collect_artifact_store_refs(child, refs);
            }
        }
        Value::Array(array) => {
            for child in array {
                collect_artifact_store_refs(child, refs);
            }
        }
        _ => {}
    }
}

fn artifact_store_locator(reference: &Value) -> Option<&str> {
    let locator = reference.get("locator")?;
    if locator.get("type").and_then(Value::as_str)? != "artifact-store" {
        return None;
    }
    locator.get("value").and_then(Value::as_str)
}

fn artifact_store_path(root: &Path, locator: &str) -> Option<PathBuf> {
    let locator_path = PathBuf::from(locator);
    if locator_path.is_absolute()
        || locator_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(root.join(locator_path))
}

fn materialize_artifact_store_path(
    destination: &Path,
    locator: &str,
    source_bases: &[PathBuf],
) -> Result<bool> {
    for source_base in source_bases {
        for source in artifact_store_source_candidates(source_base, locator) {
            if !source.is_file() {
                continue;
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    crate::core::Error::internal_io(
                        err.to_string(),
                        Some(format!("create {}", parent.display())),
                    )
                })?;
            }
            std::fs::copy(&source, destination).map_err(|err| {
                crate::core::Error::internal_io(
                    err.to_string(),
                    Some(format!(
                        "copy publication artifact-store ref {} to {}",
                        source.display(),
                        destination.display()
                    )),
                )
            })?;
            return Ok(true);
        }
    }
    Ok(false)
}

fn artifact_store_source_candidates(source_base: &Path, locator: &str) -> Vec<PathBuf> {
    let segments = PathBuf::from(locator)
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_os_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for index in 0..segments.len() {
        let mut candidate = source_base.to_path_buf();
        for segment in &segments[index..] {
            candidate.push(segment);
        }
        candidates.push(candidate);
    }
    candidates
}

fn is_remote_publication_manifest_candidate(artifact: &ArtifactRecord) -> bool {
    if artifact.artifact_type != "remote_file" || !json_artifact(artifact) {
        return false;
    }
    artifact
        .metadata_json
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| name.contains("manifest"))
        || artifact.kind.contains("manifest")
        || artifact
            .metadata_json
            .get("original_path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("manifest.json"))
}

fn json_artifact(artifact: &ArtifactRecord) -> bool {
    artifact
        .mime
        .as_deref()
        .unwrap_or_default()
        .contains("json")
}

fn remote_artifact_runner_context(path: &str) -> Option<(String, String)> {
    let token = EXECUTION_CONTRACT
        .artifacts
        .strip_runner_artifact_scheme(path)?;
    let mut parts = token.split('/');
    let runner_id = parts.next()?;
    let run_id = parts.next()?;
    if runner_id.is_empty() || run_id.is_empty() {
        return None;
    }
    Some((
        decode_uri_component(runner_id),
        decode_uri_component(run_id),
    ))
}

fn published_artifact_id(source_artifact_id: &str, manifest_id: &str, locator: &str) -> String {
    let hash = Sha256::digest(format!("{source_artifact_id}\0{manifest_id}\0{locator}").as_bytes());
    let hex = format!("{hash:x}");
    format!("{source_artifact_id}-published-{}", &hex[..16])
}

fn artifact_name(manifest_id: &str) -> String {
    PathBuf::from(manifest_id)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(manifest_id)
        .to_string()
}
