//! Durable, candidate-bound review feedback for an existing Cook.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::fs::OpenOptions;
use std::path::PathBuf;

use crate::agent_task_lifecycle::AgentTaskRunRecord;
use homeboy_core::{paths, Error, Result};

pub const FEEDBACK_SCHEMA: &str = "homeboy/agent-task-cook-feedback/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackState {
    Pending,
    Consumed,
    Stale,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CookFeedback {
    pub idempotency_key: String,
    pub cook_id: String,
    pub candidate_identity: String,
    pub author: String,
    pub source: String,
    pub text: String,
    pub state: FeedbackState,
    pub submitted_at: String,
    #[serde(default)]
    pub consumed_at: Option<String>,
    #[serde(default)]
    pub stale_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct FeedbackDocument {
    schema: String,
    cook_id: String,
    #[serde(default)]
    feedback: Vec<CookFeedback>,
}

#[derive(Debug, Clone)]
pub struct CookFeedbackStore {
    data_root: PathBuf,
}

impl CookFeedbackStore {
    pub fn new(data_root: PathBuf) -> Self {
        Self { data_root }
    }

    pub fn from_current_data_root() -> Result<Self> {
        Ok(Self::new(paths::homeboy_data()?))
    }

    fn path(&self, cook_id: &str) -> PathBuf {
        self.data_root
            .join("agent-task-cooks")
            .join(paths::sanitize_path_segment(cook_id))
            .join("feedback.json")
    }

    fn load(&self, cook_id: &str) -> Result<FeedbackDocument> {
        let path = self.path(cook_id);
        if !path.exists() {
            return Ok(FeedbackDocument {
                schema: FEEDBACK_SCHEMA.to_string(),
                cook_id: cook_id.to_string(),
                feedback: Vec::new(),
            });
        }
        let raw = fs::read_to_string(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        let document: FeedbackDocument = serde_json::from_str(&raw).map_err(|error| {
            Error::validation_invalid_json(error, Some(path.display().to_string()), Some(raw))
        })?;
        if document.schema != FEEDBACK_SCHEMA || document.cook_id != cook_id {
            return Err(Error::validation_invalid_argument(
                "cook_feedback",
                "durable feedback document does not match its Cook",
                Some(cook_id.to_string()),
                None,
            ));
        }
        Ok(document)
    }

    fn persist(&self, document: &FeedbackDocument) -> Result<()> {
        let path = self.path(&document.cook_id);
        fs::create_dir_all(path.parent().expect("feedback path has parent")).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        homeboy_core::engine::local_files::write_json_file_owner_only(&path, document)
    }

    fn with_lock<T>(&self, cook_id: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        use fs4::fs_std::FileExt;
        let path = self.path(cook_id).with_file_name("feedback.lock");
        fs::create_dir_all(path.parent().expect("feedback lock has parent")).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
        lock.lock_exclusive().map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        let result = operation();
        let _ = lock.unlock();
        result
    }

    pub fn submit(
        &self,
        cook_id: &str,
        candidate_identity: &str,
        author: &str,
        source: &str,
        text: &str,
        idempotency_key: &str,
    ) -> Result<(CookFeedback, bool)> {
        for (field, value) in [
            ("cook_id", cook_id),
            ("candidate_identity", candidate_identity),
            ("author", author),
            ("source", source),
            ("text", text),
            ("idempotency_key", idempotency_key),
        ] {
            if value.trim().is_empty() {
                return Err(Error::validation_invalid_argument(
                    format!("cook_feedback.{field}"),
                    "feedback field must not be empty",
                    None,
                    None,
                ));
            }
        }
        self.with_lock(cook_id, || {
            self.submit_locked(
                cook_id,
                candidate_identity,
                author,
                source,
                text,
                idempotency_key,
            )
        })
    }

    fn submit_locked(
        &self,
        cook_id: &str,
        candidate_identity: &str,
        author: &str,
        source: &str,
        text: &str,
        idempotency_key: &str,
    ) -> Result<(CookFeedback, bool)> {
        let mut document = self.load(cook_id)?;
        if let Some(existing) = document
            .feedback
            .iter()
            .find(|item| item.idempotency_key == idempotency_key)
        {
            if existing.candidate_identity != candidate_identity || existing.text != text {
                return Err(Error::validation_invalid_argument(
                    "cook_feedback.idempotency_key",
                    "idempotency key is already bound to different feedback",
                    Some(idempotency_key.to_string()),
                    None,
                ));
            }
            return Ok((existing.clone(), false));
        }
        let feedback = CookFeedback {
            idempotency_key: idempotency_key.to_string(),
            cook_id: cook_id.to_string(),
            candidate_identity: candidate_identity.to_string(),
            author: author.to_string(),
            source: source.to_string(),
            text: text.to_string(),
            state: FeedbackState::Pending,
            submitted_at: Utc::now().to_rfc3339(),
            consumed_at: None,
            stale_reason: None,
        };
        document.feedback.push(feedback.clone());
        self.persist(&document)?;
        Ok((feedback, true))
    }

    pub fn list(&self, cook_id: &str) -> Result<Vec<CookFeedback>> {
        Ok(self.load(cook_id)?.feedback)
    }

    /// Select and acknowledge all pending findings for the exact candidate.
    /// Findings for another candidate become explicit stale records and are
    /// never silently delivered to a later candidate.
    pub fn consume_for_candidate(
        &self,
        cook_id: &str,
        candidate_identity: &str,
    ) -> Result<Vec<CookFeedback>> {
        let pending = self.prepare_for_candidate(cook_id, candidate_identity)?;
        self.acknowledge(cook_id, &pending)
    }

    /// Prepare matching findings without acknowledging provider consumption.
    /// Candidate mismatches are still made explicit as stale under the same
    /// lock, while matching findings remain pending across a restart.
    pub fn prepare_for_candidate(
        &self,
        cook_id: &str,
        candidate_identity: &str,
    ) -> Result<Vec<CookFeedback>> {
        self.with_lock(cook_id, || {
            self.prepare_for_candidate_locked(cook_id, candidate_identity)
        })
    }

    fn prepare_for_candidate_locked(
        &self,
        cook_id: &str,
        candidate_identity: &str,
    ) -> Result<Vec<CookFeedback>> {
        let mut document = self.load(cook_id)?;
        let mut consumed = Vec::new();
        for feedback in &mut document.feedback {
            if feedback.state != FeedbackState::Pending {
                continue;
            }
            if feedback.candidate_identity != candidate_identity {
                feedback.state = FeedbackState::Stale;
                feedback.stale_reason = Some(format!(
                    "candidate identity changed before remediation: expected `{}`, got `{candidate_identity}`",
                    feedback.candidate_identity
                ));
                continue;
            }
            consumed.push(feedback.clone());
        }
        if document
            .feedback
            .iter()
            .any(|feedback| feedback.state == FeedbackState::Stale)
        {
            self.persist(&document)?;
        }
        Ok(consumed)
    }

    /// Acknowledge only the findings whose remediation provider invocation has
    /// been durably observed. The idempotency keys make this restart-safe.
    pub fn acknowledge(
        &self,
        cook_id: &str,
        prepared: &[CookFeedback],
    ) -> Result<Vec<CookFeedback>> {
        if prepared.is_empty() {
            return Ok(Vec::new());
        }
        self.with_lock(cook_id, || {
            let mut document = self.load(cook_id)?;
            let now = Utc::now().to_rfc3339();
            let keys = prepared
                .iter()
                .map(|feedback| feedback.idempotency_key.as_str())
                .collect::<std::collections::HashSet<_>>();
            let mut acknowledged = Vec::new();
            for feedback in &mut document.feedback {
                if keys.contains(feedback.idempotency_key.as_str())
                    && feedback.state == FeedbackState::Pending
                {
                    feedback.state = FeedbackState::Consumed;
                    feedback.consumed_at = Some(now.clone());
                    acknowledged.push(feedback.clone());
                }
            }
            if !acknowledged.is_empty() {
                self.persist(&document)?;
            }
            Ok(acknowledged)
        })
    }

    /// Mark a pending finding superseded by a newer operator submission while
    /// retaining it for status and evidence history.
    pub fn supersede(
        &self,
        cook_id: &str,
        idempotency_key: &str,
        replacement_key: &str,
    ) -> Result<()> {
        self.with_lock(cook_id, || {
            let mut document = self.load(cook_id)?;
            let item = document
                .feedback
                .iter_mut()
                .find(|item| item.idempotency_key == idempotency_key)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "cook_feedback.idempotency_key",
                        "feedback finding was not found",
                        Some(idempotency_key.to_string()),
                        None,
                    )
                })?;
            if item.state == FeedbackState::Pending {
                item.state = FeedbackState::Superseded;
                item.stale_reason = Some(format!("superseded by {replacement_key}"));
                self.persist(&document)?;
            }
            Ok(())
        })
    }

    pub fn status_value(&self, cook_id: &str) -> Result<Value> {
        Ok(serde_json::json!({
            "schema": "homeboy/agent-task-cook-feedback-status/v1",
            "cook_id": cook_id,
            "feedback": self.list(cook_id)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_is_durable_idempotent_candidate_bound_and_restart_safe() {
        let root = tempfile::tempdir().expect("temp root");
        let store = CookFeedbackStore::new(root.path().to_path_buf());
        let (first, created) = store
            .submit(
                "cook-1",
                "candidate-a",
                "reviewer",
                "markdown",
                "fix it",
                "review-1",
            )
            .expect("submit");
        assert!(created);
        let (duplicate, created) = store
            .submit(
                "cook-1",
                "candidate-a",
                "reviewer",
                "markdown",
                "fix it",
                "review-1",
            )
            .expect("duplicate");
        assert!(!created);
        assert_eq!(duplicate, first);
        let stale = store
            .prepare_for_candidate("cook-1", "candidate-b")
            .expect("stale candidate is explicit");
        assert!(stale.is_empty());
        assert_eq!(store.list("cook-1").unwrap()[0].state, FeedbackState::Stale);

        let (second, _) = store
            .submit(
                "cook-1",
                "candidate-b",
                "reviewer",
                "markdown",
                "new fix",
                "review-2",
            )
            .expect("second submit");
        let restarted = CookFeedbackStore::new(root.path().to_path_buf());
        let consumed = restarted
            .prepare_for_candidate("cook-1", "candidate-b")
            .expect("consume after restart");
        assert_eq!(consumed[0].idempotency_key, second.idempotency_key);
        assert_eq!(
            restarted.list("cook-1").unwrap()[1].state,
            FeedbackState::Pending
        );
        let acknowledged = restarted
            .acknowledge("cook-1", &consumed)
            .expect("acknowledge provider invocation");
        assert_eq!(acknowledged[0].text, second.text);
        assert_eq!(acknowledged[0].state, FeedbackState::Consumed);
        assert_eq!(
            restarted.list("cook-1").unwrap()[1].state,
            FeedbackState::Consumed
        );
    }

    #[test]
    fn pending_feedback_can_be_superseded_without_erasing_history() {
        let root = tempfile::tempdir().expect("temp root");
        let store = CookFeedbackStore::new(root.path().to_path_buf());
        store
            .submit(
                "cook-2",
                "candidate",
                "reviewer",
                "review",
                "old",
                "old-key",
            )
            .unwrap();
        store.supersede("cook-2", "old-key", "new-key").unwrap();
        assert_eq!(
            store.list("cook-2").unwrap()[0].state,
            FeedbackState::Superseded
        );
        assert!(store
            .consume_for_candidate("cook-2", "candidate")
            .unwrap()
            .is_empty());
    }
}

/// Resolve the reviewed candidate identity from the durable promotion record.
/// The fallback fields support both committed and artifact-backed candidates.
pub fn candidate_identity(record: &AgentTaskRunRecord) -> Option<String> {
    [
        "/latest_promotion/provenance/candidate_ref",
        "/latest_promotion/provenance/candidate/commit",
        "/latest_promotion/provenance/candidate/fingerprint/sha256",
        "/latest_promotion/provenance/candidate/sha256",
        "/candidate_identity",
    ]
    .iter()
    .find_map(|pointer| record.metadata.pointer(pointer).and_then(Value::as_str))
    .filter(|value| !value.is_empty())
    .map(str::to_string)
}

pub fn append_to_plan(
    record: &AgentTaskRunRecord,
    plan: &mut crate::agent_task_scheduler::AgentTaskPlan,
    feedback: &[CookFeedback],
) -> Result<()> {
    if feedback.is_empty() {
        return Ok(());
    }
    let candidate = candidate_identity(record).ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_feedback.candidate_identity",
            "cannot consume feedback because the durable Cook has no candidate identity",
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let mut section = format!(
        "\n\nReviewer feedback for candidate `{candidate}` (address these findings; do not change the declared verification gates):"
    );
    for item in feedback {
        section.push_str(&format!(
            "\n\n[{} by {}]\n{}",
            item.source, item.author, item.text
        ));
    }
    for task in &mut plan.tasks {
        task.instructions.push_str(&section);
    }
    Ok(())
}
