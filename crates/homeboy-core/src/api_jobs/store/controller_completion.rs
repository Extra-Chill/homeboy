//! Write-ahead terminal custody in the job store's existing SQLite index.
//! A failed whole-snapshot write must never cause a completed driver to resume.

use super::*;
use rusqlite::{params, OptionalExtension};

#[derive(Clone, Serialize, Deserialize)]
struct Completion {
    job_id: Uuid,
    execution_claim_id: Option<String>,
    status: JobStatus,
    event_kind: JobEventKind,
    message: String,
    data: Value,
}

fn decode_completion(raw: &str, job_id: Uuid) -> Result<Completion> {
    let completion: Completion = serde_json::from_str(raw).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("controller completion custody".to_string()),
        )
    })?;
    if completion.job_id != job_id || !completion.status.is_terminal() {
        return Err(Error::internal_unexpected(
            "controller completion custody has mismatched identity or nonterminal status",
        ));
    }
    Ok(completion)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(store: &JobStore) -> Uuid {
        let state = ControllerJobState {
            job_type: "test.completion-custody".to_string(),
            version: 1,
            request: serde_json::json!({}),
            public_request: serde_json::json!({}),
            request_digest: "digest".to_string(),
            active_idempotency_key: None,
            linked_durable_run_id: None,
            checkpoint: Some(serde_json::json!({"side_effect": "prepared"})),
            cancellation_requested: false,
            cancellation_reason: None,
            execution_claim_id: None,
            recovery_attempted: false,
        };
        let ControllerJobSubmissionOutcome::Submitted(id) = store
            .admit_controller_job(
                "controller.test.completion-custody".to_string(),
                Uuid::new_v4().to_string(),
                state,
            )
            .unwrap()
        else {
            panic!("unique job")
        };
        store.claim_controller_execution(id, false).unwrap();
        id
    }

    #[test]
    fn controller_completion_custody_survives_failed_write_and_restart_for_every_outcome() {
        for status in [
            JobStatus::Succeeded,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("jobs.json");
            let store = JobStore::open_without_reconciliation(&path).unwrap();
            let id = running(&store);
            if status == JobStatus::Cancelled {
                store
                    .request_controller_cancellation(id, "confirmed stop".to_string())
                    .unwrap();
            }
            store.fail_next_durable_writes(1);
            let failed = match status {
                JobStatus::Succeeded => store
                    .complete_controller_success(id, serde_json::json!({"original_result": true})),
                JobStatus::Failed => store.fail_controller_error(
                    id,
                    "original failure".to_string(),
                    serde_json::json!({"original_error": true}),
                ),
                JobStatus::Cancelled => store.complete_controller_cancellation(id),
                _ => unreachable!(),
            };
            assert!(failed.is_err());
            assert_eq!(store.get(id).unwrap().status, JobStatus::Running);
            assert!(store.controller_completion_pending(id).unwrap());
            let restarted = JobStore::open_without_reconciliation(&path).unwrap();
            assert!(
                restarted.claim_controller_execution(id, true).is_err(),
                "a pending completed result fences driver resume"
            );
            restarted.reconcile_controller_completions().unwrap();
            assert_eq!(restarted.get(id).unwrap().status, status);
            assert!(!restarted.controller_completion_pending(id).unwrap());
            let before = fs::read(&path).unwrap();
            restarted.reconcile_controller_completions().unwrap();
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn controller_completion_committed_custody_rejects_late_cancellation_without_losing_result() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.json");
        let store = JobStore::open_without_reconciliation(&path).unwrap();
        let id = running(&store);
        store.fail_next_durable_writes(1);
        assert!(store
            .complete_controller_success(id, serde_json::json!({"finished": true}))
            .is_err());
        assert!(store
            .request_controller_cancellation(id, "too late".to_string())
            .is_err());
        assert!(
            !store
                .controller_job_state(id)
                .unwrap()
                .cancellation_requested
        );
        store.reconcile_controller_completions().unwrap();
        assert_eq!(store.get(id).unwrap().status, JobStatus::Succeeded);
        assert!(store.events(id).unwrap().iter().any(|event| event
            .data
            .as_ref()
            .is_some_and(|data| data["finished"] == true)));
    }

    #[test]
    fn controller_completion_transient_failure_retains_original_result_and_clears_custody() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.json");
        let store = JobStore::open_without_reconciliation(&path).unwrap();
        let id = running(&store);
        store.terminal_write_failures.store(1, Ordering::SeqCst);
        assert!(store
            .complete_controller_success(id, serde_json::json!({"first": true}))
            .is_err());
        store
            .complete_controller_success(id, serde_json::json!({"different_retry": true}))
            .unwrap();
        assert!(store.events(id).unwrap().iter().any(|event| event
            .data
            .as_ref()
            .is_some_and(|data| data["first"] == true)));
        assert!(!store.controller_completion_pending(id).unwrap());
        assert_eq!(store.events(id).unwrap().len(), 3);
    }

    #[test]
    fn controller_completion_blocked_batch_does_not_starve_later_healthy_custody() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jobs.json");
        let store = JobStore::open_without_reconciliation(&path).unwrap();
        for _ in 0..32 {
            let id = running(&store);
            store.fail_next_durable_writes(1);
            assert!(store
                .complete_controller_success(id, serde_json::json!({"finished": true}))
                .is_err());
        }
        // Restore an older primary snapshot, as in store-loss recovery. The
        // committed completions must remain protected, not inferred absent.
        fs::write(&path, br#"{"jobs":[]}"#).unwrap();
        let restarted = JobStore::open_without_reconciliation(&path).unwrap();
        let healthy = running(&restarted);
        restarted.fail_next_durable_writes(1);
        assert!(restarted
            .complete_controller_success(healthy, serde_json::json!({"healthy": true}))
            .is_err());
        assert!(restarted.reconcile_controller_completions().is_err());
        assert_eq!(restarted.get(healthy).unwrap().status, JobStatus::Running);
        assert!(restarted.reconcile_controller_completions().is_err());
        assert_eq!(restarted.get(healthy).unwrap().status, JobStatus::Succeeded);
        assert_eq!(
            pending_completion_report(&path).unwrap()["count"],
            32,
            "unresolved custody stays inspectable"
        );
    }
}

fn index_error(error: rusqlite::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some("controller completion custody".to_string()),
    )
}

pub(super) fn pending_completion_report(path: &std::path::Path) -> Result<Value> {
    if !super::super::persistence::tombstone_path(path).exists() {
        return Ok(serde_json::json!({"count": 0, "bytes": 0, "resident": false}));
    }
    let connection = super::super::persistence::open_tombstone_store(path)?;
    let (count, bytes): (u64, u64) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(payload)), 0) FROM controller_completions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(index_error)?;
    Ok(serde_json::json!({"count": count, "bytes": bytes, "resident": false}))
}

impl JobStore {
    fn pending_controller_completion(&self, job_id: Uuid) -> Result<Option<Completion>> {
        let Some(persistence) = &self.persistence else {
            return Ok(None);
        };
        if !super::super::persistence::tombstone_path(&persistence.path).exists() {
            return Ok(None);
        }
        let connection = super::super::persistence::open_tombstone_store(&persistence.path)?;
        let raw: Option<String> = connection
            .query_row(
                "SELECT payload FROM controller_completions WHERE job_id = ?1",
                [job_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(index_error)?;
        raw.map(|raw| decode_completion(&raw, job_id)).transpose()
    }

    pub(crate) fn controller_completion_pending(&self, job_id: Uuid) -> Result<bool> {
        Ok(self.pending_controller_completion(job_id)?.is_some())
    }

    /// Called under the same cross-process lock as the primary-store transition.
    /// The first completed result retains custody; durable cancellation can
    /// supersede a pending success before it becomes the terminal winner.
    fn retain_controller_completion(
        &self,
        completion: Completion,
        cancellation_requested: bool,
    ) -> Result<Completion> {
        let Some(persistence) = &self.persistence else {
            return Ok(completion);
        };
        if let Some(existing) = self.pending_controller_completion(completion.job_id)? {
            if existing.execution_claim_id != completion.execution_claim_id {
                return Err(Error::validation_invalid_argument(
                    "execution_claim_id",
                    "controller completion belongs to another execution claim",
                    Some(completion.job_id.to_string()),
                    None,
                ));
            }
            if !(cancellation_requested && completion.status != JobStatus::Succeeded) {
                return Ok(existing);
            }
        }
        let raw = serde_json::to_string(&completion).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize controller completion custody".to_string()),
            )
        })?;
        let connection = super::super::persistence::open_tombstone_store(&persistence.path)?;
        connection
            .execute(
                "INSERT INTO controller_completions (job_id, payload) VALUES (?1, ?2)
             ON CONFLICT(job_id) DO UPDATE SET payload = excluded.payload",
                params![completion.job_id.to_string(), raw],
            )
            .map_err(index_error)?;
        Ok(completion)
    }

    fn clear_controller_completion(&self, job_id: Uuid) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        if !super::super::persistence::tombstone_path(&persistence.path).exists() {
            return Ok(());
        }
        super::super::persistence::open_tombstone_store(&persistence.path)?
            .execute(
                "DELETE FROM controller_completions WHERE job_id = ?1",
                [job_id.to_string()],
            )
            .map_err(index_error)?;
        Ok(())
    }

    /// Return only an immutable, durably committed controller terminal winner.
    fn controller_terminal_winner(inner: &JobStoreInner, job_id: Uuid) -> Result<Option<Job>> {
        let stored = inner
            .jobs
            .get(&job_id)
            .ok_or_else(|| job_not_found(job_id))?;
        if stored.controller_job.is_none() {
            return Err(Error::validation_invalid_argument(
                "job_id",
                "job is not a controller job",
                Some(job_id.to_string()),
                None,
            ));
        }
        Ok(stored.job.status.is_terminal().then(|| stored.job.clone()))
    }

    pub(super) fn terminalize_controller_job(
        &self,
        job_id: Uuid,
        status: JobStatus,
        event_kind: JobEventKind,
        message: String,
        data: Value,
    ) -> Result<Job> {
        // A cached terminal row was committed before publication and cannot
        // change again. Repeated losing writers do no reload, clone or rewrite.
        if let Some(winner) = Self::controller_terminal_winner(
            &self.inner.lock().expect("job store mutex poisoned"),
            job_id,
        )? {
            let _ = self.clear_controller_completion(job_id);
            return Ok(winner);
        }
        let transaction = self.begin_durable_transaction()?;
        let mut inner = self.inner.lock().expect("job store mutex poisoned");
        if let Some(persistence) = &self.persistence {
            self.reload_durable_snapshot_already_locked(&persistence.path, &mut inner)?;
        }
        if let Some(winner) = Self::controller_terminal_winner(&inner, job_id)? {
            let _ = self.clear_controller_completion(job_id);
            return Ok(winner);
        }
        let stored = inner.jobs.get(&job_id).expect("validated job");
        let controller = stored
            .controller_job
            .as_ref()
            .expect("validated controller");
        if status == JobStatus::Succeeded && controller.cancellation_requested {
            return Err(Error::validation_invalid_argument(
                "status",
                "cannot complete a controller job after durable cancellation was requested",
                Some(job_id.to_string()),
                None,
            ));
        }
        validate_transition(stored.job.status, status)?;
        let completion = self.retain_controller_completion(
            Completion {
                job_id,
                execution_claim_id: controller.execution_claim_id.clone(),
                status,
                event_kind,
                message,
                data,
            },
            controller.cancellation_requested,
        )?;
        // Copy only once a real, legal mutation has durable completion custody.
        let prior = inner.clone();
        #[cfg(test)]
        if self
            .terminal_write_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(Error::internal_io(
                "injected controller terminal persistence failure",
                None,
            ));
        }
        let now = timestamp_ms();
        let sequence = self.next_event_sequence.fetch_add(2, Ordering::SeqCst) + 1;
        let stored = inner.jobs.get_mut(&job_id).expect("validated job");
        stored.events.push(JobEvent {
            sequence,
            job_id,
            kind: completion.event_kind,
            timestamp_ms: now,
            message: Some(completion.message.clone()),
            data: Some(completion.data),
        });
        stored.events.push(JobEvent {
            sequence: sequence + 1,
            job_id,
            kind: JobEventKind::Status,
            timestamp_ms: now,
            message: Some(completion.message),
            data: Some(serde_json::json!({"status": completion.status})),
        });
        apply_event_retention(&mut stored.events, self.event_retention_limit());
        stored.job.event_count = stored.events.len();
        stored.job.status = completion.status;
        stored.job.updated_at_ms = now;
        stored.job.finished_at_ms = Some(now);
        stored
            .controller_job
            .as_mut()
            .expect("validated controller")
            .execution_claim_id = None;
        let job = stored.job.clone();
        for submission in inner.controller_submissions.values_mut() {
            if submission.job_id == job_id {
                submission.terminal_job = Some(job.clone());
            }
        }
        if transaction.is_some() {
            if let Err(error) = self.persist_inner_already_locked(&mut inner) {
                *inner = prior;
                return Err(error);
            }
        }
        let _ = self.clear_controller_completion(job_id);
        Ok(job)
    }

    /// One bounded pass; a blocked entry cannot strand independent completions.
    pub(crate) fn reconcile_controller_completions(&self) -> Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        if !super::super::persistence::tombstone_path(&persistence.path).exists() {
            return Ok(());
        }
        let connection = super::super::persistence::open_tombstone_store(&persistence.path)?;
        let mut statement = connection
            .prepare("SELECT job_id, payload FROM controller_completions ORDER BY rowid LIMIT 32")
            .map_err(index_error)?;
        let completions = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(index_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(index_error)?;
        drop(statement);
        drop(connection);
        let mut first_error = None;
        for (key, raw) in completions {
            let result = (|| {
                let id = Uuid::parse_str(&key).map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("controller completion custody".to_string()),
                    )
                })?;
                let completion = decode_completion(&raw, id)?;
                let current = self.controller_job_state(completion.job_id)?;
                if !self.get(completion.job_id)?.status.is_terminal()
                    && current.execution_claim_id != completion.execution_claim_id
                {
                    return Err(Error::validation_invalid_argument(
                        "execution_claim_id",
                        "pending completion does not own the current controller execution",
                        Some(completion.job_id.to_string()),
                        None,
                    ));
                }
                self.terminalize_controller_job(
                    completion.job_id,
                    completion.status,
                    completion.event_kind,
                    completion.message,
                    completion.data,
                )
                .map(|_| ())
            })();
            if let Err(error) = result {
                // Move blocked entries to the next batch without changing their
                // immutable identity or evidence. Thirty-two blocked receipts
                // must not starve later healthy completions forever.
                let _ = super::super::persistence::open_tombstone_store(&persistence.path)
                    .and_then(|connection| connection.execute(
                        "UPDATE controller_completions SET rowid = (SELECT COALESCE(MAX(rowid), 0) + 1 FROM controller_completions) WHERE job_id = ?1",
                        [&key],
                    ).map_err(index_error));
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
