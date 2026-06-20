//! Product-neutral artifact manifest validation.
//!
//! Domain-specific workloads decide which artifacts matter. Core only defines
//! the portable file-entry shape and confines declared paths to an artifact root.

use crate::core::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const ARTIFACT_MANIFEST_FILE: &str = "homeboy-artifact-manifest.json";
pub const ARTIFACT_MANIFEST_SCHEMA: &str = "homeboy/artifact-manifest/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifest {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifestEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub path: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ArtifactManifestProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<ArtifactManifestViewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url_state: Option<ArtifactManifestPublicUrlState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<ArtifactRedactionState>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifestProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifestViewer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<ArtifactManifestViewerLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifestViewerLink {
    pub kind: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactManifestPublicUrlState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRedactionState {
    Unspecified,
    Raw,
    Redacted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedArtifactManifestEntry {
    pub entry: ArtifactManifestEntry,
    pub absolute_path: PathBuf,
}

impl ArtifactManifest {
    pub fn new(artifacts: Vec<ArtifactManifestEntry>) -> Self {
        Self {
            schema: ARTIFACT_MANIFEST_SCHEMA.to_string(),
            artifacts,
        }
    }

    pub(crate) fn artifact_contracts(
        &self,
    ) -> Result<Vec<crate::core::artifact_contract::ArtifactContract>> {
        if self.schema != ARTIFACT_MANIFEST_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "schema",
                format!("expected {ARTIFACT_MANIFEST_SCHEMA}"),
                Some(self.schema.clone()),
                None,
            ));
        }
        self.artifacts
            .iter()
            .map(ArtifactManifestEntry::to_artifact_contract)
            .collect()
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read artifact manifest {}", path.display())),
            )
        })?;
        serde_json::from_str(&raw).map_err(|e| {
            Error::validation_invalid_json(
                e,
                Some(format!("artifact manifest {}", path.display())),
                Some(raw),
            )
        })
    }

    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("create artifact manifest dir {}", parent.display())),
                )
            })?;
        }
        let raw = serde_json::to_string_pretty(self).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("serialize artifact manifest".to_string()),
            )
        })?;
        fs::write(path, format!("{raw}\n")).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("write artifact manifest {}", path.display())),
            )
        })
    }

    pub fn validate_under(
        &self,
        root: impl AsRef<Path>,
    ) -> Result<Vec<ValidatedArtifactManifestEntry>> {
        if self.schema != ARTIFACT_MANIFEST_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "schema",
                format!("expected {ARTIFACT_MANIFEST_SCHEMA}"),
                Some(self.schema.clone()),
                None,
            ));
        }

        let root = root.as_ref();
        let root_canonical = canonicalize_existing_dir(root, "artifact_root")?;
        let mut validated = Vec::with_capacity(self.artifacts.len());
        for entry in &self.artifacts {
            let mut entry = entry.clone();
            validate_entry_shape(&entry)?;
            let absolute_path = confined_existing_file(&root_canonical, &entry.path)?;
            let metadata = fs::metadata(&absolute_path).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!(
                        "read artifact metadata {}",
                        absolute_path.display()
                    )),
                )
            })?;
            let actual_size = metadata.len();
            if let Some(expected_size) = entry.size_bytes {
                if expected_size != actual_size {
                    return Err(Error::validation_invalid_argument(
                        "size_bytes",
                        format!(
                            "declared size for '{}' is {}, actual size is {}",
                            entry.path, expected_size, actual_size
                        ),
                        Some(entry.path.clone()),
                        None,
                    ));
                }
            } else {
                entry.size_bytes = Some(actual_size);
            }

            let actual_sha256 = crate::core::artifact_metadata::sha256_file(&absolute_path)?;
            if let Some(expected_sha256) = &entry.sha256 {
                if expected_sha256 != &actual_sha256 {
                    return Err(Error::validation_invalid_argument(
                        "sha256",
                        format!(
                            "declared sha256 for '{}' does not match file bytes",
                            entry.path
                        ),
                        Some(entry.path.clone()),
                        None,
                    ));
                }
            } else {
                entry.sha256 = Some(actual_sha256);
            }

            if entry.content_type.is_none() {
                entry.content_type =
                    crate::core::artifact_metadata::content_type_from_path(&absolute_path);
            }

            validated.push(ValidatedArtifactManifestEntry {
                entry,
                absolute_path,
            });
        }
        Ok(validated)
    }
}

impl ArtifactManifestEntry {
    pub(crate) fn to_artifact_contract(
        &self,
    ) -> Result<crate::core::artifact_contract::ArtifactContract> {
        validate_entry_shape(self)?;
        let mut extra = std::collections::BTreeMap::new();
        if let Some(id) = &self.id {
            extra.insert("id".to_string(), Value::String(id.clone()));
        }
        if let Some(provenance) = &self.provenance {
            extra.insert(
                "provenance".to_string(),
                serde_json::to_value(provenance).map_err(|e| {
                    Error::internal_json(
                        e.to_string(),
                        Some("serialize artifact provenance".to_string()),
                    )
                })?,
            );
        }
        if let Some(viewer) = &self.viewer {
            extra.insert(
                "viewer".to_string(),
                serde_json::to_value(viewer).map_err(|e| {
                    Error::internal_json(
                        e.to_string(),
                        Some("serialize artifact viewer".to_string()),
                    )
                })?,
            );
        }
        if let Some(public_url_state) = &self.public_url_state {
            extra.insert(
                "public_url_state".to_string(),
                serde_json::to_value(public_url_state).map_err(|e| {
                    Error::internal_json(
                        e.to_string(),
                        Some("serialize artifact public URL state".to_string()),
                    )
                })?,
            );
        }

        Ok(crate::core::artifact_contract::ArtifactContract {
            schema: crate::core::artifact_contract::ARTIFACT_CONTRACT_SCHEMA.to_string(),
            kind: self.kind.clone(),
            artifact_type: "file".to_string(),
            path: Some(self.path.clone()),
            url: None,
            public_url: self.public_url.clone(),
            label: self.label.clone(),
            size_bytes: self.size_bytes,
            sha256: self.sha256.clone(),
            metadata: self.metadata.clone(),
            extra,
        })
    }

    pub fn to_artifact_ref(
        &self,
        id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Result<crate::core::artifact_ref::ArtifactRef> {
        Ok(self.to_artifact_contract()?.to_artifact_ref(id, run_id))
    }
}

fn manifest_path(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join(ARTIFACT_MANIFEST_FILE)
}

pub fn read_manifest_from_root(root: impl AsRef<Path>) -> Result<ArtifactManifest> {
    ArtifactManifest::read(manifest_path(root))
}

pub fn write_manifest_to_root(root: impl AsRef<Path>, manifest: &ArtifactManifest) -> Result<()> {
    manifest.write(manifest_path(root))
}

pub fn manifest_for_existing_files(root: impl AsRef<Path>) -> Result<ArtifactManifest> {
    let root = root.as_ref();
    let root_canonical = canonicalize_existing_dir(root, "artifact_root")?;
    let mut artifacts = Vec::new();
    collect_manifest_entries(&root_canonical, &root_canonical, &mut artifacts)?;
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(ArtifactManifest::new(artifacts))
}

fn collect_manifest_entries(
    root: &Path,
    current: &Path,
    artifacts: &mut Vec<ArtifactManifestEntry>,
) -> Result<()> {
    for entry in fs::read_dir(current).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read artifact directory {}", current.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read artifact directory entry {}",
                    current.display()
                )),
            )
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read artifact metadata {}", path.display())),
            )
        })?;
        if metadata.is_dir() {
            collect_manifest_entries(root, &path, artifacts)?;
        } else if metadata.is_file() {
            let relative = slash_path(path.strip_prefix(root).map_err(|e| {
                Error::internal_unexpected(format!(
                    "artifact path {} escaped root {}: {e}",
                    path.display(),
                    root.display()
                ))
            })?);
            if relative == ARTIFACT_MANIFEST_FILE {
                continue;
            }
            artifacts.push(ArtifactManifestEntry {
                id: None,
                kind: "file".to_string(),
                label: None,
                content_type: crate::core::artifact_metadata::content_type_from_path(&path),
                provenance: None,
                viewer: None,
                public_url: None,
                public_url_state: None,
                size_bytes: Some(metadata.len()),
                sha256: Some(crate::core::artifact_metadata::sha256_file(&path)?),
                redaction: None,
                metadata: serde_json::Value::Object(serde_json::Map::new()),
                path: relative,
            });
        }
    }
    Ok(())
}

fn validate_entry_shape(entry: &ArtifactManifestEntry) -> Result<()> {
    if let Some(id) = &entry.id {
        validate_non_empty("id", id)?;
    }
    validate_non_empty("path", &entry.path)?;
    validate_non_empty("kind", &entry.kind)?;
    if let Some(label) = &entry.label {
        validate_non_empty("label", label)?;
    }
    if let Some(content_type) = &entry.content_type {
        validate_non_empty("content_type", content_type)?;
    }
    if let Some(public_url) = &entry.public_url {
        validate_non_empty("public_url", public_url)?;
    }
    if let Some(provenance) = &entry.provenance {
        validate_optional_non_empty("provenance.producer", provenance.producer.as_deref())?;
        validate_optional_non_empty("provenance.run_id", provenance.run_id.as_deref())?;
        validate_optional_non_empty("provenance.source", provenance.source.as_deref())?;
        if !provenance.metadata.is_object() {
            return Err(Error::validation_invalid_argument(
                "provenance.metadata",
                "must be an object",
                Some(entry.path.clone()),
                None,
            ));
        }
    }
    if let Some(viewer) = &entry.viewer {
        validate_optional_non_empty("viewer.url", viewer.url.as_deref())?;
        for link in &viewer.links {
            validate_non_empty("viewer.links.kind", &link.kind)?;
            validate_non_empty("viewer.links.url", &link.url)?;
        }
    }
    if let Some(public_url_state) = &entry.public_url_state {
        validate_optional_non_empty("public_url_state.url", public_url_state.url.as_deref())?;
        validate_optional_non_empty("public_url_state.error", public_url_state.error.as_deref())?;
    }
    if let Some(sha256) = &entry.sha256 {
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::validation_invalid_argument(
                "sha256",
                "must be a 64-character hex digest",
                Some(entry.path.clone()),
                None,
            ));
        }
    }
    if !entry.metadata.is_object() {
        return Err(Error::validation_invalid_argument(
            "metadata",
            "must be an object",
            Some(entry.path.clone()),
            None,
        ));
    }
    Ok(())
}

fn validate_optional_non_empty(field: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_non_empty(field, value)?;
    }
    Ok(())
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "value cannot be empty",
            None,
            None,
        ));
    }
    Ok(())
}

fn confined_existing_file(root_canonical: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() || relative_path.components().any(disallowed_component) {
        return Err(Error::validation_invalid_argument(
            "path",
            "artifact path must be relative and stay within the artifact root",
            Some(relative.to_string()),
            None,
        ));
    }

    let candidate = root_canonical.join(relative_path);
    let canonical = candidate.canonicalize().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            return Error::validation_invalid_argument(
                "path",
                format!("artifact file not found: {relative}"),
                Some(relative.to_string()),
                None,
            );
        }
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "canonicalize artifact path {}",
                candidate.display()
            )),
        )
    })?;
    if !canonical.starts_with(root_canonical) {
        return Err(Error::validation_invalid_argument(
            "path",
            "artifact path resolves outside the artifact root",
            Some(relative.to_string()),
            None,
        ));
    }
    if !canonical.is_file() {
        return Err(Error::validation_invalid_argument(
            "path",
            "artifact manifest entries must reference files",
            Some(relative.to_string()),
            None,
        ));
    }
    Ok(canonical)
}

fn disallowed_component(component: Component<'_>) -> bool {
    matches!(
        component,
        Component::Prefix(_) | Component::RootDir | Component::ParentDir | Component::CurDir
    )
}

fn canonicalize_existing_dir(path: &Path, field: &str) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("canonicalize artifact root {}", path.display())),
        )
    })?;
    if !canonical.is_dir() {
        return Err(Error::validation_invalid_argument(
            field,
            "must be an existing directory",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    Ok(canonical)
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn default_schema() -> String {
    ARTIFACT_MANIFEST_SCHEMA.to_string()
}

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn is_empty_metadata(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| object.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_confined_relative_file_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("logs")).expect("mkdir");
        fs::write(dir.path().join("logs/output.log"), "hello").expect("write artifact");
        let manifest = ArtifactManifest::new(vec![ArtifactManifestEntry {
            id: None,
            path: "logs/output.log".to_string(),
            kind: "log".to_string(),
            label: Some("Output log".to_string()),
            content_type: None,
            provenance: None,
            viewer: None,
            public_url: None,
            public_url_state: None,
            size_bytes: None,
            sha256: None,
            redaction: Some(ArtifactRedactionState::Redacted),
            metadata: json!({ "phase": "test" }),
        }]);

        let entries = manifest.validate_under(dir.path()).expect("valid manifest");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.size_bytes, Some(5));
        assert_eq!(entries[0].entry.content_type.as_deref(), Some("text/plain"));
        assert_eq!(
            entries[0].entry.sha256.as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert!(entries[0].absolute_path.ends_with("logs/output.log"));
    }

    #[test]
    fn rejects_parent_path_escape_before_touching_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = ArtifactManifest::new(vec![ArtifactManifestEntry {
            id: None,
            path: "../secret.txt".to_string(),
            kind: "log".to_string(),
            label: None,
            content_type: None,
            provenance: None,
            viewer: None,
            public_url: None,
            public_url_state: None,
            size_bytes: None,
            sha256: None,
            redaction: None,
            metadata: json!({}),
        }]);

        let err = manifest
            .validate_under(dir.path())
            .expect_err("escape should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("path"));
        assert!(err.details.to_string().contains("../secret.txt"));
    }

    #[test]
    fn missing_metadata_defaults_to_empty_object() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("output.txt"), "hello").expect("write artifact");
        let manifest: ArtifactManifest = serde_json::from_value(json!({
            "schema": ARTIFACT_MANIFEST_SCHEMA,
            "artifacts": [{
                "path": "output.txt",
                "kind": "log"
            }]
        }))
        .expect("parse manifest");

        let entries = manifest.validate_under(dir.path()).expect("valid manifest");

        assert_eq!(entries[0].entry.metadata, json!({}));
    }

    #[test]
    fn rejects_symlink_escape() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let outside = tempfile::tempdir().expect("outside tempdir");
            let outside_file = outside.path().join("secret.txt");
            fs::write(&outside_file, "secret").expect("write outside");
            std::os::unix::fs::symlink(&outside_file, dir.path().join("link.txt"))
                .expect("symlink");
            let manifest = ArtifactManifest::new(vec![ArtifactManifestEntry {
                id: None,
                path: "link.txt".to_string(),
                kind: "log".to_string(),
                label: None,
                content_type: None,
                provenance: None,
                viewer: None,
                public_url: None,
                public_url_state: None,
                size_bytes: None,
                sha256: None,
                redaction: None,
                metadata: json!({}),
            }]);

            let err = manifest
                .validate_under(dir.path())
                .expect_err("symlink escape should fail");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("resolves outside"));
        }
    }

    #[test]
    fn generates_manifest_for_existing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("nested")).expect("mkdir");
        fs::write(dir.path().join("nested/result.json"), r#"{"ok":true}"#).expect("write result");
        fs::write(dir.path().join(ARTIFACT_MANIFEST_FILE), "{}").expect("write manifest");

        let manifest = manifest_for_existing_files(dir.path()).expect("manifest");

        assert_eq!(manifest.schema, ARTIFACT_MANIFEST_SCHEMA);
        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.artifacts[0].path, "nested/result.json");
        assert_eq!(manifest.artifacts[0].kind, "file");
        assert_eq!(
            manifest.artifacts[0].content_type.as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn validates_descriptor_fields_and_projects_artifact_contracts() {
        let manifest = ArtifactManifest::new(vec![ArtifactManifestEntry {
            id: Some("artifact-1".to_string()),
            path: "reports/summary.json".to_string(),
            kind: "summary".to_string(),
            label: Some("Summary".to_string()),
            content_type: Some("application/json".to_string()),
            provenance: Some(ArtifactManifestProvenance {
                producer: Some("homeboy-extension".to_string()),
                run_id: Some("run-1".to_string()),
                source: Some("invocation".to_string()),
                metadata: json!({ "step": "report" }),
            }),
            viewer: Some(ArtifactManifestViewer {
                url: Some("https://viewer.example.test/report".to_string()),
                links: vec![ArtifactManifestViewerLink {
                    kind: "report-viewer".to_string(),
                    url: "https://viewer.example.test/report".to_string(),
                    replay: Some(json!({ "mode": "summary" })),
                }],
            }),
            public_url: Some("https://artifacts.example.test/reports/summary.json".to_string()),
            public_url_state: Some(ArtifactManifestPublicUrlState {
                url: Some("https://artifacts.example.test/reports/summary.json".to_string()),
                reachable: Some(true),
                status_code: Some(200),
                error: None,
            }),
            size_bytes: Some(17),
            sha256: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ),
            redaction: Some(ArtifactRedactionState::Raw),
            metadata: json!({ "format": "homeboy-proof" }),
        }]);

        let contracts = manifest.artifact_contracts().expect("contracts");

        assert_eq!(contracts.len(), 1);
        assert_eq!(
            contracts[0].schema,
            crate::core::artifact_contract::ARTIFACT_CONTRACT_SCHEMA
        );
        assert_eq!(contracts[0].kind, "summary");
        assert_eq!(contracts[0].path.as_deref(), Some("reports/summary.json"));
        assert_eq!(
            contracts[0].public_url.as_deref(),
            Some("https://artifacts.example.test/reports/summary.json")
        );
        assert_eq!(contracts[0].label.as_deref(), Some("Summary"));
        assert_eq!(contracts[0].size_bytes, Some(17));
        assert_eq!(contracts[0].metadata, json!({ "format": "homeboy-proof" }));
        assert_eq!(contracts[0].extra["id"], "artifact-1");
        assert_eq!(
            contracts[0].extra["provenance"]["producer"],
            "homeboy-extension"
        );
        assert_eq!(
            contracts[0].extra["viewer"]["links"][0]["kind"],
            "report-viewer"
        );
        assert_eq!(contracts[0].extra["public_url_state"]["reachable"], true);
    }

    #[test]
    fn descriptor_projection_rejects_empty_viewer_links() {
        let entry = ArtifactManifestEntry {
            id: None,
            path: "report.json".to_string(),
            kind: "summary".to_string(),
            label: None,
            content_type: None,
            provenance: None,
            viewer: Some(ArtifactManifestViewer {
                url: None,
                links: vec![ArtifactManifestViewerLink {
                    kind: " ".to_string(),
                    url: "https://viewer.example.test/report".to_string(),
                    replay: None,
                }],
            }),
            public_url: None,
            public_url_state: None,
            size_bytes: None,
            sha256: None,
            redaction: None,
            metadata: json!({}),
        };

        let err = entry
            .to_artifact_contract()
            .expect_err("invalid viewer link should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("viewer.links.kind"));
    }
}
