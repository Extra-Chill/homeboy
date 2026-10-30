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

/// The most recent promotion report.
pub fn recorded_promotion_report(metadata: &Value) -> Option<&Value> {
    recorded_artifact(metadata, "/promotion")
}

/// [`recorded_promotion_report`] parsed as the exact report a replay
/// executes. `None` when absent or not a valid report.
pub fn recorded_promotion_report_typed(metadata: &Value) -> Option<AgentTaskPromotionReport> {
    let value = homeboy_core::observation::store::metadata_payloads::resolve_metadata_payload(
        recorded_promotion_report(metadata)?,
        &homeboy_core::paths::artifact_root().ok()?,
    )
    .ok()?;
    serde_json::from_value(value).ok()
}

/// The append-only history of promotion reports.
pub fn recorded_promotion_history(metadata: &Value) -> Option<&Vec<Value>> {
    recorded_artifact(metadata, "/promotions").and_then(Value::as_array)
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
