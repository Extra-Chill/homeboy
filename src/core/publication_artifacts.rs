use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::observation::{ArtifactRecord, ObservationStore};
use crate::core::{paths, Result};

pub(crate) fn index_published_artifact_refs(
    store: &ObservationStore,
    source: &ArtifactRecord,
) -> Result<()> {
    if source.artifact_type != "file"
        || !source.mime.as_deref().unwrap_or_default().contains("json")
    {
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
    let manifest_id = manifest
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut refs = Vec::new();
    collect_artifact_store_refs(&manifest, &mut refs);

    for reference in refs {
        let Some(locator) = artifact_store_locator(reference) else {
            continue;
        };
        let Some(path) = artifact_store_path(&artifact_root, locator) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
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
            .or_else(|| crate::core::artifact_metadata::content_type_from_path(&path));
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
            artifact_type: "file".to_string(),
            path: path.to_string_lossy().to_string(),
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
