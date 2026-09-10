use homeboy_control_plane_contract::{
    ControlPlaneActionAcknowledgement, ControlPlaneActionFence, ControlPlaneActionIntent,
    ControlPlaneEffectState, ControlPlaneEffectTerminal, EffectId, RunId,
};
use rusqlite::{params, OptionalExtension};

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneActionClaim {
    Acquired { accepted_at: String },
    Recover { accepted_at: String },
    InProgress,
    Completed(ControlPlaneActionAcknowledgement),
}

/// Authoritative persisted state for one effect. `Leased` deliberately says
/// only that a worker may have crossed the external-effect crash window; it is
/// not evidence that the effect happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlaneEffectStatus {
    pub state: ControlPlaneEffectState,
    pub intent: ControlPlaneActionIntent,
    pub fence: ControlPlaneActionFence,
    pub lease_fence: u64,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<String>,
    pub terminal: Option<ControlPlaneEffectTerminal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneEffectAdmission {
    Enqueued(ControlPlaneEffectStatus),
    Duplicate(ControlPlaneEffectStatus),
}

impl ObservationStore {
    /// Transaction boundary: verify the durable run version and eligibility,
    /// then persist the immutable intent and pending outbox row together.
    pub fn enqueue_control_plane_action_intent(
        &self,
        intent: &ControlPlaneActionIntent,
        fence: &ControlPlaneActionFence,
        idempotency_digest: &str,
    ) -> Result<ControlPlaneEffectAdmission> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error("begin control-plane action intent admission"))?;
        let result = (|| {
            if !fence.eligible {
                return Err(Error::validation_invalid_argument(
                    "action",
                    fence
                        .reason
                        .clone()
                        .unwrap_or_else(|| "action is not eligible".to_string()),
                    None,
                    None,
                ));
            }
            let updated_at: Option<String> = self
                .connection
                .query_row(
                    "SELECT COALESCE(finished_at, started_at) FROM runs WHERE id = ?1",
                    [intent.resource.run.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_error("read control-plane action resource fence"))?;
            let Some(updated_at) = updated_at else {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "control-plane run not found",
                    Some(intent.resource.run.to_string()),
                    None,
                ));
            };
            if updated_at != fence.resource_updated_at {
                return Err(Error::validation_invalid_argument(
                    "expected_updated_at",
                    "run changed since the supplied eligibility fence",
                    Some(updated_at),
                    None,
                ));
            }
            if let Some(existing) =
                self.effect_status_by_idempotency(intent.resource.run.as_str(), idempotency_digest)?
            {
                if existing.intent.request_digest != intent.request_digest {
                    return Err(Error::validation_invalid_argument(
                        "idempotency_key",
                        "control-plane action idempotency key was already used for different input",
                        None,
                        None,
                    ));
                }
                return Ok(ControlPlaneEffectAdmission::Duplicate(existing));
            }
            let encoded_intent = serde_json::to_string(intent)
                .map_err(|e| Error::internal_json(e.to_string(), None))?;
            let encoded_fence = serde_json::to_string(fence)
                .map_err(|e| Error::internal_json(e.to_string(), None))?;
            self.connection.execute(
                "INSERT INTO control_plane_action_claims(run_id, idempotency_digest, request_digest, state, owner_pid, accepted_at, intent_json, fence_json, effect_id, outbox_state) VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7, ?8, 'pending')",
                params![intent.resource.run.as_str(), idempotency_digest, intent.request_digest, std::process::id(), intent.accepted_at, encoded_intent, encoded_fence, intent.effect_id.0],
            ).map_err(sqlite_error("persist control-plane action intent and outbox"))?;
            self.effect_status(&intent.effect_id)?
                .ok_or_else(|| {
                    Error::internal_unexpected("persisted action intent was not readable")
                })
                .map(ControlPlaneEffectAdmission::Enqueued)
        })();
        match result {
            Ok(value) => {
                self.connection
                    .execute_batch("COMMIT")
                    .map_err(sqlite_error("commit control-plane action intent admission"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Transaction boundary: select a pending or expired lease, increment its
    /// fence, and persist the new owner before returning it to the worker.
    pub fn lease_control_plane_effect(
        &self,
        owner: &str,
        now: &str,
        expires_at: &str,
    ) -> Result<Option<ControlPlaneEffectStatus>> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error("begin control-plane effect lease"))?;
        let result = (|| {
            let effect: Option<String> = self.connection.query_row(
                "SELECT effect_id FROM control_plane_action_claims WHERE outbox_state = 'pending' OR (outbox_state = 'leased' AND lease_expires_at <= ?1) ORDER BY accepted_at, effect_id LIMIT 1", [now], |row| row.get(0),
            ).optional().map_err(sqlite_error("select leaseable control-plane effect"))?;
            let Some(effect) = effect else {
                return Ok(None);
            };
            self.connection.execute(
                "UPDATE control_plane_action_claims SET outbox_state = 'leased', lease_owner = ?2, lease_fence = lease_fence + 1, lease_expires_at = ?3 WHERE effect_id = ?1",
                params![effect, owner, expires_at],
            ).map_err(sqlite_error("lease control-plane effect"))?;
            self.effect_status(&EffectId(effect))
        })();
        match result {
            Ok(value) => {
                self.connection
                    .execute_batch("COMMIT")
                    .map_err(sqlite_error("commit control-plane effect lease"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Transaction boundary: a matching lease fence records terminal outcome,
    /// acknowledgement, and audit evidence as one durable fact.
    pub fn terminalize_control_plane_effect(
        &self,
        effect_id: &EffectId,
        lease_fence: u64,
        terminal: &ControlPlaneEffectTerminal,
    ) -> Result<ControlPlaneEffectStatus> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error("begin control-plane effect terminalization"))?;
        let result = (|| {
            let encoded = serde_json::to_string(terminal)
                .map_err(|e| Error::internal_json(e.to_string(), None))?;
            let audit = serde_json::to_string(&terminal.audit)
                .map_err(|e| Error::internal_json(e.to_string(), None))?;
            let changed = self.connection.execute(
                "UPDATE control_plane_action_claims SET state = 'completed', outbox_state = 'terminal', terminal_json = ?3, audit_json = ?4, acknowledgement_json = ?5, completed_at = ?6 WHERE effect_id = ?1 AND outbox_state = 'leased' AND lease_fence = ?2",
                params![effect_id.0, lease_fence, encoded, audit, serde_json::to_string(&terminal.acknowledgement).map_err(|e| Error::internal_json(e.to_string(), None))?, terminal.completed_at],
            ).map_err(sqlite_error("terminalize control-plane effect"))?;
            if changed != 1 {
                return Err(Error::validation_invalid_argument(
                    "lease_fence",
                    "control-plane effect lease is stale or terminal",
                    Some(effect_id.0.clone()),
                    None,
                ));
            }
            self.effect_status(effect_id)?.ok_or_else(|| {
                Error::internal_unexpected("terminal control-plane effect was not readable")
            })
        })();
        match result {
            Ok(value) => {
                self.connection
                    .execute_batch("COMMIT")
                    .map_err(sqlite_error("commit control-plane effect terminalization"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn control_plane_effect_status(
        &self,
        effect_id: &EffectId,
    ) -> Result<Option<ControlPlaneEffectStatus>> {
        self.effect_status(effect_id)
    }

    fn effect_status_by_idempotency(
        &self,
        run: &str,
        idempotency_digest: &str,
    ) -> Result<Option<ControlPlaneEffectStatus>> {
        let effect: Option<String> = self.connection.query_row("SELECT effect_id FROM control_plane_action_claims WHERE run_id = ?1 AND idempotency_digest = ?2", params![run, idempotency_digest], |row| row.get(0)).optional().map_err(sqlite_error("read control-plane idempotency claim"))?;
        effect
            .map(EffectId)
            .map_or(Ok(None), |id| self.effect_status(&id))
    }

    fn effect_status(&self, effect_id: &EffectId) -> Result<Option<ControlPlaneEffectStatus>> {
        let row: Option<(String, String, String, u64, Option<String>, Option<String>, Option<String>)> = self.connection.query_row(
            "SELECT intent_json, fence_json, outbox_state, lease_fence, lease_owner, lease_expires_at, terminal_json FROM control_plane_action_claims WHERE effect_id = ?1", [&effect_id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        ).optional().map_err(sqlite_error("read control-plane effect status"))?;
        let Some((intent, fence, state, lease_fence, lease_owner, lease_expires_at, terminal)) =
            row
        else {
            return Ok(None);
        };
        Ok(Some(ControlPlaneEffectStatus {
            state: match state.as_str() {
                "pending" => ControlPlaneEffectState::Pending,
                "leased" => ControlPlaneEffectState::Leased,
                "terminal" => ControlPlaneEffectState::Terminal,
                _ => {
                    return Err(Error::internal_unexpected(
                        "invalid control-plane effect state",
                    ))
                }
            },
            intent: serde_json::from_str(&intent)
                .map_err(|e| Error::internal_json(e.to_string(), None))?,
            fence: serde_json::from_str(&fence)
                .map_err(|e| Error::internal_json(e.to_string(), None))?,
            lease_fence,
            lease_owner,
            lease_expires_at,
            terminal: terminal
                .map(|value| {
                    serde_json::from_str(&value)
                        .map_err(|e| Error::internal_json(e.to_string(), None))
                })
                .transpose()?,
        }))
    }
    pub fn existing_control_plane_action(
        &self,
        run: &RunId,
        idempotency_digest: &str,
        request_digest: &str,
    ) -> Result<Option<ControlPlaneActionClaim>> {
        let stored: Option<(String, String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT request_digest, state, acknowledgement_json \
                 FROM control_plane_action_claims \
                 WHERE run_id = ?1 AND idempotency_digest = ?2",
                params![run.as_str(), idempotency_digest],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sqlite_error("read completed control-plane action"))?;
        let Some((stored_digest, state, acknowledgement)) = stored else {
            return Ok(None);
        };
        if stored_digest != request_digest {
            return Err(Error::validation_invalid_argument(
                "idempotency_key",
                "control-plane action idempotency key was already used for different input",
                None,
                None,
            ));
        }
        if state != "completed" {
            return Ok(Some(ControlPlaneActionClaim::InProgress));
        }
        serde_json::from_str(&acknowledgement.ok_or_else(|| {
            Error::internal_unexpected("completed control-plane action has no acknowledgement")
        })?)
        .map(ControlPlaneActionClaim::Completed)
        .map(Some)
        .map_err(|error| Error::internal_json(error.to_string(), None))
    }

    pub fn claim_control_plane_action(
        &self,
        run: &RunId,
        idempotency_digest: &str,
        request_digest: &str,
    ) -> Result<ControlPlaneActionClaim> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error("begin control-plane action claim"))?;
        let result = (|| {
            let exists: bool = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runs WHERE id = ?1)",
                    [run.as_str()],
                    |row| row.get(0),
                )
                .map_err(sqlite_error("verify control-plane action run"))?;
            if !exists {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "control-plane run not found",
                    Some(run.to_string()),
                    None,
                ));
            }

            let existing: Option<(String, String, u32, Option<String>, String, Option<String>)> = self
                .connection
                .query_row(
                    "SELECT request_digest, state, owner_pid, owner_start_identity_json, accepted_at, acknowledgement_json \
                     FROM control_plane_action_claims \
                     WHERE run_id = ?1 AND idempotency_digest = ?2",
                    params![run.as_str(), idempotency_digest],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
                )
                .optional()
                .map_err(sqlite_error("read control-plane action claim"))?;
            if let Some((
                stored_digest,
                state,
                owner_pid,
                owner_identity,
                accepted_at,
                acknowledgement,
            )) = existing
            {
                if stored_digest != request_digest {
                    return Err(Error::validation_invalid_argument(
                        "idempotency_key",
                        "control-plane action idempotency key was already used for different input",
                        None,
                        None,
                    ));
                }
                if state == "completed" {
                    let acknowledgement = acknowledgement.ok_or_else(|| {
                        Error::internal_unexpected(
                            "completed control-plane action has no acknowledgement",
                        )
                    })?;
                    return serde_json::from_str(&acknowledgement)
                        .map(ControlPlaneActionClaim::Completed)
                        .map_err(|error| Error::internal_json(error.to_string(), None));
                }
                let owner_identity = owner_identity
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| Error::internal_json(error.to_string(), None))?;
                if matches!(
                    crate::process::process_identity_state_with_start_identity(
                        owner_pid,
                        None,
                        owner_identity.as_ref(),
                    ),
                    crate::process::ProcessIdentityState::Live
                        | crate::process::ProcessIdentityState::Unverifiable
                ) {
                    return Ok(ControlPlaneActionClaim::InProgress);
                }
                let current_identity = crate::process::process_start_identity(std::process::id())
                    .map_err(Error::internal_unexpected)?
                    .map(|identity| serde_json::to_string(&identity))
                    .transpose()
                    .map_err(|error| Error::internal_json(error.to_string(), None))?;
                self.connection
                    .execute(
                        "UPDATE control_plane_action_claims \
                             SET owner_pid = ?3, owner_start_identity_json = ?4 \
                         WHERE run_id = ?1 AND idempotency_digest = ?2",
                        params![
                            run.as_str(),
                            idempotency_digest,
                            std::process::id(),
                            current_identity
                        ],
                    )
                    .map_err(sqlite_error("adopt interrupted control-plane action"))?;
                return Ok(ControlPlaneActionClaim::Recover { accepted_at });
            }

            let accepted_at = chrono::Utc::now().to_rfc3339();
            let owner_identity = crate::process::process_start_identity(std::process::id())
                .map_err(Error::internal_unexpected)?
                .map(|identity| serde_json::to_string(&identity))
                .transpose()
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            self.connection
                    .execute(
                        "INSERT INTO control_plane_action_claims(\
                        run_id, idempotency_digest, request_digest, state, owner_pid, owner_start_identity_json, accepted_at\
                     ) VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6)",
                        params![
                            run.as_str(),
                            idempotency_digest,
                            request_digest,
                            std::process::id(),
                            owner_identity,
                            accepted_at,
                        ],
                    )
                    .map_err(sqlite_error("persist control-plane action claim"))?;
            Ok(ControlPlaneActionClaim::Acquired { accepted_at })
        })();
        match result {
            Ok(claim) => {
                self.connection
                    .execute_batch("COMMIT")
                    .map_err(sqlite_error("commit control-plane action claim"))?;
                Ok(claim)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn complete_control_plane_action(
        &self,
        run: &RunId,
        idempotency_digest: &str,
        acknowledgement: &ControlPlaneActionAcknowledgement,
    ) -> Result<ControlPlaneActionAcknowledgement> {
        let encoded = serde_json::to_string(acknowledgement)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        let completed_at = chrono::Utc::now().to_rfc3339();
        let changed = self
            .connection
            .execute(
                "UPDATE control_plane_action_claims \
                 SET state = 'completed', acknowledgement_json = ?3, completed_at = ?4 \
                 WHERE run_id = ?1 AND idempotency_digest = ?2 AND state = 'running'",
                params![run.as_str(), idempotency_digest, encoded, completed_at],
            )
            .map_err(sqlite_error("complete control-plane action"))?;
        if changed == 1 {
            return Ok(acknowledgement.clone());
        }
        let stored: Option<String> = self
            .connection
            .query_row(
                "SELECT acknowledgement_json FROM control_plane_action_claims \
                 WHERE run_id = ?1 AND idempotency_digest = ?2 AND state = 'completed'",
                params![run.as_str(), idempotency_digest],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_error("read completed control-plane action"))?;
        serde_json::from_str(&stored.ok_or_else(|| {
            Error::internal_unexpected("control-plane action completed without its durable claim")
        })?)
        .map_err(|error| Error::internal_json(error.to_string(), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::{
        ControlPlaneAction, ControlPlaneActionFence, ControlPlaneActionIntent,
        ControlPlaneActionOutcome, ControlPlaneActionPayload, ControlPlaneActionRequest,
        ControlPlaneActionResource, ControlPlaneEffectAudit, ControlPlaneEffectState,
        ControlPlaneEffectTerminal, ControlPlaneRef, ControlPlaneRun, EffectId,
        CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA, CONTROL_PLANE_ACTION_FENCE_SCHEMA,
        CONTROL_PLANE_ACTION_INTENT_SCHEMA, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
        CONTROL_PLANE_EFFECT_AUDIT_SCHEMA, CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA,
    };

    #[test]
    fn action_claim_replays_one_immutable_acknowledgement() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            ObservationStore::open_initialized_at(directory.path().join("store.sqlite")).unwrap();
        store.connection.execute("INSERT INTO runs(id, kind, started_at, status) VALUES ('run-1', 'deploy', 'now', 'running')", []).unwrap();
        let run = RunId::new("run-1").unwrap();
        let accepted_at = match store
            .claim_control_plane_action(&run, &"a".repeat(64), &"b".repeat(64))
            .unwrap()
        {
            ControlPlaneActionClaim::Acquired { accepted_at } => accepted_at,
            claim => panic!("unexpected claim: {claim:?}"),
        };
        let acknowledgement = ControlPlaneActionAcknowledgement {
            schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
            acknowledgement: "run-1:action:resume:key".to_string(),
            run: run.clone(),
            action: ControlPlaneAction::Resume,
            idempotency_key: "key".to_string(),
            actor: "test".to_string(),
            accepted_at,
            completed_at: "later".to_string(),
            outcome: ControlPlaneActionOutcome::Succeeded,
            resource: ControlPlaneRun::new(run.clone()),
            result: ControlPlaneActionPayload::empty(),
            message: None,
        };
        store
            .complete_control_plane_action(&run, &"a".repeat(64), &acknowledgement)
            .unwrap();
        assert_eq!(
            store
                .claim_control_plane_action(&run, &"a".repeat(64), &"b".repeat(64))
                .unwrap(),
            ControlPlaneActionClaim::Completed(acknowledgement)
        );
    }

    #[test]
    fn action_claim_recovers_only_after_its_process_owner_is_dead() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            ObservationStore::open_initialized_at(directory.path().join("store.sqlite")).unwrap();
        store.connection.execute("INSERT INTO runs(id, kind, started_at, status) VALUES ('run-1', 'deploy', 'now', 'running')", []).unwrap();
        let run = RunId::new("run-1").unwrap();
        assert!(matches!(
            store
                .claim_control_plane_action(&run, &"a".repeat(64), &"b".repeat(64))
                .unwrap(),
            ControlPlaneActionClaim::Acquired { .. }
        ));
        assert_eq!(
            store
                .claim_control_plane_action(&run, &"a".repeat(64), &"b".repeat(64))
                .unwrap(),
            ControlPlaneActionClaim::InProgress
        );
        store
            .connection
            .execute(
                "UPDATE control_plane_action_claims \
                 SET owner_pid = 2147483647, owner_start_identity_json = NULL",
                [],
            )
            .unwrap();
        assert!(matches!(
            store
                .claim_control_plane_action(&run, &"a".repeat(64), &"b".repeat(64))
                .unwrap(),
            ControlPlaneActionClaim::Recover { .. }
        ));
    }

    #[test]
    fn outbox_admission_reclaim_fence_and_terminal_acknowledgement_are_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        store.connection.execute("INSERT INTO runs(id, kind, started_at, status) VALUES ('run-1', 'deploy', 'now', 'running')", []).unwrap();
        let run = RunId::new("run-1").unwrap();
        let intent = ControlPlaneActionIntent {
            schema: CONTROL_PLANE_ACTION_INTENT_SCHEMA.to_string(),
            effect_id: EffectId("effect-1".to_string()),
            resource: ControlPlaneActionResource {
                resource: ControlPlaneRef::Run(run.clone()),
                run: run.clone(),
                original_alias: Some("legacy-run-1".to_string()),
            },
            request: ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Resume,
                idempotency_key: "key".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: false,
            },
            request_digest: "b".repeat(64),
            accepted_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let fence = ControlPlaneActionFence {
            schema: CONTROL_PLANE_ACTION_FENCE_SCHEMA.to_string(),
            resource_updated_at: "now".to_string(),
            eligible: true,
            reason: None,
        };
        assert!(matches!(
            store
                .enqueue_control_plane_action_intent(&intent, &fence, &"a".repeat(64))
                .unwrap(),
            ControlPlaneEffectAdmission::Enqueued(_)
        ));
        drop(store);
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        assert!(matches!(
            store
                .enqueue_control_plane_action_intent(&intent, &fence, &"a".repeat(64))
                .unwrap(),
            ControlPlaneEffectAdmission::Duplicate(_)
        ));
        let first = store
            .lease_control_plane_effect("worker-1", "2026-01-01T00:00:01Z", "2026-01-01T00:00:02Z")
            .unwrap()
            .unwrap();
        assert_eq!(first.state, ControlPlaneEffectState::Leased);
        let reclaimed = store
            .lease_control_plane_effect("worker-2", "2026-01-01T00:00:03Z", "2026-01-01T00:00:04Z")
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.lease_fence, first.lease_fence + 1);
        let acknowledgement = ControlPlaneActionAcknowledgement {
            schema: CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA.to_string(),
            acknowledgement: "ack".to_string(),
            run: run.clone(),
            action: ControlPlaneAction::Resume,
            idempotency_key: "key".to_string(),
            actor: "test".to_string(),
            accepted_at: intent.accepted_at.clone(),
            completed_at: "2026-01-01T00:00:04Z".to_string(),
            outcome: ControlPlaneActionOutcome::Succeeded,
            resource: ControlPlaneRun::new(run),
            result: ControlPlaneActionPayload::empty(),
            message: None,
        };
        let terminal = ControlPlaneEffectTerminal {
            schema: CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA.to_string(),
            completed_at: acknowledgement.completed_at.clone(),
            acknowledgement,
            audit: ControlPlaneEffectAudit {
                schema: CONTROL_PLANE_EFFECT_AUDIT_SCHEMA.to_string(),
                observed_at: "2026-01-01T00:00:04Z".to_string(),
                evidence: serde_json::json!({"reconciled": true}),
            },
        };
        assert!(
            store
                .terminalize_control_plane_effect(&intent.effect_id, first.lease_fence, &terminal)
                .is_err(),
            "stale lease must not terminalize"
        );
        let terminalized = store
            .terminalize_control_plane_effect(&intent.effect_id, reclaimed.lease_fence, &terminal)
            .unwrap();
        assert_eq!(terminalized.state, ControlPlaneEffectState::Terminal);
        assert_eq!(terminalized.terminal, Some(terminal));
    }

    #[test]
    fn concurrent_admission_has_one_enqueued_effect_and_one_duplicate() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        store.connection.execute("INSERT INTO runs(id, kind, started_at, status) VALUES ('run-1', 'deploy', 'now', 'running')", []).unwrap();
        drop(store);
        let run = RunId::new("run-1").unwrap();
        let intent = ControlPlaneActionIntent {
            schema: CONTROL_PLANE_ACTION_INTENT_SCHEMA.to_string(),
            effect_id: EffectId("effect-race".to_string()),
            resource: ControlPlaneActionResource {
                resource: ControlPlaneRef::Run(run.clone()),
                run,
                original_alias: None,
            },
            request: ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
                action: ControlPlaneAction::Resume,
                idempotency_key: "race".to_string(),
                actor: "test".to_string(),
                expected_updated_at: None,
                parameters: ControlPlaneActionPayload::empty(),
                confirmed: false,
            },
            request_digest: "c".repeat(64),
            accepted_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let fence = ControlPlaneActionFence {
            schema: CONTROL_PLANE_ACTION_FENCE_SCHEMA.to_string(),
            resource_updated_at: "now".to_string(),
            eligible: true,
            reason: None,
        };
        let barrier = Arc::new(Barrier::new(2));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let intent = intent.clone();
            let fence = fence.clone();
            let barrier = barrier.clone();
            joins.push(std::thread::spawn(move || {
                let store = ObservationStore::open_initialized_at(path).unwrap();
                barrier.wait();
                store
                    .enqueue_control_plane_action_intent(&intent, &fence, &"d".repeat(64))
                    .unwrap()
            }));
        }
        let outcomes = joins
            .into_iter()
            .map(|join| join.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ControlPlaneEffectAdmission::Enqueued(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, ControlPlaneEffectAdmission::Duplicate(_)))
                .count(),
            1
        );
    }
}
