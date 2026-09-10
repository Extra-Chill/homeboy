use homeboy_control_plane_contract::{ControlPlaneActionAcknowledgement, RunId};
use rusqlite::{params, OptionalExtension};

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPlaneActionClaim {
    Acquired { accepted_at: String },
    Recover { accepted_at: String },
    InProgress,
    Completed(ControlPlaneActionAcknowledgement),
}

impl ObservationStore {
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
        ControlPlaneAction, ControlPlaneActionOutcome, ControlPlaneActionPayload, ControlPlaneRun,
        CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
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
}
