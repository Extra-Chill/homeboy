//! The named readers of the full artifacts the `cook_delivery` record holds
//! (#16049).
//!
//! Delivery *facts* (candidate identity, fingerprint and patch sha, verified
//! base, gate outcome, applied/verified/published state, commit and pushed
//! sha, PR identity, blocker, base movements) are read from
//! [`CookDelivery`](super::CookDelivery), never from these artifacts.
//!
//! A few callers need the full artifact a delivery step recorded, because
//! they execute it again: replaying the exact `AgentTaskPromotionReport` to
//! finalize or resume, authenticating a manual publication dossier against its
//! digest, or handing a finalization receipt back verbatim. They read it here,
//! through exactly one accessor per artifact of the record, so every remaining
//! raw reader is explicit and greppable (`recorded_`).

use std::path::Path;

use homeboy_core::observation::store::metadata_payloads;
use homeboy_core::{Error, Result};
use serde_json::Value;

use super::cook_delivery::{COOK_DELIVERY_KEY, COOK_DELIVERY_SCHEMA};
use crate::agent_task_promotion::AgentTaskPromotionReport;

/// The artifact at `pointer` inside the run's `cook_delivery` record.
fn recorded_artifact<'a>(metadata: &'a Value, pointer: &str) -> Option<&'a Value> {
    metadata
        .get(COOK_DELIVERY_KEY)
        .filter(|record| {
            record.get("schema").and_then(Value::as_str) == Some(COOK_DELIVERY_SCHEMA)
        })?
        .get("artifacts")?
        .pointer(pointer)
        .filter(|artifact| !artifact.is_null())
}

/// The most recent promotion report, with any externalized payload resolved
/// from the ambient artifact root.
///
/// Run metadata stores a large report as a content-addressed
/// `metadata_payload` reference (#16627), so the raw value is not a report.
/// Every reader goes through here so none of them parses the reference.
pub fn recorded_promotion_report(metadata: &Value) -> Result<Option<Value>> {
    recorded_artifact(metadata, "/promotion")
        .map(|report| resolve_recorded(report, None, "promotion"))
        .transpose()
}

/// [`recorded_promotion_report`] resolved against a store's own artifact root.
pub fn recorded_promotion_report_at(metadata: &Value, root: &Path) -> Result<Option<Value>> {
    recorded_artifact(metadata, "/promotion")
        .map(|report| resolve_recorded(report, Some(root), "promotion"))
        .transpose()
}

/// [`recorded_promotion_report`] parsed as the exact report a replay
/// executes. `None` when absent; an error names why it cannot be read.
pub fn recorded_promotion_report_typed(
    metadata: &Value,
) -> Result<Option<AgentTaskPromotionReport>> {
    recorded_promotion_report(metadata)?
        .map(parse_promotion_report)
        .transpose()
}

/// Parse a resolved promotion report, keeping the serde cause.
pub fn parse_promotion_report(value: Value) -> Result<AgentTaskPromotionReport> {
    serde_json::from_value(value).map_err(|error| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion",
            format!("persisted Cook promotion report is invalid: {error}"),
            None,
            None,
        )
    })
}

/// The append-only history of promotion reports, each resolved.
pub fn recorded_promotion_history(metadata: &Value) -> Result<Option<Vec<Value>>> {
    let Some(history) = recorded_artifact(metadata, "/promotions").and_then(Value::as_array) else {
        return Ok(None);
    };
    history
        .iter()
        .map(|report| resolve_recorded(report, None, "promotions"))
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

/// Resolve `metadata_payload` references in `value`. The artifact root is only
/// looked up when a reference exists, so inline reports need no data root.
fn resolve_recorded(value: &Value, root: Option<&Path>, artifact: &str) -> Result<Value> {
    if !holds_payload_reference(value) {
        return Ok(value.clone());
    }
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => homeboy_core::paths::artifact_root()?,
    };
    metadata_payloads::resolve_metadata_payload(value, &root).map_err(|error| {
        Error::validation_invalid_argument(
            format!("cook_delivery.artifacts.{artifact}"),
            format!(
                "externalized Cook {artifact} payload under {} could not be read: {}",
                root.join("metadata-payloads").display(),
                error.message
            ),
            None,
            None,
        )
    })
}

fn holds_payload_reference(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(metadata_payloads::REFERENCE)
                || object.values().any(holds_payload_reference)
        }
        Value::Array(array) => array.iter().any(holds_payload_reference),
        _ => false,
    }
}

/// The publication receipt exactly as the publisher recorded it.
pub fn recorded_finalization_receipt(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/finalization")
}

/// `manual_finalization_identity`: marks a run reserved for manual
/// publication. It is the run reservation's identity, written before any
/// delivery step runs, so it stays a run-metadata key of its own.
pub fn persisted_manual_finalization_identity(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_identity")
}

/// The validated manual publication dossier.
pub fn recorded_manual_finalization_intent(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/manual/intent")
}

/// Integrity digest of the manual publication dossier.
pub fn recorded_manual_finalization_intent_digest(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/manual/intent_digest")
}

/// The preflight dossier was marked a retryable direct publication.
pub fn recorded_manual_finalization_retry(metadata: &Value) -> bool {
    recorded_artifact(metadata, "/manual/retry") == Some(&Value::Bool(true))
}

/// The manual publication retry's candidate fingerprint.
pub fn recorded_manual_finalization_retry_candidate(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/manual/retry_candidate")
}

/// The retry candidate a completed manual receipt consumed.
pub fn recorded_manual_finalization_retry_origin(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/manual/retry_origin")
}

/// Digest of the completed manual receipt.
pub fn recorded_manual_finalization_receipt_digest(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/manual/receipt_digest")
}

/// The intent digest the completed manual receipt was bound to.
pub fn recorded_manual_finalization_receipt_intent_digest(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/manual/receipt_intent_digest")
}
