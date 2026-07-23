use serde::Serialize;
use sha2::{Digest, Sha256};

pub const SOURCE_SNAPSHOT_METADATA_ENV: &str = "HOMEBOY_SOURCE_SNAPSHOT_JSON";
pub const LAB_OFFLOAD_METADATA_ENV: &str = "HOMEBOY_LAB_OFFLOAD_JSON";
pub const PREVIEW_METADATA_ENV: &str = "HOMEBOY_PREVIEW_JSON";
pub const PREVIEW_PUBLIC_URL_ENV: &str = "HOMEBOY_PREVIEW_PUBLIC_URL";
pub const PROVENANCE_REFERENCE_SCHEMA: &str = "homeboy/provenance-reference/v1";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunContext {
    pub provenance: RunProvenance,
}

impl RunContext {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_provenance(provenance: RunProvenance) -> Self {
        Self { provenance }
    }

    pub fn subprocess_compatibility_from_env() -> Self {
        Self::from_provenance(RunProvenance {
            source_snapshot: env_json(SOURCE_SNAPSHOT_METADATA_ENV),
            lab_offload: env_json(LAB_OFFLOAD_METADATA_ENV),
            preview: preview_metadata_from_env(),
            artifact_mirror: None,
        })
    }

    pub fn with_missing_from(mut self, fallback: Self) -> Self {
        self.provenance = self.provenance.with_missing_from(fallback.provenance);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunProvenance {
    pub source_snapshot: Option<serde_json::Value>,
    pub lab_offload: Option<serde_json::Value>,
    pub preview: Option<serde_json::Value>,
    pub artifact_mirror: Option<serde_json::Value>,
}

impl RunProvenance {
    pub fn with_source_snapshot(mut self, source_snapshot: impl Serialize) -> Self {
        self.source_snapshot = serde_json::to_value(source_snapshot).ok();
        self
    }

    pub fn with_lab_offload(mut self, lab_offload: impl Serialize) -> Self {
        self.lab_offload = serde_json::to_value(lab_offload).ok();
        self
    }

    pub fn with_preview(mut self, preview: impl Serialize) -> Self {
        self.preview = serde_json::to_value(preview).ok();
        self
    }

    pub fn with_artifact_mirror(mut self, artifact_mirror: impl Serialize) -> Self {
        self.artifact_mirror = serde_json::to_value(artifact_mirror).ok();
        self
    }

    fn with_missing_from(mut self, fallback: Self) -> Self {
        if self.source_snapshot.is_none() {
            self.source_snapshot = fallback.source_snapshot;
        }
        if self.lab_offload.is_none() {
            self.lab_offload = fallback.lab_offload;
        }
        if self.preview.is_none() {
            self.preview = fallback.preview;
        }
        if self.artifact_mirror.is_none() {
            self.artifact_mirror = fallback.artifact_mirror;
        }
        self
    }
}

pub fn env_json(name: &str) -> Option<serde_json::Value> {
    std::env::var(name)
        .ok()
        .and_then(|raw| resolve_json_value(&raw))
}

/// Resolve inline JSON or a content-addressed provenance file reference. The
/// reference keeps `execve` bounded without changing the established env names.
pub fn resolve_json_value(raw: &str) -> Option<serde_json::Value> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(PROVENANCE_REFERENCE_SCHEMA)
    {
        return Some(value);
    }

    let path = value.get("path")?.as_str()?;
    let expected_hash = value.get("sha256")?.as_str()?;
    let contents = std::fs::read(path).ok()?;
    let actual_hash = format!("{:x}", Sha256::digest(&contents));
    (actual_hash == expected_hash)
        .then(|| serde_json::from_slice::<serde_json::Value>(&contents).ok())
        .flatten()
}

fn preview_metadata_from_env() -> Option<serde_json::Value> {
    let mut preview = env_json(PREVIEW_METADATA_ENV)?;
    if let Ok(public_url) = std::env::var(PREVIEW_PUBLIC_URL_ENV) {
        if !public_url.trim().is_empty() {
            if let Some(object) = preview.as_object_mut() {
                object
                    .entry("public_url")
                    .or_insert_with(|| serde_json::Value::String(public_url));
            }
        }
    }
    Some(preview)
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_json_value, RunContext, PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV,
        PROVENANCE_REFERENCE_SCHEMA,
    };
    use sha2::{Digest, Sha256};

    #[test]
    fn subprocess_context_reads_generic_preview_metadata_with_public_url_overlay() {
        std::env::set_var(
            PREVIEW_METADATA_ENV,
            r#"{"schema":"homeboy/preview/v1","local_url":"http://127.0.0.1:8080","hold_seconds":600,"expires_at":"2026-06-01T22:00:00Z","status":"running","process_id":"pid-123","runtime_id":"runtime-abc","cleanup_status":"pending"}"#,
        );
        std::env::set_var(PREVIEW_PUBLIC_URL_ENV, "https://preview.example.test/run-1");

        let context = RunContext::subprocess_compatibility_from_env();
        let preview = context
            .provenance
            .preview
            .expect("preview metadata should be captured");

        assert_eq!(preview["schema"], "homeboy/preview/v1");
        assert_eq!(preview["local_url"], "http://127.0.0.1:8080");
        assert_eq!(preview["public_url"], "https://preview.example.test/run-1");
        assert_eq!(preview["hold_seconds"], 600);
        assert_eq!(preview["status"], "running");
        assert_eq!(preview["process_id"], "pid-123");
        assert_eq!(preview["runtime_id"], "runtime-abc");
        assert_eq!(preview["cleanup_status"], "pending");

        std::env::remove_var(PREVIEW_METADATA_ENV);
        std::env::remove_var(PREVIEW_PUBLIC_URL_ENV);
    }

    #[test]
    fn provenance_resolver_preserves_inline_json_and_rejects_tampered_references() {
        let inline = r#"{"schema":"fixture/v1","value":"inline"}"#;
        assert_eq!(
            resolve_json_value(inline),
            serde_json::from_str(inline).ok()
        );

        let directory = tempfile::tempdir().expect("provenance directory");
        let path = directory.path().join("payload.json");
        let payload = br#"{"schema":"fixture/v1","value":"staged"}"#;
        std::fs::write(&path, payload).expect("stage provenance");
        let reference = serde_json::json!({
            "schema": PROVENANCE_REFERENCE_SCHEMA,
            "path": path,
            "sha256": format!("{:x}", Sha256::digest(payload)),
        })
        .to_string();

        assert_eq!(
            resolve_json_value(&reference),
            serde_json::from_slice(payload).ok()
        );

        std::fs::write(&path, br#"{"schema":"fixture/v1","value":"tampered"}"#)
            .expect("tamper provenance");
        assert_eq!(resolve_json_value(&reference), None);
    }
}
