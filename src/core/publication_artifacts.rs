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

struct ManifestArtifactRef<'a> {
    reference: &'a Value,
    locator: String,
    locator_type: &'static str,
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
    let url_bases = publication_url_bases(manifest);
    collect_publication_artifact_refs(manifest, &url_bases, &mut refs);

    for reference in refs {
        let locator = reference.locator.as_str();
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
            .reference
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&locator);
        let id = published_artifact_id(&source.id, original_manifest_id, &locator);
        if store.get_artifact(&id)?.is_some() {
            continue;
        }
        let kind = reference
            .reference
            .get("kind")
            .and_then(Value::as_str)
            .or_else(|| reference.reference.get("role").and_then(Value::as_str))
            .unwrap_or("published_artifact")
            .to_string();
        let media_type = reference
            .reference
            .get("media_type")
            .or_else(|| reference.reference.get("mime"))
            .or_else(|| reference.reference.get("contentType"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| crate::core::artifact_metadata::content_type_from_path(Path::new(&path)));
        let size_bytes = reference
            .reference
            .get("bytes")
            .or_else(|| reference.reference.get("size_bytes"))
            .and_then(Value::as_i64);
        let sha256 = reference
            .reference
            .get("sha256")
            .and_then(|sha256| {
                sha256
                    .as_str()
                    .or_else(|| sha256.get("value").and_then(Value::as_str))
            })
            .map(str::to_string);
        let created_at = reference
            .reference
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
            "role": reference.reference.get("role").cloned().unwrap_or(Value::Null),
            "locator": {
                "type": reference.locator_type,
                "value": locator,
            },
            "media_type": media_type,
            "bytes": size_bytes,
            "sha256": sha256,
            "description": reference.reference.get("description").cloned().unwrap_or(Value::Null),
            "metadata": reference.reference.get("metadata").cloned().unwrap_or(Value::Null),
            "viewer": reference.reference.get("viewer").cloned().unwrap_or(Value::Null),
        });

        let mut artifact = ArtifactRecord {
            id,
            run_id: source.run_id.clone(),
            kind,
            artifact_type,
            path,
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256,
            size_bytes,
            mime: media_type,
            metadata_json,
            created_at,
        };
        crate::core::artifact_links::annotate_public_artifact_url_validation(&mut artifact);
        store.import_artifact(&artifact)?;
    }

    Ok(())
}

fn collect_publication_artifact_refs<'a>(
    value: &'a Value,
    url_bases: &[String],
    refs: &mut Vec<ManifestArtifactRef<'a>>,
) {
    match value {
        Value::Object(object) => {
            if let Some(locator) = artifact_store_locator(value) {
                refs.push(ManifestArtifactRef {
                    reference: value,
                    locator: locator.to_string(),
                    locator_type: "artifact-store",
                });
            } else if let Some(locator) = url_locator_artifact_store_locator(value, url_bases) {
                refs.push(ManifestArtifactRef {
                    reference: value,
                    locator,
                    locator_type: "url",
                });
            }
            for child in object.values() {
                collect_publication_artifact_refs(child, url_bases, refs);
            }
        }
        Value::Array(array) => {
            for child in array {
                collect_publication_artifact_refs(child, url_bases, refs);
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

fn url_locator_artifact_store_locator(reference: &Value, url_bases: &[String]) -> Option<String> {
    let locator = reference.get("locator")?;
    if locator.get("type").and_then(Value::as_str)? != "url" {
        return None;
    }
    let value = locator.get("value").and_then(Value::as_str)?;
    url_locator_relative_path(value, url_bases)
}

fn publication_url_bases(manifest: &Value) -> Vec<String> {
    let mut bases = Vec::new();
    for object in [manifest.get("publication"), Some(manifest)]
        .into_iter()
        .flatten()
    {
        for key in [
            "public_base_url",
            "publicBaseUrl",
            "artifact_base",
            "artifactBase",
        ] {
            if let Some(base) = object.get(key).and_then(Value::as_str) {
                bases.push(base.to_string());
            }
        }
    }
    bases.sort();
    bases.dedup();
    bases
}

fn url_locator_relative_path(value: &str, bases: &[String]) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    for base in bases {
        let Ok(base) = reqwest::Url::parse(base) else {
            continue;
        };
        if url.scheme() != base.scheme()
            || url.host_str() != base.host_str()
            || url.port_or_known_default() != base.port_or_known_default()
        {
            continue;
        }
        let mut base_path = base.path().to_string();
        if !base_path.ends_with('/') {
            base_path.push('/');
        }
        let Some(relative) = url.path().strip_prefix(&base_path) else {
            continue;
        };
        if relative.is_empty() || artifact_store_path(Path::new(""), relative).is_none() {
            continue;
        }
        return Some(relative.to_string());
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::{NewRunRecord, ObservationStore};

    #[test]
    fn indexes_url_locators_under_publication_base_and_preserves_viewer() {
        crate::test_support::with_isolated_home(|_| {
            let publication_dir = tempfile::tempdir().expect("publication dir");
            std::fs::write(publication_dir.path().join("blueprint.after.json"), "{}").unwrap();
            let manifest_path = publication_dir.path().join("publication-manifest.json");
            std::fs::write(
                &manifest_path,
                serde_json::json!({
                    "id": "workflow-bench-publication",
                    "publication": {
                        "public_base_url": "https://artifacts.example.test/runs/e62c34cf/"
                    },
                    "blueprint": {
                        "after": {
                            "id": "blueprint.after",
                            "kind": "blueprint",
                            "media_type": "application/json",
                            "locator": {
                                "type": "url",
                                "value": "https://artifacts.example.test/runs/e62c34cf/blueprint.after.json"
                            },
                            "viewer": {
                                "url": "https://viewer.example.test/?artifact=blueprint.after"
                            }
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let manifest = store
                .record_artifact(&run.id, "publication_manifest", &manifest_path)
                .expect("manifest artifact");
            let artifacts = store.list_artifacts(&run.id).expect("artifacts");
            let indexed = artifacts
                .iter()
                .find(|artifact| artifact.id != manifest.id && artifact.kind == "blueprint")
                .expect("indexed blueprint artifact");

            assert_eq!(indexed.artifact_type, "file");
            assert!(Path::new(&indexed.path).is_file());
            assert_eq!(indexed.metadata_json["locator"]["type"], "url");
            assert_eq!(
                indexed.metadata_json["locator"]["value"],
                "blueprint.after.json"
            );
            assert_eq!(
                indexed.metadata_json["viewer"]["url"],
                "https://viewer.example.test/?artifact=blueprint.after"
            );
        });
    }

    #[test]
    fn rejects_url_locators_outside_publication_base() {
        crate::test_support::with_isolated_home(|_| {
            let publication_dir = tempfile::tempdir().expect("publication dir");
            std::fs::write(publication_dir.path().join("leak.json"), "{}").unwrap();
            let manifest_path = publication_dir.path().join("publication-manifest.json");
            std::fs::write(
                &manifest_path,
                serde_json::json!({
                    "publication": {
                        "public_base_url": "https://artifacts.example.test/runs/e62c34cf/"
                    },
                    "blueprint": {
                        "after": {
                            "id": "blueprint.after",
                            "kind": "blueprint",
                            "locator": {
                                "type": "url",
                                "value": "https://evil.example.test/runs/e62c34cf/leak.json"
                            }
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            store
                .record_artifact(&run.id, "publication_manifest", &manifest_path)
                .expect("manifest artifact");
            let artifacts = store.list_artifacts(&run.id).expect("artifacts");

            assert_eq!(artifacts.len(), 1);
        });
    }
}
