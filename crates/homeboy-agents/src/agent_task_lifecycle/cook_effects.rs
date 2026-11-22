//! Cook's idempotent side effects share the control-plane outbox ownership primitive.
use std::{sync::mpsc, thread, time::Duration};

use homeboy_control_plane_contract::*;
use homeboy_core::observation::ControlPlaneResourceProjection;
use serde_json::{json, Value};

use super::*;

#[derive(Debug)]
pub enum ClaimOutcome {
    Acquired(CookEffectLease),
    AlreadyCompleted(Value),
    LeaseHeld,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ClaimState {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationClaim {
    pub operation_key: String,
    pub state: ClaimState,
    pub lease_deadline: Option<String>,
    pub result: Option<Value>,
}

/// Scoped renewal: all exits, including errors and unwinding, stop the heartbeat.
#[derive(Debug)]
pub struct CookEffectLease {
    store: AgentTaskLifecycleStore,
    effect: EffectId,
    fence: u64,
    _heartbeat: EffectLeaseHeartbeat,
}

/// Keeps an owned outbox lease alive until its result has been persisted.
#[derive(Debug)]
pub(crate) struct EffectLeaseHeartbeat {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for EffectLeaseHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl EffectLeaseHeartbeat {
    pub(crate) fn start(
        store: AgentTaskLifecycleStore,
        effect: EffectId,
        fence: u64,
        lease: Duration,
    ) -> Self {
        let (stop, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while receiver.recv_timeout(lease / 3) == Err(mpsc::RecvTimeoutError::Timeout) {
                let renewed = store.open_observation_initialized().and_then(|db| {
                    db.renew_control_plane_effect_lease(&effect, fence, &deadline(lease))
                });
                if !matches!(renewed, Ok(true)) {
                    break;
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl CookEffectLease {
    pub fn complete(&self, result: Value) -> Result<()> {
        self.terminalize(result, false)
    }
    pub fn fail(&self, result: Value) -> Result<()> {
        self.terminalize(result, true)
    }
    fn terminalize(&self, result: Value, failed: bool) -> Result<()> {
        terminalize(&self.store, &self.effect, self.fence, result, failed)
    }
}

fn effect_id(run: &str, operation: &str) -> EffectId {
    action_effect_id("cook", &sanitize_run_id(run), "operation", operation)
}
// Failed executions remain immutable. An explicit retry gets the next
// deterministic attempt identity; readers continue to see the latest result.
fn latest_effect(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
    retry: bool,
) -> Result<EffectId> {
    let db = store.open_observation_initialized()?;
    let base = effect_id(run, operation);
    let mut effect = base.clone();
    for attempt in 1u64.. {
        let Some(status) = db.control_plane_effect_status(&effect)? else {
            return Ok(effect);
        };
        if !status
            .terminal
            .as_ref()
            .is_some_and(|t| t.acknowledgement.outcome == ControlPlaneActionOutcome::Failed)
        {
            return Ok(effect);
        }
        let next = action_effect_id("cook", &base.0, "retry", &attempt.to_string());
        if !retry && db.control_plane_effect_status(&next)?.is_none() {
            return Ok(effect);
        }
        effect = next;
    }
    unreachable!()
}

fn deadline(lease: Duration) -> String {
    (chrono::Utc::now() + chrono::Duration::from_std(lease).expect("bounded lease"))
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

pub fn claim_cook_operation_in_store(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
    lease: Duration,
) -> Result<ClaimOutcome> {
    claim_operation_with_intent_in_store(store, run, operation, lease, &Value::Null)
}

pub fn claim_operation_with_intent_in_store(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
    lease: Duration,
    intent: &Value,
) -> Result<ClaimOutcome> {
    let run = sanitize_run_id(run);
    let record = store.read_record(&run)?;
    let run = RunId::new(run).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    let effect = latest_effect(store, run.as_str(), operation, true)?;
    let lease = lease.clamp(Duration::from_millis(60), Duration::from_secs(60));
    let db = store.open_observation_initialized()?;
    let request = ControlPlaneActionRequest {
        schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.into(),
        effect_id: effect.clone(),
        action: ControlPlaneAction::Reconcile,
        idempotency_key: effect.0.clone(),
        actor: "cook".into(),
        expected_updated_at: None,
        parameters: ControlPlaneActionPayload {
            schema: "homeboy/cook-operation/v1".into(),
            data: json!({"operation_key": operation, "intent": intent}),
        },
        confirmed: true,
    };
    let digest = homeboy_engine_primitives::content_hash::sha256_hex(
        serde_json::to_string(&request)
            .expect("request JSON")
            .as_bytes(),
    );
    let intent = ControlPlaneActionIntent {
        schema: CONTROL_PLANE_ACTION_INTENT_SCHEMA.into(),
        effect_id: effect.clone(),
        resource: ControlPlaneActionResource {
            resource: ControlPlaneRef::Run(run.clone()),
            run: run.clone(),
            original_alias: None,
        },
        request,
        request_digest: digest,
        accepted_at: now_timestamp(),
    };
    let projection = ControlPlaneResourceProjection {
        resource_type: "cook_operation".into(),
        resource_id: run.to_string(),
        version: record.updated_at.unwrap_or(record.submitted_at),
        state: "available".into(),
        aliases: vec![],
        eligibility: json!({}),
        provenance: json!({}),
    };
    db.enqueue_control_plane_action_intent_with_projection(
        &intent,
        &projection,
        &homeboy_engine_primitives::content_hash::sha256_hex(effect.0.as_bytes()),
        |p, _| {
            Ok(ControlPlaneActionFence {
                schema: CONTROL_PLANE_ACTION_FENCE_SCHEMA.into(),
                resource_updated_at: p.version.clone(),
                eligible: true,
                reason: None,
            })
        },
    )?;
    let now = deadline(Duration::ZERO);
    db.expire_control_plane_effect_leases(&now)?;
    let status = db
        .control_plane_effect_status(&effect)?
        .expect("enqueued effect");
    if let Some(terminal) = status.terminal {
        return Ok(ClaimOutcome::AlreadyCompleted(
            terminal.acknowledgement.result.data,
        ));
    }
    // Expired effects are recovered by rerunning this content-addressed,
    // idempotent domain operation, never by weakening generic outbox expiry.
    let leased = if status.recovery_required {
        db.claim_control_plane_effect_recovery_by_id(&effect, "cook", &now, &deadline(lease))?
    } else {
        db.lease_control_plane_effect_by_id(&effect, "cook", &now, &deadline(lease))?
    };
    let Some(leased) = leased else {
        return Ok(ClaimOutcome::LeaseHeld);
    };
    let fence = leased.lease_fence;
    let heartbeat = EffectLeaseHeartbeat::start(store.clone(), effect.clone(), fence, lease);
    Ok(ClaimOutcome::Acquired(CookEffectLease {
        store: store.clone(),
        effect,
        fence,
        _heartbeat: heartbeat,
    }))
}

fn terminalize(
    store: &AgentTaskLifecycleStore,
    effect: &EffectId,
    fence: u64,
    result: Value,
    failed: bool,
) -> Result<()> {
    let db = store.open_observation_initialized()?;
    let status = db
        .control_plane_effect_status(effect)?
        .ok_or_else(|| Error::internal_unexpected("operation has no outbox effect"))?;
    if status.terminal.is_some() {
        return Ok(());
    }
    let now = now_timestamp();
    let intent = status.intent;
    let terminal = ControlPlaneEffectTerminal {
        schema: CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA.into(),
        completed_at: now.clone(),
        acknowledgement: ControlPlaneActionAcknowledgement {
            schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.into(),
            acknowledgement: effect.0.clone(),
            run: intent.resource.run.clone(),
            action: intent.request.action,
            idempotency_key: intent.request.idempotency_key,
            actor: "cook".into(),
            accepted_at: intent.accepted_at,
            completed_at: now.clone(),
            outcome: if failed {
                ControlPlaneActionOutcome::Failed
            } else {
                ControlPlaneActionOutcome::Succeeded
            },
            resource: ControlPlaneRun::new(intent.resource.run),
            result: ControlPlaneActionPayload {
                schema: "homeboy/cook-operation-result/v1".into(),
                data: result,
            },
            message: None,
        },
        audit: ControlPlaneEffectAudit {
            schema: CONTROL_PLANE_EFFECT_AUDIT_SCHEMA.into(),
            observed_at: now,
            evidence: json!({}),
        },
    };
    // The effect is content-addressed: a concurrent owner or crash recovery may
    // terminalize it between the read above and this write. That is success.
    if let Err(error) = db.terminalize_control_plane_effect(effect, fence, &terminal) {
        if db
            .control_plane_effect_status(effect)?
            .is_some_and(|status| status.terminal.is_some())
        {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

pub fn complete_cook_operation_in_store(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
    result: Value,
) -> Result<()> {
    let effect = latest_effect(store, run, operation, false)?;
    let status = store
        .open_observation_initialized()?
        .control_plane_effect_status(&effect)?
        .ok_or_else(|| Error::internal_unexpected("operation has no outbox effect"))?;
    terminalize(store, &effect, status.lease_fence, result, false)
}
pub fn fail_cook_operation_in_store(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
    result: Value,
) -> Result<()> {
    let effect = latest_effect(store, run, operation, false)?;
    let status = store
        .open_observation_initialized()?
        .control_plane_effect_status(&effect)?
        .ok_or_else(|| Error::internal_unexpected("operation has no outbox effect"))?;
    terminalize(store, &effect, status.lease_fence, result, true)
}
#[cfg(test)]
pub(crate) fn operation_claim(run: &str, operation: &str) -> Result<Option<OperationClaim>> {
    operation_claim_in_store(
        &AgentTaskLifecycleStore::from_environment()?,
        run,
        operation,
    )
}
pub fn operation_claim_in_store(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
) -> Result<Option<OperationClaim>> {
    Ok(store
        .open_observation_initialized()?
        .control_plane_effect_status(&latest_effect(store, run, operation, false)?)?
        .map(|status| {
            let state = match status.terminal.as_ref().map(|t| t.acknowledgement.outcome) {
                Some(ControlPlaneActionOutcome::Failed) => ClaimState::Failed,
                Some(_) => ClaimState::Completed,
                None => ClaimState::Running,
            };
            OperationClaim {
                operation_key: operation.into(),
                state,
                lease_deadline: status.lease_expires_at,
                result: status.terminal.map(|t| t.acknowledgement.result.data),
            }
        }))
}
pub fn operation_lease_is_active_in_store(
    store: &AgentTaskLifecycleStore,
    run: &str,
    operation: &str,
) -> Result<bool> {
    Ok(
        operation_claim_in_store(store, run, operation)?.is_some_and(|c| {
            c.state == ClaimState::Running
                && c.lease_deadline
                    .is_some_and(|d| d > deadline(Duration::ZERO))
        }),
    )
}
