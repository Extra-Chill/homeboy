use homeboy_control_plane_contract::{
    ControlPlaneEvent, ControlPlaneEventAppendRequest, ControlPlaneEventRetention, EventId, RunId,
    CONTROL_PLANE_EVENT_RETENTION_SCHEMA, CONTROL_PLANE_EVENT_SCHEMA,
};
use rusqlite::{params, OptionalExtension};

use super::*;

pub const CONTROL_PLANE_EVENT_RETENTION_LIMIT: i64 = 100;

impl ObservationStore {
    pub fn control_plane_event_receipt_exists(
        &self,
        run: &RunId,
        idempotency_digest: &str,
    ) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM control_plane_event_appends WHERE run_id = ?1 AND idempotency_digest = ?2)",
                params![run.as_str(), idempotency_digest],
                |row| row.get(0),
            )
            .map_err(|error| self.read_error("read control-plane event receipt", error))
    }

    pub fn control_plane_event_receipt_digests(&self, run: &RunId) -> Result<Vec<String>> {
        let has_ledger: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'control_plane_event_appends')", [], |row| row.get(0)).map_err(|error| self.read_error("inspect control-plane event ledger", error))?;
        if !has_ledger {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare("SELECT idempotency_digest FROM control_plane_event_appends WHERE run_id = ?1 ORDER BY sequence").map_err(|error| self.read_error("list control-plane event receipts", error))?;
        let rows = statement
            .query_map([run.as_str()], |row| row.get::<_, String>(0))
            .map_err(|error| self.read_error("list control-plane event receipts", error))?;
        rows.map(|row| {
            row.map_err(|error| self.read_error("read control-plane event receipt", error))
        })
        .collect()
    }

    pub fn append_control_plane_event(
        &self,
        run: &RunId,
        request: &ControlPlaneEventAppendRequest,
        idempotency_digest: &str,
        request_digest: &str,
    ) -> Result<ControlPlaneEvent> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(sqlite_error("begin control-plane event append"))?;
        let result = (|| {
            let exists: bool = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runs WHERE id = ?1)",
                    [run.as_str()],
                    |row| row.get(0),
                )
                .map_err(sqlite_error("verify control-plane event run"))?;
            if !exists {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "control-plane run not found",
                    Some(run.as_str().to_string()),
                    None,
                ));
            }
            let existing: Option<(String, String, u64, Option<String>)> = self.connection.query_row("SELECT request_digest, event_id, sequence, event_json FROM control_plane_event_appends WHERE run_id = ?1 AND idempotency_digest = ?2", params![run.as_str(), idempotency_digest], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional().map_err(sqlite_error("read control-plane event receipt"))?;
            if let Some((stored_digest, event_id, sequence, event_json)) = existing {
                if stored_digest != request_digest {
                    return Err(Error::validation_invalid_argument(
                        "idempotency_key",
                        "control-plane event idempotency key was already used for different input",
                        None,
                        None,
                    ));
                }
                let event = match event_json {
                    Some(json) => serde_json::from_str(&json)
                        .map_err(|error| Error::internal_unexpected(error.to_string()))?,
                    None => ControlPlaneEvent {
                        schema: CONTROL_PLANE_EVENT_SCHEMA.to_string(),
                        event: EventId::new(event_id.clone())
                            .map_err(|error| Error::internal_unexpected(error.to_string()))?,
                        sequence,
                        occurred_at: request.occurred_at.clone(),
                        mission: None,
                        run: run.clone(),
                        task: request.task.clone(),
                        attempt: request.attempt.clone(),
                        execution: request.execution.clone(),
                        kind: request.kind.clone(),
                        source: request.source.clone(),
                        data: request.data.clone(),
                        artifacts: request.artifacts.clone(),
                        evidence: request.evidence.clone(),
                    },
                };
                validate_stored_event(run, &event_id, sequence, &event)?;
                return Ok(event);
            }
            let sequence: u64 = self.connection.query_row("SELECT COALESCE(MAX(sequence), 0) + 1 FROM control_plane_event_appends WHERE run_id = ?1", [run.as_str()], |row| row.get(0)).map_err(sqlite_error("allocate control-plane event sequence"))?;
            let event = ControlPlaneEvent {
                schema: CONTROL_PLANE_EVENT_SCHEMA.to_string(),
                event: EventId::new(format!("{}:event:{sequence}", run.as_str())).map_err(
                    |error| {
                        Error::validation_invalid_argument("event", error.to_string(), None, None)
                    },
                )?,
                sequence,
                occurred_at: request.occurred_at.clone(),
                mission: None,
                run: run.clone(),
                task: request.task.clone(),
                attempt: request.attempt.clone(),
                execution: request.execution.clone(),
                kind: request.kind.clone(),
                source: request.source.clone(),
                data: request.data.clone(),
                artifacts: request.artifacts.clone(),
                evidence: request.evidence.clone(),
            };
            let json = serde_json::to_string(&event)
                .map_err(|error| Error::internal_unexpected(error.to_string()))?;
            self.connection.execute("INSERT INTO control_plane_event_appends(run_id, idempotency_digest, request_digest, event_id, sequence, event_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![run.as_str(), idempotency_digest, request_digest, event.event.as_str(), sequence, json, chrono::Utc::now().to_rfc3339()]).map_err(sqlite_error("persist control-plane event"))?;
            self.connection.execute("UPDATE control_plane_event_appends SET event_json = NULL WHERE run_id = ?1 AND event_json IS NOT NULL AND sequence <= (SELECT MAX(sequence) - ?2 FROM control_plane_event_appends WHERE run_id = ?1)", params![run.as_str(), CONTROL_PLANE_EVENT_RETENTION_LIMIT]).map_err(sqlite_error("prune control-plane event payloads"))?;
            Ok(event)
        })();
        match result {
            Ok(event) => {
                self.connection
                    .execute_batch("COMMIT")
                    .map_err(sqlite_error("commit control-plane event append"))?;
                Ok(event)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn control_plane_event_stream(
        &self,
        run: &RunId,
    ) -> Result<Option<Vec<ControlPlaneEvent>>> {
        if self.get_run(run.as_str())?.is_none() {
            return Ok(None);
        }
        let has_ledger: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'control_plane_event_appends')", [], |row| row.get(0)).map_err(|error| self.read_error("inspect control-plane event ledger", error))?;
        if !has_ledger {
            return Ok(Some(Vec::new()));
        }
        let mut statement = self.connection.prepare("SELECT event_id, sequence, event_json FROM control_plane_event_appends WHERE run_id = ?1 AND event_json IS NOT NULL ORDER BY sequence").map_err(|error| self.read_error("list control-plane events", error))?;
        let rows = statement
            .query_map([run.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| self.read_error("list control-plane events", error))?;
        let events = rows
            .map(|row| {
                row.map_err(|error| self.read_error("read control-plane event", error))
                    .and_then(|(event_id, sequence, json)| {
                        let event = serde_json::from_str::<ControlPlaneEvent>(&json)
                            .map_err(|error| Error::internal_unexpected(error.to_string()))?;
                        validate_stored_event(run, &event_id, sequence, &event)?;
                        Ok(event)
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(events))
    }

    pub fn control_plane_event_retention(
        &self,
        run: &RunId,
    ) -> Result<Option<ControlPlaneEventRetention>> {
        if self.get_run(run.as_str())?.is_none() {
            return Ok(None);
        }
        let has_ledger: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'control_plane_event_appends')", [], |row| row.get(0)).map_err(|error| self.read_error("inspect control-plane event ledger", error))?;
        if !has_ledger {
            return Ok(Some(ControlPlaneEventRetention {
                schema: CONTROL_PLANE_EVENT_RETENTION_SCHEMA.to_string(),
                run: run.clone(),
                earliest_sequence: None,
                latest_sequence: None,
            }));
        }
        let (earliest_sequence, latest_sequence) = self.connection.query_row("SELECT MIN(sequence), MAX(sequence) FROM control_plane_event_appends WHERE run_id = ?1 AND event_json IS NOT NULL", [run.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|error| self.read_error("read control-plane event retention", error))?;
        Ok(Some(ControlPlaneEventRetention {
            schema: CONTROL_PLANE_EVENT_RETENTION_SCHEMA.to_string(),
            run: run.clone(),
            earliest_sequence,
            latest_sequence,
        }))
    }
}

fn validate_stored_event(
    run: &RunId,
    event_id: &str,
    sequence: u64,
    event: &ControlPlaneEvent,
) -> Result<()> {
    if event_id != format!("{}:event:{sequence}", run.as_str())
        || event.event.as_str() != event_id
        || event.sequence != sequence
        || event.run != *run
    {
        return Err(Error::internal_unexpected(
            "stored control-plane event identity is inconsistent".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::{
        ControlPlaneEventAppendRequest, ControlPlaneEventSource,
        CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
    };

    fn fixture() -> (tempfile::TempDir, ObservationStore, RunId) {
        let directory = tempfile::tempdir().unwrap();
        let store =
            ObservationStore::open_initialized_at(directory.path().join("observations.sqlite"))
                .unwrap();
        store.connection.execute("INSERT INTO runs(id, kind, started_at, status) VALUES ('run-1', 'test', 'now', 'running')", []).unwrap();
        (directory, store, RunId::new("run-1").unwrap())
    }

    fn request(key: &str, kind: &str) -> ControlPlaneEventAppendRequest {
        ControlPlaneEventAppendRequest {
            schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.to_string(),
            idempotency_key: key.to_string(),
            actor: "broker:fixture".to_string(),
            kind: kind.to_string(),
            source: ControlPlaneEventSource {
                component: "fixture".to_string(),
                instance: None,
            },
            occurred_at: None,
            task: None,
            attempt: None,
            execution: None,
            data: serde_json::json!({"message":"ok"}),
            artifacts: vec![],
            evidence: vec![],
        }
    }

    #[test]
    fn append_replays_conflicts_orders_and_prunes_payloads() {
        let (_directory, store, run) = fixture();
        let first = request("key-0", "state");
        let event = store
            .append_control_plane_event(&run, &first, &"a".repeat(64), &"b".repeat(64))
            .unwrap();
        assert_eq!(event.sequence, 1);
        assert_eq!(
            store
                .append_control_plane_event(&run, &first, &"a".repeat(64), &"b".repeat(64))
                .unwrap(),
            event
        );
        assert!(store
            .append_control_plane_event(
                &run,
                &request("key-0", "other"),
                &"a".repeat(64),
                &"c".repeat(64)
            )
            .is_err());
        for sequence in 2..=102 {
            let request = request(&format!("key-{sequence}"), "state");
            store
                .append_control_plane_event(
                    &run,
                    &request,
                    &format!("{sequence:064x}"),
                    &format!("{:064x}", sequence + 1000),
                )
                .unwrap();
        }
        let events = store.control_plane_event_stream(&run).unwrap().unwrap();
        assert_eq!(events.len(), 100);
        assert_eq!(events[0].sequence, 3);
        assert_eq!(events.last().unwrap().sequence, 102);
        let receipts: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM control_plane_event_appends WHERE run_id = 'run-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payloads: i64 = store.connection.query_row("SELECT COUNT(*) FROM control_plane_event_appends WHERE run_id = 'run-1' AND event_json IS NOT NULL", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 102);
        assert_eq!(payloads, 100);
        assert_eq!(
            store
                .append_control_plane_event(&run, &first, &"a".repeat(64), &"b".repeat(64))
                .unwrap(),
            event,
            "an evicted payload still replays from its compact receipt and canonical request"
        );
        store
            .connection
            .execute(
                "UPDATE control_plane_event_appends SET event_id = 'corrupt' WHERE run_id = 'run-1' AND sequence = 1",
                [],
            )
            .unwrap();
        assert!(store
            .append_control_plane_event(&run, &first, &"a".repeat(64), &"b".repeat(64))
            .is_err());
    }
}
