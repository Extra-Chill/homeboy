//! Running a schedule and deciding whether the result is worth reporting.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::state::{load_state, save_state, ScheduleState};
use super::types::{NotifyPolicy, Schedule};

/// What a single schedule run produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRunOutcome {
    pub schedule_id: String,
    pub command: Vec<String>,
    pub status: String,
    pub exit_code: i32,
    pub started_at: String,
    pub finished_at: String,
    /// Whether this run's reportable payload differs from the previous run's.
    pub changed: bool,
    pub notified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Executes a homeboy command and reports its structured result.
///
/// A trait so the tick can be tested without spawning processes, and so the
/// CLI layer can later supply an in-process runner without core depending
/// upward on the CLI crate.
pub trait ScheduleCommandRunner: Send + Sync {
    /// Run `argv` (without the leading binary name) and return the parsed
    /// `homeboy/command-result/v3` envelope.
    fn run(&self, argv: &[String]) -> Result<serde_json::Value>;
}

/// Runs schedules by re-executing the homeboy binary.
///
/// Core cannot call into the CLI crate, and a subprocess additionally means a
/// panicking or hanging command cannot take the daemon down with it.
pub struct SubprocessRunner {
    binary: std::path::PathBuf,
}

impl SubprocessRunner {
    pub fn new() -> Result<Self> {
        let binary = std::env::current_exe().map_err(|error| {
            Error::internal_io(
                format!("Failed to resolve the homeboy binary for scheduled runs: {error}"),
                None,
            )
        })?;
        Ok(Self { binary })
    }
}

impl ScheduleCommandRunner for SubprocessRunner {
    fn run(&self, argv: &[String]) -> Result<serde_json::Value> {
        let output_file = tempfile::Builder::new()
            .prefix("homeboy-schedule-")
            .suffix(".json")
            .tempfile()
            .map_err(|error| {
                Error::internal_io(
                    format!("Failed to create a schedule output file: {error}"),
                    None,
                )
            })?;
        let output_path = output_file.path().to_path_buf();

        let status = std::process::Command::new(&self.binary)
            .arg("--output")
            .arg(&output_path)
            .args(argv)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|error| {
                Error::internal_io(
                    format!("Failed to run scheduled command: {error}"),
                    Some(self.binary.display().to_string()),
                )
            })?;

        let raw = std::fs::read_to_string(&output_path).unwrap_or_default();
        if raw.trim().is_empty() {
            // The command produced no envelope — surface the exit code rather
            // than inventing a result.
            return Ok(serde_json::json!({
                "schema": "homeboy/command-result/v3",
                "success": status.success(),
                "exit_code": status.code().unwrap_or(-1),
                "status": if status.success() { "succeeded" } else { "failed" },
            }));
        }
        serde_json::from_str(&raw).map_err(|error| {
            Error::internal_io(
                format!("Scheduled command wrote output that is not valid JSON: {error}"),
                Some(output_path.display().to_string()),
            )
        })
    }
}

/// Fingerprint the part of a result that indicates *what the world looks
/// like*, so an unchanged healthy run can stay quiet.
///
/// Volatile fields — timestamps, durations, run ids — are excluded, otherwise
/// every run would look like a change and `NotifyPolicy::Change` would degrade
/// into `Always`.
pub fn result_digest(value: &serde_json::Value) -> String {
    let mut normalized = value.clone();
    strip_volatile(&mut normalized);
    let rendered = serde_json::to_string(&normalized).unwrap_or_default();
    let digest = <sha2::Sha256 as sha2::Digest>::digest(rendered.as_bytes());
    format!("{digest:x}")
}

const VOLATILE_KEYS: &[&str] = &[
    "duration_ms",
    "elapsed_ms",
    "finished_at",
    "generated_at",
    "run_id",
    "started_at",
    "timestamp",
    "took_ms",
];

fn strip_volatile(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|key, _| !VOLATILE_KEYS.contains(&key.as_str()));
            for entry in map.values_mut() {
                strip_volatile(entry);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                strip_volatile(item);
            }
        }
        _ => {}
    }
}

/// Whether a completed run should notify, given the policy and whether the
/// result changed.
pub fn should_notify(policy: NotifyPolicy, succeeded: bool, changed: bool) -> bool {
    match policy {
        NotifyPolicy::Always => true,
        NotifyPolicy::Failure => !succeeded,
        // A failure is always a change worth reporting, even if it fails the
        // same way twice: staying silent on a repeated failure would hide an
        // ongoing outage.
        NotifyPolicy::Change => changed || !succeeded,
    }
}

/// Run one schedule now, record the outcome, and notify if the policy says so.
pub fn run_schedule(schedule: &Schedule, runner: &dyn ScheduleCommandRunner) -> ScheduleRunOutcome {
    let started = chrono::Utc::now();
    let previous = load_state(&schedule.id);

    // Mark in-flight before executing so an overlapping tick declines.
    let _ = save_state(
        &schedule.id,
        &ScheduleState {
            running: true,
            started_at: Some(started.to_rfc3339()),
            ..previous.clone()
        },
    );

    let result = runner.run(&schedule.command);
    let finished = chrono::Utc::now();

    let (status, exit_code, digest, summary) = match &result {
        Ok(value) => {
            let exit_code = value
                .get("exit_code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32;
            let status = value
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(if exit_code == 0 {
                    "succeeded"
                } else {
                    "failed"
                })
                .to_string();
            let summary = value
                .get("summary")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            (status, exit_code, result_digest(value), summary)
        }
        Err(error) => (
            "failed".to_string(),
            -1,
            String::new(),
            Some(error.to_string()),
        ),
    };

    let succeeded = status == "succeeded" && exit_code == 0;
    let changed = previous
        .last_digest
        .as_deref()
        .map(|last| last != digest)
        .unwrap_or(true);

    let mut outcome = ScheduleRunOutcome {
        schedule_id: schedule.id.clone(),
        command: schedule.command.clone(),
        status: status.clone(),
        exit_code,
        started_at: started.to_rfc3339(),
        finished_at: finished.to_rfc3339(),
        changed,
        notified: false,
        notify_error: None,
        summary,
    };

    if should_notify(schedule.notify_on, succeeded, changed) {
        let (notified, error) = notify(schedule, &outcome);
        outcome.notified = notified;
        outcome.notify_error = error;
    }

    let _ = save_state(
        &schedule.id,
        &ScheduleState {
            last_run_at: Some(started.to_rfc3339()),
            last_status: Some(status),
            last_exit_code: Some(exit_code),
            last_digest: Some(digest),
            running: false,
            started_at: None,
            consecutive_failures: if succeeded {
                0
            } else {
                previous.consecutive_failures.saturating_add(1)
            },
        },
    );

    outcome
}

fn notify(schedule: &Schedule, outcome: &ScheduleRunOutcome) -> (bool, Option<String>) {
    let route = match (
        schedule.notification_transport.as_deref(),
        schedule.notification_route.as_deref(),
    ) {
        (Some(transport), Some(route)) => {
            match crate::notification_route::NotificationRoute::new(transport, route) {
                Ok(route) => Some(route),
                Err(error) => return (false, Some(error.to_string())),
            }
        }
        _ => None,
    };

    let title = format!("schedule {} — {}", schedule.id, outcome.status);
    let mut body = format!("Command: homeboy {}\n", schedule.command.join(" "));
    body.push_str(&format!(
        "Status: {} (exit {})\n",
        outcome.status, outcome.exit_code
    ));
    body.push_str(&format!(
        "Result: {}\n",
        if outcome.changed {
            "changed since the previous run"
        } else {
            "unchanged"
        }
    ));
    if let Some(summary) = &outcome.summary {
        body.push_str(&format!("Summary: {summary}\n"));
    }

    let event = crate::notify::NotifyEvent {
        run_id: format!("schedule-{}-{}", schedule.id, outcome.started_at),
        status: outcome.status.clone(),
        title,
        body,
        transport: route.as_ref().map(|route| route.transport.clone()),
        route: route.as_ref().map(|route| route.route.clone()),
    };
    let result = crate::notify::dispatch(&event);
    (result.delivered, result.error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::types::{Cadence, OverlapPolicy};

    fn schedule(policy: NotifyPolicy) -> Schedule {
        Schedule {
            id: "digest-fixture".to_string(),
            command: vec!["triage".to_string()],
            every: Cadence::from_seconds(3_600).expect("cadence"),
            notify_on: policy,
            on_overlap: OverlapPolicy::default(),
            notification_transport: None,
            notification_route: None,
            jitter_seconds: None,
            enabled: true,
            description: None,
            aliases: Vec::new(),
        }
    }

    /// The point of `change`: a fleet that is healthy and unchanged stays
    /// silent, so a notification always means something needs a human.
    #[test]
    fn change_policy_is_silent_only_when_healthy_and_unchanged() {
        assert!(!should_notify(NotifyPolicy::Change, true, false));
        assert!(should_notify(NotifyPolicy::Change, true, true));
    }

    /// A repeated identical failure must keep reporting — an ongoing outage
    /// that goes quiet because it is "unchanged" is the worst outcome here.
    #[test]
    fn change_policy_still_reports_a_repeated_failure() {
        assert!(should_notify(NotifyPolicy::Change, false, false));
    }

    #[test]
    fn failure_policy_ignores_changed_successes() {
        assert!(!should_notify(NotifyPolicy::Failure, true, true));
        assert!(should_notify(NotifyPolicy::Failure, false, false));
    }

    #[test]
    fn always_policy_reports_everything() {
        assert!(should_notify(NotifyPolicy::Always, true, false));
        assert!(should_notify(NotifyPolicy::Always, false, true));
    }

    /// Timestamps and run ids change on every single run. If they fed the
    /// digest, `change` would fire every time and be indistinguishable from
    /// `always`.
    #[test]
    fn digest_ignores_volatile_fields() {
        let first = serde_json::json!({
            "data": { "drifted": 0, "components": ["a", "b"] },
            "run_id": "run-1",
            "duration_ms": 12,
            "generated_at": "2026-07-26T00:00:00Z",
        });
        let second = serde_json::json!({
            "data": { "drifted": 0, "components": ["a", "b"] },
            "run_id": "run-2",
            "duration_ms": 9_999,
            "generated_at": "2026-07-27T00:00:00Z",
        });
        assert_eq!(result_digest(&first), result_digest(&second));
    }

    #[test]
    fn digest_reflects_a_real_change() {
        let healthy = serde_json::json!({ "data": { "drifted": 0 } });
        let drifted = serde_json::json!({ "data": { "drifted": 1 } });
        assert_ne!(result_digest(&healthy), result_digest(&drifted));
    }

    #[test]
    fn nested_volatile_fields_are_stripped_too() {
        let first = serde_json::json!({ "data": { "runs": [{ "ok": true, "took_ms": 1 }] } });
        let second = serde_json::json!({ "data": { "runs": [{ "ok": true, "took_ms": 900 }] } });
        assert_eq!(result_digest(&first), result_digest(&second));
    }

    struct StubRunner(serde_json::Value);

    impl ScheduleCommandRunner for StubRunner {
        fn run(&self, _argv: &[String]) -> Result<serde_json::Value> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn a_run_records_state_and_reports_change_against_the_previous_run() {
        crate::test_support::with_isolated_home(|_| {
            let schedule = schedule(NotifyPolicy::Change);
            let runner = StubRunner(serde_json::json!({
                "status": "succeeded",
                "exit_code": 0,
                "data": { "drifted": 0 },
            }));

            let first = run_schedule(&schedule, &runner);
            assert_eq!(first.status, "succeeded");
            assert!(
                first.changed,
                "the first run has nothing to compare against"
            );

            let second = run_schedule(&schedule, &runner);
            assert!(
                !second.changed,
                "an identical result must not be reported as a change"
            );

            let state = load_state(&schedule.id);
            assert!(!state.running, "state must not stay marked in-flight");
            assert_eq!(state.last_exit_code, Some(0));
            assert_eq!(state.consecutive_failures, 0);
        });
    }

    #[test]
    fn a_failing_run_counts_consecutive_failures() {
        crate::test_support::with_isolated_home(|_| {
            let mut schedule = schedule(NotifyPolicy::Failure);
            schedule.id = "failing-fixture".to_string();
            let runner = StubRunner(serde_json::json!({
                "status": "failed",
                "exit_code": 2,
                "data": { "drifted": 3 },
            }));

            run_schedule(&schedule, &runner);
            run_schedule(&schedule, &runner);

            let state = load_state(&schedule.id);
            assert_eq!(state.consecutive_failures, 2);
            assert_eq!(state.last_exit_code, Some(2));
            assert!(!state.running);
        });
    }
}
