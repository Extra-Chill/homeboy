//! Durable exactly-once cook side-effect operation claims.
//!
//! Cook continuation persists enough intent to resume after a reconciled
//! attempt, but the external side effects it drives — promotion, retry
//! dispatch, and PR finalization — record their result only *after* the effect
//! completes. A controller crash in the window between "external effect
//! executed" and "result durably recorded" can repeat the effect on restart
//! (double push, duplicate PR, re-dispatched retry) (#8357).
//!
//! This module provides the lifecycle-owned durable operation claim the cook
//! side-effect boundary reserves *before* performing an external effect. A claim
//! is keyed by `(run_id, operation_key)` where `operation_key` covers the
//! promotion report identity, retry run id, or finalization candidate
//! fingerprint. Its states are:
//!
//! - `pending`/`running` — leased: an in-flight effect owns the claim. A second
//!   controller pass observes [`ClaimOutcome::LeaseHeld`] and must reconcile the
//!   lease (via Git/PR lookup) rather than blindly repeating the effect.
//! - `completed` — an immutable result the operation produced. A resumed
//!   controller observes [`ClaimOutcome::AlreadyCompleted`] and returns the
//!   recorded result without re-running the effect.
//!
//! The claim is written through the same atomic [`store::mutate_record`]
//! read-modify-write used by the rest of the lifecycle, so concurrent
//! controller passes converge on one owner. The primitive is deliberately
//! generic and product/framework agnostic: callers supply the operation key and
//! the completed result value; this module owns only claim identity and
//! lifecycle.

use std::time::Duration;

use serde_json::{json, Value};

use super::*;

/// Metadata key under which the durable operation-claim ledger is stored on a
/// run record. Each entry is keyed by its `operation_key`.
const OPERATION_CLAIMS_KEY: &str = "cook_operation_claims";

/// Result of attempting to claim a cook side-effect operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This pass acquired a fresh lease and must perform the external effect,
    /// then call [`complete_cook_operation`].
    Acquired,
    /// A prior pass already completed this operation. The immutable recorded
    /// result is returned; the caller must not repeat the effect.
    AlreadyCompleted(Value),
    /// A concurrent (or interrupted) pass holds a still-fresh lease. The caller
    /// must not perform the effect; it should wait or reconcile the lease.
    LeaseHeld,
}

/// State of a single durable operation claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Leased and in flight (no result recorded yet).
    Running,
    /// Completed with an immutable recorded result.
    Completed,
}

/// A durable operation claim projected for reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationClaim {
    pub operation_key: String,
    pub state: ClaimState,
    pub leased_at: String,
    pub lease_deadline: Option<String>,
    pub result: Option<Value>,
}

/// Reserve a cook side-effect operation before an external effect.
///
/// Writes a durable `running` lease keyed by `operation_key` when none exists,
/// returning [`ClaimOutcome::Acquired`]. If the operation already completed, the
/// recorded result is returned via [`ClaimOutcome::AlreadyCompleted`] and no
/// lease is taken. If a still-fresh lease is held by another pass,
/// [`ClaimOutcome::LeaseHeld`] is returned. A lease whose deadline has elapsed
/// is reclaimable and is re-leased to this pass (the caller is expected to
/// reconcile any partially-applied external effect via Git/PR lookup first).
pub fn claim_cook_operation(
    run_id: &str,
    operation_key: &str,
    lease: Duration,
) -> Result<ClaimOutcome> {
    let run_id = sanitize_run_id(run_id);
    let now = now_timestamp();
    let lease_deadline = timestamp_after(&now, lease);
    let mut outcome = ClaimOutcome::LeaseHeld;
    store::mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let claims = metadata
            .entry(OPERATION_CLAIMS_KEY.to_string())
            .or_insert_with(|| json!([]));
        if !claims.is_array() {
            *claims = json!([]);
        }
        let claims = claims.as_array_mut().expect("operation claims array");

        if let Some(existing) = claims
            .iter()
            .find(|claim| claim["operation_key"] == json!(operation_key))
        {
            // A completed claim is immutable: return its recorded result.
            if existing["state"] == json!("completed") {
                outcome = ClaimOutcome::AlreadyCompleted(
                    existing.get("result").cloned().unwrap_or(Value::Null),
                );
                return false;
            }
            // A still-fresh lease is owned by another pass.
            if !lease_is_expired(existing, &now) {
                outcome = ClaimOutcome::LeaseHeld;
                return false;
            }
            // Expired lease: reclaim it for this pass.
        }

        let claim = json!({
            "operation_key": operation_key,
            "state": "running",
            "leased_at": now,
            "lease_deadline": lease_deadline,
        });
        // Replace an expired lease in place, or append a new one.
        if let Some(slot) = claims
            .iter_mut()
            .find(|claim| claim["operation_key"] == json!(operation_key))
        {
            *slot = claim;
        } else {
            claims.push(claim);
        }
        record.updated_at = Some(now.clone());
        outcome = ClaimOutcome::Acquired;
        true
    })?;
    Ok(outcome)
}

/// Record the immutable result of a completed cook side-effect operation.
///
/// Transitions the `(run_id, operation_key)` claim to `completed` and stores the
/// result. Idempotent: completing an already-completed claim leaves the first
/// recorded result intact. Returns an error if no claim was ever reserved (a
/// terminal result without its durable lease is a lifecycle invariant break,
/// mirroring [`record_provider_execution_terminal`]).
pub fn complete_cook_operation(run_id: &str, operation_key: &str, result: Value) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    let now = now_timestamp();
    let mut found = false;
    store::mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let Some(claims) = metadata
            .get_mut(OPERATION_CLAIMS_KEY)
            .and_then(Value::as_array_mut)
        else {
            return false;
        };
        let Some(claim) = claims
            .iter_mut()
            .find(|claim| claim["operation_key"] == json!(operation_key))
        else {
            return false;
        };
        found = true;
        // Completed results are immutable: the first result wins.
        if claim["state"] == json!("completed") {
            return false;
        }
        claim["state"] = json!("completed");
        claim["completed_at"] = json!(now);
        claim["result"] = result.clone();
        record.updated_at = Some(now.clone());
        true
    })?;
    if !found {
        return Err(Error::internal_unexpected(
            "cook operation completed without its durable claim; reserve the operation before performing its side effect",
        ));
    }
    Ok(())
}

/// Read a single operation claim for reconciliation, if present.
pub fn operation_claim(run_id: &str, operation_key: &str) -> Result<Option<OperationClaim>> {
    let run_id = sanitize_run_id(run_id);
    let record = store::read_record(&run_id)?;
    Ok(record
        .metadata
        .get(OPERATION_CLAIMS_KEY)
        .and_then(Value::as_array)
        .and_then(|claims| {
            claims
                .iter()
                .find(|claim| claim["operation_key"] == json!(operation_key))
        })
        .and_then(project_claim))
}

/// Whether the `(run_id, operation_key)` operation has a still-fresh in-flight
/// lease that another controller pass owns. Callers use this to decide between
/// waiting and reconciling an interrupted effect via Git/PR lookup.
pub fn operation_lease_is_active(run_id: &str, operation_key: &str) -> Result<bool> {
    let now = now_timestamp();
    Ok(
        operation_claim(run_id, operation_key)?.is_some_and(|claim| {
            claim.state == ClaimState::Running
                && claim
                    .lease_deadline
                    .as_deref()
                    .is_none_or(|deadline| deadline > now.as_str())
        }),
    )
}

fn project_claim(claim: &Value) -> Option<OperationClaim> {
    let operation_key = claim.get("operation_key")?.as_str()?.to_string();
    let state = match claim.get("state").and_then(Value::as_str)? {
        "completed" => ClaimState::Completed,
        _ => ClaimState::Running,
    };
    Some(OperationClaim {
        operation_key,
        state,
        leased_at: claim
            .get("leased_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        lease_deadline: claim
            .get("lease_deadline")
            .and_then(Value::as_str)
            .map(str::to_string),
        result: claim.get("result").cloned(),
    })
}

/// A leased claim is expired when its deadline has elapsed relative to `now`. A
/// claim without a deadline never expires on its own (it must be completed or
/// explicitly reconciled).
fn lease_is_expired(claim: &Value, now: &str) -> bool {
    claim
        .get("lease_deadline")
        .and_then(Value::as_str)
        .is_some_and(|deadline| deadline <= now)
}

/// RFC3339 timestamp `lease` after `base`. Falls back to `base` when the base is
/// unparseable so a claim is never handed an earlier-than-now deadline.
fn timestamp_after(base: &str, lease: Duration) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(base).ok()?;
    let deadline = parsed + chrono::Duration::from_std(lease).ok()?;
    Some(deadline.to_rfc3339())
}
