//! The named raw readers of the per-stage Cook delivery artifacts (#16049).
//!
//! Delivery *facts* (candidate identity, fingerprint and patch sha, verified
//! base, gate outcome, applied/verified/published state, commit and pushed
//! sha, PR identity, blocker, base movements) are read from
//! [`CookDelivery`](super::CookDelivery), never from these keys.
//!
//! A few callers need the full artifact a delivery stage persisted, because
//! they execute it again: replaying the exact `AgentTaskPromotionReport` to
//! finalize or resume, authenticating a manual publication dossier against its
//! digest, or handing a finalization receipt back verbatim. They read it here,
//! through exactly one accessor per metadata key, so every remaining raw reader
//! is explicit and greppable (`persisted_`). The delivery writers still write
//! these keys; slice 6b retires them together with these accessors.
//!
//! Each accessor returns the value exactly as stored (a JSON `null` included),
//! so callers keep their own presence rules.

use serde_json::Value;

use crate::agent_task_promotion::AgentTaskPromotionReport;

/// `latest_promotion`: the most recent promotion report.
pub fn persisted_promotion_report(metadata: &Value) -> Option<&Value> {
    metadata.get("latest_promotion")
}

/// `latest_promotion` parsed as the exact report a replay executes. `None`
/// when absent or not a valid report.
pub fn persisted_promotion_report_typed(metadata: &Value) -> Option<AgentTaskPromotionReport> {
    serde_json::from_value(persisted_promotion_report(metadata)?.clone()).ok()
}

/// `promotions`: the append-only history of promotion reports.
pub fn persisted_promotion_history(metadata: &Value) -> Option<&Vec<Value>> {
    metadata.get("promotions").and_then(Value::as_array)
}

/// `cook_finalization`: the publication receipt exactly as the publisher
/// recorded it.
pub fn persisted_finalization_receipt(metadata: &Value) -> Option<&Value> {
    metadata.get("cook_finalization")
}

/// `cook_recovery_source_checkpoint`: the first post-apply promotion
/// checkpoint, kept as immutable recovery authority.
pub fn persisted_recovery_source_checkpoint(metadata: &Value) -> Option<&Value> {
    metadata.get("cook_recovery_source_checkpoint")
}

/// `cook_force_with_lease_receipt`: the lease-guarded push receipt.
pub fn persisted_force_with_lease_receipt(metadata: &Value) -> Option<&Value> {
    metadata.get("cook_force_with_lease_receipt")
}

/// `cook_moving_base_recovery`: the historical moving-base recovery record.
pub fn persisted_moving_base_recovery(metadata: &Value) -> Option<&Value> {
    metadata.get("cook_moving_base_recovery")
}

/// `manual_finalization_identity`: marks a run created for manual publication.
pub fn persisted_manual_finalization_identity(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_identity")
}

/// `manual_finalization_intent`: the validated manual publication dossier.
pub fn persisted_manual_finalization_intent(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_intent")
}

/// `manual_finalization_intent_digest`: integrity digest of the dossier.
pub fn persisted_manual_finalization_intent_digest(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_intent_digest")
}

/// `manual_finalization_failure`: the failed manual publication receipt.
pub fn persisted_manual_finalization_failure(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_failure")
}

/// `manual_finalization_retry`: a preflight dossier marked retryable.
pub fn persisted_manual_finalization_retry(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_retry")
}

/// `manual_finalization_retry_candidate`: the retry's candidate fingerprint.
pub fn persisted_manual_finalization_retry_candidate(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_retry_candidate")
}

/// `manual_finalization_retry_origin`: the retry candidate a completed
/// receipt consumed.
pub fn persisted_manual_finalization_retry_origin(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_retry_origin")
}

/// `manual_finalization_receipt_digest`: digest of the completed receipt.
pub fn persisted_manual_finalization_receipt_digest(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_receipt_digest")
}

/// `manual_finalization_receipt_intent_digest`: the intent digest the
/// completed receipt was bound to.
pub fn persisted_manual_finalization_receipt_intent_digest(metadata: &Value) -> Option<&Value> {
    metadata.get("manual_finalization_receipt_intent_digest")
}
