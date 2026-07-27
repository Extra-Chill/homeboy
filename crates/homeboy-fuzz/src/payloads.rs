//! Bounded, schema-aware fuzz evidence backed by content-addressed payload files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::FuzzCampaign;

pub const INLINE_FUZZ_PAYLOAD_LIMIT_BYTES: usize = 64 * 1024;
pub const FUZZ_PAYLOAD_ARTIFACT_KIND: &str = "fuzz_payload";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuzzPayloadBudget {
    pub max_payload_bytes: usize,
    pub max_aggregate_bytes: usize,
    pub max_distinct_payloads: usize,
}

impl Default for FuzzPayloadBudget {
    fn default() -> Self {
        Self {
            max_payload_bytes: 1024 * 1024,
            max_aggregate_bytes: 4 * 1024 * 1024,
            max_distinct_payloads: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzPayload {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FuzzPayloadExternalization {
    pub payloads: Vec<FuzzPayload>,
    pub refused_payloads: usize,
    pub refused_bytes: u64,
}

/// Externalize only contract-declared `Value` and extension fields. Typed strings
/// such as IDs, schemas, labels, paths, and status values are never traversed.
pub fn externalize_fuzz_campaign_payloads(
    campaign: &mut FuzzCampaign,
    artifact_root: &Path,
    run_id: &str,
    budget: FuzzPayloadBudget,
) -> homeboy_core::Result<FuzzPayloadExternalization> {
    let mut state = State {
        artifact_root,
        run_scope: sha256(run_id.as_bytes()),
        budget,
        payloads: BTreeMap::new(),
        refused_payloads: 0,
        refused_bytes: 0,
        aggregate_bytes: 0,
    };
    externalize_value(&mut campaign.metadata, &mut state)?;
    externalize_map(&mut campaign.extra, &mut state)?;
    for case in &mut campaign.cases {
        externalize_value(&mut case.input, &mut state)?;
        externalize_value(&mut case.expected, &mut state)?;
        externalize_value(&mut case.observed, &mut state)?;
        externalize_value(&mut case.metadata, &mut state)?;
        externalize_map(&mut case.extra, &mut state)?;
    }
    for artifact in &mut campaign.artifacts {
        externalize_value(&mut artifact.metadata, &mut state)?;
        externalize_map(&mut artifact.extra, &mut state)?;
    }
    for finding in &mut campaign.findings {
        externalize_value(&mut finding.metadata, &mut state)?;
        externalize_map(&mut finding.extra, &mut state)?;
    }
    for threshold in &mut campaign.thresholds {
        externalize_value(&mut threshold.metadata, &mut state)?;
        externalize_map(&mut threshold.extra, &mut state)?;
    }
    if let Some(provenance) = &mut campaign.provenance {
        externalize_value(&mut provenance.metadata, &mut state)?;
        externalize_map(&mut provenance.extra, &mut state)?;
    }
    Ok(FuzzPayloadExternalization {
        payloads: state.payloads.into_values().collect(),
        refused_payloads: state.refused_payloads,
        refused_bytes: state.refused_bytes,
    })
}

struct State<'a> {
    artifact_root: &'a Path,
    run_scope: String,
    budget: FuzzPayloadBudget,
    payloads: BTreeMap<String, FuzzPayload>,
    aggregate_bytes: usize,
    refused_payloads: usize,
    refused_bytes: u64,
}

fn externalize_map(
    values: &mut BTreeMap<String, Value>,
    state: &mut State<'_>,
) -> homeboy_core::Result<()> {
    for value in values.values_mut() {
        externalize_value(value, state)?;
    }
    Ok(())
}

fn externalize_value(value: &mut Value, state: &mut State<'_>) -> homeboy_core::Result<()> {
    match value {
        Value::String(body) if body.len() >= INLINE_FUZZ_PAYLOAD_LIMIT_BYTES => {
            let size_bytes = body.len();
            let sha256 = sha256(body.as_bytes());
            if let Some(payload) = state.payloads.get(&sha256) {
                *value = payload_ref(payload, "stored", None);
                return Ok(());
            }
            let refusal = if size_bytes > state.budget.max_payload_bytes {
                Some("per_payload_bytes")
            } else if state.aggregate_bytes.saturating_add(size_bytes)
                > state.budget.max_aggregate_bytes
            {
                Some("aggregate_bytes")
            } else if state.payloads.len() >= state.budget.max_distinct_payloads {
                Some("distinct_payloads")
            } else {
                None
            };
            if let Some(reason) = refusal {
                state.refused_payloads += 1;
                state.refused_bytes += size_bytes as u64;
                *value = refusal_ref(&sha256, size_bytes as u64, reason);
                return Ok(());
            }
            let id = format!("fuzz-payload-{}-{sha256}", &state.run_scope[..16]);
            let path = state
                .artifact_root
                .join(format!("fuzz-payload-{sha256}.txt"));
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
                size_bytes: size_bytes as u64,
                path,
            };
            state.aggregate_bytes += size_bytes;
            state.payloads.insert(sha256, payload.clone());
            *value = payload_ref(&payload, "stored", None);
        }
        Value::Array(values) => {
            for value in values {
                externalize_value(value, state)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                externalize_value(value, state)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn payload_ref(payload: &FuzzPayload, status: &str, reason: Option<&str>) -> Value {
    serde_json::json!({
        "schema": "homeboy/fuzz-payload-ref/v1", "status": status,
        "artifact_id": payload.id, "sha256": payload.sha256,
        "size_bytes": payload.size_bytes, "summary": { "kind": "text" }, "reason": reason,
    })
}

fn refusal_ref(sha256: &str, size_bytes: u64, reason: &str) -> Value {
    serde_json::json!({
        "schema": "homeboy/fuzz-payload-ref/v1", "status": "refused",
        "sha256": sha256, "size_bytes": size_bytes,
        "summary": { "kind": "text", "body_retained": false }, "reason": reason,
    })
}

fn sha256(bytes: &[u8]) -> String {
    homeboy_engine_primitives::content_hash::sha256_hex(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn campaign(metadata: Value) -> FuzzCampaign {
        serde_json::from_value(serde_json::json!({
            "schema":"homeboy/fuzz-campaign/v1", "version":1, "id":"campaign",
            "safety_class":"read_only", "metadata":metadata
        }))
        .expect("campaign")
    }

    #[test]
    fn bounds_repeated_500k_payload_across_many_metadata_projections() {
        let dir = tempfile::tempdir().expect("dir");
        let body = "x".repeat(500 * 1024);
        let metadata = (0..24)
            .map(|i| (format!("p{i}"), Value::String(body.clone())))
            .collect();
        let mut campaign = campaign(Value::Object(metadata));
        let result = externalize_fuzz_campaign_payloads(
            &mut campaign,
            dir.path(),
            "run-a",
            FuzzPayloadBudget::default(),
        )
        .expect("externalize");
        let persisted = serde_json::to_vec(&campaign).expect("json").len()
            + std::fs::metadata(&result.payloads[0].path)
                .expect("metadata")
                .len() as usize;
        assert_eq!(result.payloads.len(), 1);
        assert!(persisted < body.len() + 16 * 1024);
    }

    #[test]
    fn preserves_typed_long_ids_and_schema_strings() {
        let dir = tempfile::tempdir().expect("dir");
        let long = "i".repeat(INLINE_FUZZ_PAYLOAD_LIMIT_BYTES);
        let mut campaign: FuzzCampaign = serde_json::from_value(serde_json::json!({
            "schema":long, "version":1, "id":long, "title":long, "safety_class":"read_only",
            "metadata":{"stdout":"x".repeat(65536)}
        }))
        .expect("typed campaign");
        let original = (
            campaign.schema.clone(),
            campaign.id.clone(),
            campaign.title.clone(),
        );
        externalize_fuzz_campaign_payloads(
            &mut campaign,
            dir.path(),
            "run-a",
            FuzzPayloadBudget::default(),
        )
        .expect("externalize");
        assert_eq!((campaign.schema, campaign.id, campaign.title), original);
    }

    #[test]
    fn scopes_identical_content_to_each_run_and_refuses_unique_budget_overflow() {
        let dir = tempfile::tempdir().expect("dir");
        let body = "x".repeat(65536);
        let mut first = campaign(serde_json::json!({"stdout":body}));
        let mut second = campaign(serde_json::json!({"stdout":body}));
        let a = externalize_fuzz_campaign_payloads(
            &mut first,
            &dir.path().join("a"),
            "run-a",
            FuzzPayloadBudget::default(),
        )
        .expect("first");
        let b = externalize_fuzz_campaign_payloads(
            &mut second,
            &dir.path().join("b"),
            "run-b",
            FuzzPayloadBudget::default(),
        )
        .expect("second");
        assert_ne!(a.payloads[0].id, b.payloads[0].id);
        assert_eq!(a.payloads[0].sha256, b.payloads[0].sha256);
        let mut many = campaign(Value::Array(
            (0..3)
                .map(|i| Value::String(format!("{i}{}", "x".repeat(65536))))
                .collect(),
        ));
        let budget = FuzzPayloadBudget {
            max_distinct_payloads: 1,
            ..FuzzPayloadBudget::default()
        };
        let result = externalize_fuzz_campaign_payloads(
            &mut many,
            &dir.path().join("many"),
            "run-many",
            budget,
        )
        .expect("many");
        assert_eq!(result.payloads.len(), 1);
        assert_eq!(result.refused_payloads, 2);
        assert_eq!(
            many.metadata.pointer("/1/status").and_then(Value::as_str),
            Some("refused")
        );

        let mut per_payload = campaign(serde_json::json!({"stdout": body}));
        let per_payload_result = externalize_fuzz_campaign_payloads(
            &mut per_payload,
            &dir.path().join("per-payload"),
            "run-per-payload",
            FuzzPayloadBudget {
                max_payload_bytes: 65535,
                ..FuzzPayloadBudget::default()
            },
        )
        .expect("per-payload budget");
        assert_eq!(per_payload_result.refused_payloads, 1);
        assert_eq!(
            per_payload
                .metadata
                .pointer("/stdout/reason")
                .and_then(Value::as_str),
            Some("per_payload_bytes")
        );

        let mut aggregate =
            campaign(serde_json::json!({"one":body, "two": format!("y{}", "x".repeat(65535))}));
        let aggregate_result = externalize_fuzz_campaign_payloads(
            &mut aggregate,
            &dir.path().join("aggregate"),
            "run-aggregate",
            FuzzPayloadBudget {
                max_aggregate_bytes: 65536,
                ..FuzzPayloadBudget::default()
            },
        )
        .expect("aggregate budget");
        assert_eq!(aggregate_result.refused_payloads, 1);
        assert_eq!(
            aggregate
                .metadata
                .pointer("/two/reason")
                .and_then(Value::as_str),
            Some("aggregate_bytes")
        );
    }

    #[test]
    fn keeps_small_private_values_and_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().expect("dir");
        let mut campaign = campaign(serde_json::json!({"private":"redacted", "stdout":"short"}));
        let before = campaign.clone();
        assert!(externalize_fuzz_campaign_payloads(
            &mut campaign,
            dir.path(),
            "run-a",
            FuzzPayloadBudget::default()
        )
        .expect("externalize")
        .payloads
        .is_empty());
        assert_eq!(campaign, before);
        assert!(std::fs::read_dir(dir.path())
            .expect("entries")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp")));
    }
}
