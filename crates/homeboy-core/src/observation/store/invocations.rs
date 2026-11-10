//! Invocation rows share the run store and its terminal retention policy.
use super::{sqlite_error, ObservationStore};
use crate::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Invocation {
    pub command_path: String,
    pub exit_code: i32,
    pub error_code: Option<String>,
    pub error_field: Option<String>,
    pub duration_ms: u64,
    pub caller_session: Option<String>,
    pub run_id: Option<String>,
}

impl ObservationStore {
    /// One insert, no contextual reads, no argv or error messages persisted.
    pub fn record_invocation(
        &self,
        invocation: &Invocation,
        version: &str,
        started_at: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.connection.execute(
            "INSERT INTO runs (id, kind, started_at, finished_at, status, command, homeboy_version, metadata_json) VALUES (?1, 'cli-invocation', ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), started_at, now,
                if invocation.exit_code == 0 { "pass" } else { "error" },
                invocation.command_path, version, serde_json::to_string(invocation).unwrap_or_default()],
        ).map_err(sqlite_error("record CLI invocation"))?;
        Ok(())
    }
}
