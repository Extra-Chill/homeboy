//! Controller-side record of the most recent failed job observation (#17648).
//!
//! A runner can answer one `runner status` probe while the jobs it owns keep
//! losing contact with this controller. A momentary probe alone would then
//! report `connected | accepting`. Job paths record each failed observation
//! here, and status projects any recent failure so connection health includes
//! the last job-observation outcome instead of only the current probe.
//!
//! This file also owns the typed `observation_unavailable` classification: an
//! exhausted observation deadline or a timed-out daemon request is a retryable
//! observation result and never evidence about daemon ownership.

use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::paths;

/// Typed code for an observation that did not complete.
pub const OBSERVATION_UNAVAILABLE: &str = "observation_unavailable";
/// Typed code for a controller that lost contact while polling a known job.
pub const CONTROLLER_DISCONNECTED: &str = "controller_disconnected";

/// How long a failed job observation keeps the runner reported as degraded.
const RECENT_FAILURE_WINDOW_MINUTES: i64 = 15;

/// The most recent job-observation failure this controller saw for a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerObservationFailure {
    pub code: String,
    pub message: String,
    pub observed_at: String,
}

/// A retryable error naming an observation that did not complete in time.
pub(crate) fn observation_unavailable_error(message: impl Into<String>) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        message.into(),
        json!({ OBSERVATION_UNAVAILABLE: true }),
    )
    .with_retryable(true)
}

/// Whether `error` is an observation that did not complete, rather than an
/// answer from the daemon.
pub(crate) fn is_observation_unavailable_error(error: &Error) -> bool {
    error.details[OBSERVATION_UNAVAILABLE] == Value::Bool(true)
        || error
            .details
            .pointer("/daemon_transport_error/kind")
            .and_then(Value::as_str)
            == Some("timeout")
}

fn record_path(runner_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy()?
        .join("runner-observation-health")
        .join(paths::sanitize_path_segment(runner_id))
        .join(format!(
            "{}.json",
            paths::sanitize_path_segment(&crate::connection::controller_id())
        )))
}

/// Record a failed job observation. Best effort: a diagnostic write must never
/// replace the job error the caller is about to report.
pub(crate) fn record_observation_failure(runner_id: &str, code: &str, message: &str) {
    let Ok(path) = record_path(runner_id) else {
        return;
    };
    let failure = RunnerObservationFailure {
        code: code.to_string(),
        message: message.to_string(),
        observed_at: Utc::now().to_rfc3339(),
    };
    let Ok(body) = serde_json::to_vec_pretty(&failure) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let staged = path.with_extension("json.tmp");
    if std::fs::write(&staged, body).is_ok() {
        let _ = std::fs::rename(&staged, &path);
    }
}

/// The recorded job-observation failure, when it is recent enough to still
/// describe the runner's connection health.
pub(crate) fn recent_observation_failure(runner_id: &str) -> Option<RunnerObservationFailure> {
    let body = std::fs::read(record_path(runner_id).ok()?).ok()?;
    let failure: RunnerObservationFailure = serde_json::from_slice(&body).ok()?;
    let observed_at = DateTime::parse_from_rfc3339(&failure.observed_at).ok()?;
    (Utc::now().signed_duration_since(observed_at)
        < Duration::minutes(RECENT_FAILURE_WINDOW_MINUTES))
    .then_some(failure)
}
