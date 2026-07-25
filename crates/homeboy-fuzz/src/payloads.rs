//! Bounded inline fuzz evidence backed by content-addressed payload files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::FuzzCampaign;

/// Values at or above this size are persisted once and represented inline by a ref.
pub const INLINE_FUZZ_PAYLOAD_LIMIT_BYTES: usize = 64 * 1024;
pub const FUZZ_PAYLOAD_ARTIFACT_KIND: &str = "fuzz_payload";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzPayload {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

/// Replace oversized strings in a runner campaign with typed artifact references.
///
/// Payload bytes are written under the runner artifact root, once per SHA-256.
/// The replacement shape is intentionally ordinary JSON metadata, so readers from
/// older Homeboy versions retain the campaign schema and can ignore the ref.
pub fn externalize_fuzz_campaign_payloads(
    campaign: &mut FuzzCampaign,
    artifact_root: &Path,
) -> homeboy_core::Result<Vec<FuzzPayload>> {
    let mut value = serde_json::to_value(&*campaign).map_err(|error| {
        homeboy_core::Error::internal_unexpected(format!(
            "failed to encode fuzz campaign for payload externalization: {error}"
        ))
    })?;
    let mut payloads = BTreeMap::new();
    externalize_value(&mut value, artifact_root, &mut payloads)?;
    *campaign = serde_json::from_value(value).map_err(|error| {
        homeboy_core::Error::internal_unexpected(format!(
            "failed to decode bounded fuzz campaign: {error}"
        ))
    })?;
    Ok(payloads.into_values().collect())
}

fn externalize_value(
    value: &mut Value,
    artifact_root: &Path,
    payloads: &mut BTreeMap<String, FuzzPayload>,
) -> homeboy_core::Result<()> {
    match value {
        Value::String(body) if body.len() >= INLINE_FUZZ_PAYLOAD_LIMIT_BYTES => {
            let sha256 = sha256(body.as_bytes());
            let payload = if let Some(payload) = payloads.get(&sha256) {
                payload.clone()
            } else {
                let id = format!("fuzz-payload-{sha256}");
                let path = artifact_root.join(format!("{id}.txt"));
                homeboy_core::io::write_output_file_atomically(
                    &path,
                    body.as_bytes(),
                    homeboy_core::io::OutputWriteOptions::artifact(),
                )
                .map_err(|error| {
                    homeboy_core::Error::internal_io(
                        error.to_string(),
                        Some(path.display().to_string()),
                    )
                })?;
                let payload = FuzzPayload {
                    id,
                    sha256: sha256.clone(),
                    size_bytes: body.len() as u64,
                    path,
                };
                payloads.insert(sha256.clone(), payload.clone());
                payload
            };
            *value = serde_json::json!({
                "schema": "homeboy/fuzz-payload-ref/v1",
                "artifact_id": payload.id,
                "sha256": payload.sha256,
                "size_bytes": payload.size_bytes,
                "summary": { "kind": "text" },
            });
        }
        Value::Array(values) => {
            for value in values {
                externalize_value(value, artifact_root, payloads)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                externalize_value(value, artifact_root, payloads)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn externalizes_repeated_payload_once_without_retaining_inline_text() {
        let dir = tempfile::tempdir().expect("temp dir");
        let body = "x".repeat(INLINE_FUZZ_PAYLOAD_LIMIT_BYTES);
        let mut campaign: FuzzCampaign = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/fuzz-campaign/v1", "version": 1, "id": "campaign",
            "safety_class": "read_only", "metadata": { "stdout": body, "nested": { "stdout": body } }
        }))
        .expect("campaign");

        let payloads =
            externalize_fuzz_campaign_payloads(&mut campaign, dir.path()).expect("externalize");
        assert_eq!(payloads.len(), 1);
        assert_eq!(
            std::fs::read(&payloads[0].path).expect("payload").len(),
            INLINE_FUZZ_PAYLOAD_LIMIT_BYTES
        );
        assert_eq!(
            campaign.metadata.pointer("/stdout/artifact_id"),
            campaign.metadata.pointer("/nested/stdout/artifact_id")
        );
        assert!(
            serde_json::to_string(&campaign).expect("json").len()
                < INLINE_FUZZ_PAYLOAD_LIMIT_BYTES / 8
        );
        assert!(std::fs::read_dir(dir.path())
            .expect("read artifact root")
            .all(|entry| !entry
                .expect("artifact entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp")));
    }

    #[test]
    fn bounds_many_projection_layers_to_one_payload_body() {
        let dir = tempfile::tempdir().expect("temp dir");
        let body = "o".repeat(500 * 1024);
        let mut projections = serde_json::Map::new();
        for index in 0..24 {
            projections.insert(format!("projection-{index}"), Value::String(body.clone()));
        }
        let mut campaign: FuzzCampaign = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/fuzz-campaign/v1", "version": 1, "id": "campaign",
            "safety_class": "read_only", "metadata": projections
        }))
        .expect("campaign");

        let payloads =
            externalize_fuzz_campaign_payloads(&mut campaign, dir.path()).expect("externalize");
        let bounded_bytes = serde_json::to_vec(&campaign).expect("campaign json").len();
        let persisted_bytes = bounded_bytes
            + std::fs::metadata(&payloads[0].path)
                .expect("payload metadata")
                .len() as usize;

        assert_eq!(payloads.len(), 1);
        assert!(bounded_bytes < 16 * 1024);
        assert!(persisted_bytes < body.len() + 16 * 1024);
    }

    #[test]
    fn retry_and_deep_nesting_reuse_the_same_content_addressed_ref() {
        let dir = tempfile::tempdir().expect("temp dir");
        let body = "r".repeat(INLINE_FUZZ_PAYLOAD_LIMIT_BYTES + 1);
        let campaign_value = serde_json::json!({
            "schema": "homeboy/fuzz-campaign/v1", "version": 1, "id": "campaign",
            "safety_class": "read_only",
            "metadata": { "a": { "b": { "c": { "result": body } } } }
        });
        let mut first: FuzzCampaign =
            serde_json::from_value(campaign_value.clone()).expect("first campaign");
        let mut retry: FuzzCampaign =
            serde_json::from_value(campaign_value).expect("retry campaign");

        let first_payloads =
            externalize_fuzz_campaign_payloads(&mut first, dir.path()).expect("first");
        let retry_payloads =
            externalize_fuzz_campaign_payloads(&mut retry, dir.path()).expect("retry");

        assert_eq!(first_payloads, retry_payloads);
        assert_eq!(std::fs::read_dir(dir.path()).expect("read dir").count(), 1);
        assert_eq!(
            first
                .metadata
                .pointer("/a/b/c/result/schema")
                .and_then(Value::as_str),
            Some("homeboy/fuzz-payload-ref/v1")
        );
    }

    #[test]
    fn preserves_legacy_small_and_private_metadata_without_expansion() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut campaign: FuzzCampaign = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/fuzz-campaign/v1", "version": 1, "id": "campaign",
            "safety_class": "read_only", "metadata": { "token": "redacted", "stdout": "short" }
        }))
        .expect("campaign");
        let before = campaign.clone();

        assert!(
            externalize_fuzz_campaign_payloads(&mut campaign, dir.path())
                .expect("externalize")
                .is_empty()
        );
        assert_eq!(campaign, before);
        assert_eq!(std::fs::read_dir(dir.path()).expect("read dir").count(), 0);
    }
}
