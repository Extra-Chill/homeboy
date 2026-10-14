//! Terminal consumer custody is independent of job-row/event retention.
//! Exited checkpoint custody is provisional until a terminal winner commits;
//! completion custody can supersede it (e.g. cancellation wins after exit).
//! ACK keeps a small receipt tombstone and releases the private payload.
use super::*;
use crate::daemon::step_job::{self, StepTerminalDelivery};
use rusqlite::{params, Connection, OptionalExtension};

fn sql_error(error: rusqlite::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some("step terminal delivery".to_string()),
    )
}

pub(crate) fn retain_on(
    connection: &Connection,
    delivery: &StepTerminalDelivery,
    completion: bool,
) -> Result<()> {
    let payload = serde_json::to_string(delivery)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    connection
        .execute(
            if completion {
                "INSERT INTO step_terminal_deliveries(job_id, payload) VALUES (?1, ?2) ON CONFLICT(job_id) DO UPDATE SET payload = excluded.payload WHERE receipt IS NULL"
            } else {
                "INSERT OR IGNORE INTO step_terminal_deliveries(job_id, payload) VALUES (?1, ?2)"
            },
            params![delivery.job_id, payload],
        )
        .map_err(sql_error)?;
    Ok(())
}

impl JobStore {
    /// Read pending private evidence, including deliveries whose job was pruned.
    pub fn pending_step_terminal_deliveries(&self) -> Result<Vec<StepTerminalDelivery>> {
        self.step_delivery_rows(false)
            .map(|rows| rows.into_iter().map(|(_, delivery)| delivery).collect())
    }

    fn step_delivery_rows(&self, due_only: bool) -> Result<Vec<(u32, StepTerminalDelivery)>> {
        let Some(path) = self.persistence_path() else {
            return Ok(Vec::new());
        };
        let connection = super::super::persistence::open_tombstone_store(&path)?;
        let mut statement = connection.prepare(
            "SELECT attempts, payload FROM step_terminal_deliveries AS delivery WHERE receipt IS NULL AND (?1 = 0 OR (next_attempt_ms <= ?2 AND NOT EXISTS (SELECT 1 FROM daemon_jobs AS job WHERE job.job_id = delivery.job_id AND job.active = 1))) ORDER BY next_attempt_ms, rowid LIMIT 32"
        ).map_err(sql_error)?;
        let rows = statement
            .query_map(params![due_only, timestamp_ms() as i64], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sql_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        rows.into_iter()
            .map(|(attempts, raw)| {
                serde_json::from_str(&raw)
                    .map(|delivery| (attempts, delivery))
                    .map_err(|error| Error::internal_json(error.to_string(), None))
            })
            .collect()
    }

    /// Receipt ACK is idempotent; an incompatible receipt fails closed.
    pub fn acknowledge_step_terminal_delivery(&self, job_id: &str, receipt: &Value) -> Result<()> {
        let path = self.persistence_path().ok_or_else(|| {
            Error::internal_unexpected("terminal delivery requires a durable store")
        })?;
        let connection = super::super::persistence::open_tombstone_store(&path)?;
        let receipt = serde_json::to_string(receipt)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        let changed = connection.execute("UPDATE step_terminal_deliveries SET receipt = ?2, payload = '', last_error = NULL WHERE job_id = ?1 AND receipt IS NULL", params![job_id, receipt]).map_err(sql_error)?;
        if changed == 0 {
            let existing: Option<String> = connection
                .query_row(
                    "SELECT receipt FROM step_terminal_deliveries WHERE job_id = ?1",
                    [job_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)?
                .flatten();
            if existing.as_deref() != Some(&receipt) {
                return Err(Error::internal_unexpected(
                    "terminal delivery absent or conflicting ACK",
                ));
            }
        }
        Ok(())
    }

    /// One serialized bounded daemon tick. Consumer failures retain evidence
    /// with exponential backoff (1s to 60s); restart never resets the deadline.
    pub fn deliver_pending_step_terminals(&self) -> Result<()> {
        static DRAIN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _drain = DRAIN.lock().expect("terminal delivery drain");
        let Some(path) = self.persistence_path() else {
            return Ok(());
        };
        let mut first_error = None;
        for (attempts, delivery) in self.step_delivery_rows(true)? {
            match step_job::deliver_terminal(&delivery) {
                Ok(receipt) => {
                    self.acknowledge_step_terminal_delivery(&delivery.job_id, &receipt)?
                }
                Err(error) => {
                    let delay = (1000_u64 << attempts.min(6)).min(60_000);
                    super::super::persistence::open_tombstone_store(&path)?.execute(
                        "UPDATE step_terminal_deliveries SET attempts = attempts + 1, next_attempt_ms = ?2, last_error = ?3 WHERE job_id = ?1 AND receipt IS NULL",
                        params![delivery.job_id, (timestamp_ms() + delay) as i64, crate::redaction::redact_string(&error.message)],
                    ).map_err(sql_error)?;
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn reconciled_terminal_evidence_commits_before_same_transaction_compaction() {
        with_isolated_home(|home| {
            step_job::register_step_job_driver();
            let path = home.path().join("compaction.json");
            let store = JobStore::open_without_reconciliation_with_retention(&path, 2, 1).unwrap();
            let mut ids = Vec::new();
            for n in 0..2 {
                let request = step_job::StepJobRequest::new(
                    format!("compaction:{n}"),
                    "/bin/sh",
                    vec![],
                    "/tmp",
                    Default::default(),
                    step_job::STEP_RECEIPT_CONSUMER,
                );
                let envelope = serde_json::from_value(request.submission().unwrap()).unwrap();
                let state = crate::daemon::controller_job_state(&envelope).unwrap();
                let ControllerJobSubmissionOutcome::Submitted(id) = store
                    .admit_controller_job("controller.step".to_string(), request.key, state)
                    .unwrap()
                else {
                    panic!("new submission");
                };
                ids.push(id);
            }
            // Recovery can settle rows without a worker/completion callback.
            // Compaction must not lose either newly terminal payload.
            store
                .durable_transaction(|inner| {
                    for id in &ids {
                        let stored = inner.jobs.get_mut(id).unwrap();
                        stored.job.status = JobStatus::Succeeded;
                        stored.job.finished_at_ms = Some(timestamp_ms());
                    }
                    Ok(())
                })
                .unwrap();
            assert_eq!(store.list().len(), 1);
            let fresh = JobStore::open_without_reconciliation(&path).unwrap();
            assert_eq!(fresh.pending_step_terminal_deliveries().unwrap().len(), 2);
            assert!(fresh
                .pending_step_terminal_deliveries()
                .unwrap()
                .iter()
                .all(|delivery| delivery.outcome == step_job::StepTerminalOutcome::Succeeded));
            fresh.deliver_pending_step_terminals().unwrap();
            assert!(fresh.pending_step_terminal_deliveries().unwrap().is_empty());
        });
    }
}
