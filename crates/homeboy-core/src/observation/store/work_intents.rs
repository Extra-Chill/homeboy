//! Generic work intents committed with an observation run and drained into the
//! daemon's separate durable job store. The intent key is the submission key:
//! replay after a lost ACK must select the same controller job.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{sqlite_error, ObservationStore};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkIntent {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    pub version: u32,
    pub payload: Value,
}

impl WorkIntent {
    pub fn validate(&self, expected_run_id: &str) -> Result<()> {
        if self.id.trim().is_empty()
            || self.kind.trim().is_empty()
            || self.version == 0
            || self.run_id != expected_run_id
        {
            return Err(Error::validation_invalid_argument(
                "work_intent",
                "work intents require a key, type, version, and matching durable run",
                Some(self.id.clone()),
                None,
            ));
        }
        Ok(())
    }
}

pub(super) fn append_work_intent_on(connection: &Connection, intent: &WorkIntent) -> Result<()> {
    intent.validate(&intent.run_id)?;
    let payload = serde_json::to_string(&intent.payload)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let existing: Option<(String, String, u32, String)> = connection
        .query_row(
            "SELECT run_id, kind, version, payload_json FROM control_plane_work_intents WHERE id = ?1",
            [&intent.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error("inspect work intent receipt"))?;
    if let Some(existing) = existing {
        if existing
            != (
                intent.run_id.clone(),
                intent.kind.clone(),
                intent.version,
                payload,
            )
        {
            return Err(Error::validation_invalid_argument(
                "work_intent.id",
                "work intent key was already used for different input",
                Some(intent.id.clone()),
                None,
            ));
        }
        return Ok(());
    }
    connection
        .execute(
            "INSERT INTO control_plane_work_intents(id, run_id, kind, version, payload_json, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![intent.id, intent.run_id, intent.kind, intent.version, payload, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(sqlite_error("persist work intent"))?;
    Ok(())
}

impl ObservationStore {
    /// Immutable exact intent, including intents already acknowledged by the
    /// scheduler. A child must not depend on the pending queue's current head.
    pub fn work_intent_by_id(&self, id: &str) -> Result<Option<WorkIntent>> {
        let row: Option<(String, String, u32, String)> = self.connection.query_row(
            "SELECT run_id, kind, version, payload_json FROM control_plane_work_intents WHERE id = ?1",
            [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional().map_err(|error| self.read_error("read exact work intent", error))?;
        row.map(|(run_id, kind, version, payload)| {
            Ok(WorkIntent {
                id: id.to_string(),
                run_id,
                kind,
                version,
                payload: serde_json::from_str(&payload)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
            })
        })
        .transpose()
    }
    /// Read the next indexed pending intent without claiming a foreign daemon
    /// job. Submission is deduplicated by its stable id and the ACK is durable.
    pub fn next_pending_work_intent(&self) -> Result<Option<WorkIntent>> {
        let row: Option<(String, String, String, u32, String)> = self
            .connection
            .query_row(
                "SELECT id, run_id, kind, version, payload_json FROM control_plane_work_intents WHERE state = 'pending' ORDER BY created_at, id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(|error| self.read_error("read pending work intent", error))?;
        row.map(|(id, run_id, kind, version, payload)| {
            Ok(WorkIntent {
                id,
                run_id,
                kind,
                version,
                payload: serde_json::from_str(&payload)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
            })
        })
        .transpose()
    }

    /// Settle the outbox only after the daemon accepted the idempotent job.
    /// Repeating a receipt is harmless; conflicting receipts fail closed.
    pub fn acknowledge_work_intent(&self, id: &str, receipt: &Value) -> Result<()> {
        let receipt_json = serde_json::to_string(receipt)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        let applied = self.connection.execute(
            "UPDATE control_plane_work_intents SET state = 'submitted', receipt_json = ?2 WHERE id = ?1 AND state = 'pending'",
            params![id, receipt_json],
        ).map_err(sqlite_error("acknowledge work intent"))?;
        if applied > 0 {
            return Ok(());
        }
        let existing: Option<String> = self.connection.query_row(
            "SELECT receipt_json FROM control_plane_work_intents WHERE id = ?1 AND state = 'submitted'",
            [id],
            |row| row.get(0),
        ).optional().map_err(sqlite_error("inspect work intent acknowledgement"))?;
        if existing.as_deref() == Some(receipt_json.as_str()) {
            return Ok(());
        }
        Err(Error::validation_invalid_argument(
            "work_intent.id",
            "work intent is absent or has a conflicting acknowledgement",
            Some(id.to_string()),
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn exact_intent_lookup_retains_acknowledged_input() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().unwrap();
            store.connection.execute("INSERT INTO runs(id, kind, started_at, status) VALUES ('run', 'test', ?1, 'running')", [chrono::Utc::now().to_rfc3339()]).unwrap();
            let intent = WorkIntent {
                id: "exact-intent".to_string(),
                run_id: "run".to_string(),
                kind: "test".to_string(),
                version: 1,
                payload: serde_json::json!({"frozen": true}),
            };
            append_work_intent_on(&store.connection, &intent).unwrap();
            assert_eq!(
                store.work_intent_by_id(&intent.id).unwrap(),
                Some(intent.clone())
            );
            store
                .acknowledge_work_intent(&intent.id, &serde_json::json!({"job_id": "job"}))
                .unwrap();
            assert!(store.next_pending_work_intent().unwrap().is_none());
            assert_eq!(store.work_intent_by_id(&intent.id).unwrap(), Some(intent));
            assert!(store.work_intent_by_id("absent").unwrap().is_none());
        });
    }
}
