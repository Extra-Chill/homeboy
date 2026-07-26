//! Running a schedule and deciding whether the result is worth reporting.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::state::{load_state, save_state, ScheduleState};
use super::types::{NotifyPolicy, Schedule, ScheduledCommand};

/// Largest amount of external-command output retained.
///
/// A chatty program must not be able to grow the runtime state file without
/// bound, and only the tail is ever reported.
pub const MAX_CAPTURED_OUTPUT_BYTES: usize = 64 * 1024;

/// Longest summary carried into a notification.
pub const MAX_SUMMARY_CHARS: usize = 500;

/// What a scheduled command produced.
///
/// Homeboy commands emit a structured envelope; external programs do not, so
/// their result is carried as raw output plus an exit code and fingerprinted
/// differently.
#[derive(Debug, Clone)]
pub enum ScheduleCommandResult {
    Envelope(serde_json::Value),
    Raw { exit_code: i32, output: String },
}

/// What a single schedule run produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRunOutcome {
    pub schedule_id: String,
    pub command: String,
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
    /// Run a scheduled command and return its result.
    fn run(&self, command: ScheduledCommand<'_>) -> Result<ScheduleCommandResult>;
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
    fn run(&self, command: ScheduledCommand<'_>) -> Result<ScheduleCommandResult> {
        match command {
            ScheduledCommand::Homeboy(argv) => self.run_homeboy(argv),
            ScheduledCommand::Exec(exec) => run_external(exec),
        }
    }
}

impl SubprocessRunner {
    fn run_homeboy(&self, argv: &[String]) -> Result<ScheduleCommandResult> {
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
            return Ok(ScheduleCommandResult::Envelope(serde_json::json!({
                "schema": "homeboy/command-result/v3",
                "success": status.success(),
                "exit_code": status.code().unwrap_or(-1),
                "status": if status.success() { "succeeded" } else { "failed" },
            })));
        }
        serde_json::from_str(&raw)
            .map(ScheduleCommandResult::Envelope)
            .map_err(|error| {
                Error::internal_io(
                    format!("Scheduled command wrote output that is not valid JSON: {error}"),
                    Some(output_path.display().to_string()),
                )
            })
    }
}

/// Run an external program directly.
///
/// No shell: the program and its arguments are passed as an argument vector,
/// so nothing is word-split and there is no quoting or injection surface.
/// stdout and stderr are captured together, because a failing command usually
/// explains itself on stderr and that is what an operator needs in the
/// notification.
fn run_external(exec: &super::types::ExecCommand) -> Result<ScheduleCommandResult> {
    let mut command = std::process::Command::new(&exec.program);
    command.args(&exec.args);
    if let Some(dir) = exec.working_dir.as_deref() {
        command.current_dir(dir);
    }
    let output = command.output().map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to run scheduled program '{}': {error}",
                exec.program
            ),
            exec.working_dir.clone(),
        )
    })?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    Ok(ScheduleCommandResult::Raw {
        exit_code: output.status.code().unwrap_or(-1),
        output: bound_output(&combined),
    })
}

/// Keep the tail of oversized output. The end of a run is where failures and
/// summaries appear; the beginning is usually setup noise.
pub fn bound_output(value: &str) -> String {
    if value.len() <= MAX_CAPTURED_OUTPUT_BYTES {
        return value.to_string();
    }
    let start = value.len() - MAX_CAPTURED_OUTPUT_BYTES;
    // Do not split a UTF-8 character.
    let start = (start..value.len())
        .find(|index| value.is_char_boundary(*index))
        .unwrap_or(value.len());
    format!("[output truncated]\n{}", &value[start..])
}

/// Reduce a command result to the fields the scheduler reasons about.
fn summarize(result: &ScheduleCommandResult) -> (String, i32, String, Option<String>) {
    match result {
        ScheduleCommandResult::Envelope(value) => {
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
        ScheduleCommandResult::Raw { exit_code, output } => {
            let status = if *exit_code == 0 {
                "succeeded"
            } else {
                "failed"
            };
            // An external program has no envelope, so its output *is* the
            // result: fingerprint the output together with the exit code so a
            // probe whose output stops changing goes quiet, and one whose
            // output changes reports.
            let digest = raw_digest(*exit_code, output);
            (status.to_string(), *exit_code, digest, summary_tail(output))
        }
    }
}

/// Fingerprint an external command's output and exit code.
pub fn raw_digest(exit_code: i32, output: &str) -> String {
    let rendered = format!("{exit_code}\u{1e}{output}");
    let digest = <sha2::Sha256 as sha2::Digest>::digest(rendered.as_bytes());
    format!("{digest:x}")
}

/// The tail of a command's output, bounded for a notification.
fn summary_tail(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= MAX_SUMMARY_CHARS {
        return Some(trimmed.to_string());
    }
    let tail: String = chars[chars.len() - MAX_SUMMARY_CHARS..].iter().collect();
    Some(format!("…{tail}"))
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

    let result = match schedule.scheduled_command() {
        Some(command) => runner.run(command),
        None => Err(Error::validation_invalid_argument(
            "command",
            "Schedule declares neither a homeboy command nor a program to run",
            Some(schedule.id.clone()),
            None,
        )),
    };
    let finished = chrono::Utc::now();

    let (status, exit_code, digest, summary) = match &result {
        Ok(outcome) => summarize(outcome),
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
        command: schedule.command_display(),
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
    let mut body = format!("Command: {}\n", schedule.command_display());
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
            command: Some(vec!["triage".to_string()]),
            exec: None,
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

    fn exec_schedule(program: &str, args: &[&str]) -> Schedule {
        Schedule {
            id: format!("exec-{program}"),
            command: None,
            exec: Some(super::super::types::ExecCommand {
                program: program.to_string(),
                args: args.iter().map(|arg| arg.to_string()).collect(),
                working_dir: None,
            }),
            every: Cadence::from_seconds(3_600).expect("cadence"),
            notify_on: NotifyPolicy::Change,
            on_overlap: OverlapPolicy::default(),
            notification_transport: None,
            notification_route: None,
            jitter_seconds: None,
            enabled: true,
            description: None,
            aliases: Vec::new(),
        }
    }

    /// An external program has no result envelope, so its output *is* the
    /// result and has to be fingerprinted directly.
    #[test]
    fn an_external_program_reports_its_exit_code_and_output() {
        crate::test_support::with_isolated_home(|_| {
            let runner = SubprocessRunner::new().expect("runner");
            let schedule = exec_schedule("echo", &["scheduled output"]);

            let outcome = run_schedule(&schedule, &runner);
            assert_eq!(outcome.status, "succeeded");
            assert_eq!(outcome.exit_code, 0);
            assert_eq!(outcome.summary.as_deref(), Some("scheduled output"));
            assert!(outcome.changed, "the first run has nothing to compare to");

            // Identical output must not be reported as a change.
            let again = run_schedule(&schedule, &runner);
            assert!(!again.changed, "identical output is not a change");
        });
    }

    #[test]
    fn a_failing_external_program_is_reported_as_failed() {
        crate::test_support::with_isolated_home(|_| {
            let runner = SubprocessRunner::new().expect("runner");
            let mut schedule = exec_schedule("false", &[]);
            schedule.id = "exec-failing".to_string();

            let outcome = run_schedule(&schedule, &runner);
            assert_eq!(outcome.status, "failed");
            assert_ne!(outcome.exit_code, 0);
            assert_eq!(load_state("exec-failing").consecutive_failures, 1);
        });
    }

    /// A missing program must be a recorded failure, not a panic or a run that
    /// silently looks successful.
    #[test]
    fn a_missing_program_fails_the_run() {
        crate::test_support::with_isolated_home(|_| {
            let runner = SubprocessRunner::new().expect("runner");
            let mut schedule = exec_schedule("definitely-not-a-real-program-10133", &[]);
            schedule.id = "exec-missing".to_string();

            let outcome = run_schedule(&schedule, &runner);
            assert_eq!(outcome.status, "failed");
            assert!(!load_state("exec-missing").running, "state must settle");
        });
    }

    /// Arguments are passed as a vector, so one containing spaces stays one
    /// argument rather than being word-split as a shell would.
    #[test]
    fn arguments_are_not_word_split() {
        crate::test_support::with_isolated_home(|_| {
            let runner = SubprocessRunner::new().expect("runner");
            let mut schedule = exec_schedule("echo", &["one two three"]);
            schedule.id = "exec-spaces".to_string();

            let outcome = run_schedule(&schedule, &runner);
            assert_eq!(outcome.summary.as_deref(), Some("one two three"));
        });
    }

    /// Changed output must report even though the exit code is unchanged —
    /// that is the whole point of scheduling a probe.
    #[test]
    fn raw_digest_tracks_output_and_exit_code_independently() {
        assert_eq!(raw_digest(0, "healthy"), raw_digest(0, "healthy"));
        assert_ne!(
            raw_digest(0, "healthy"),
            raw_digest(0, "degraded"),
            "changed output must change the digest"
        );
        assert_ne!(
            raw_digest(0, "same"),
            raw_digest(1, "same"),
            "a changed exit code must change the digest even with identical output"
        );
    }

    /// A chatty program must not be able to grow the state file without bound.
    #[test]
    fn oversized_output_is_bounded_and_keeps_the_tail() {
        let noisy = format!("{}TAIL-MARKER", "x".repeat(MAX_CAPTURED_OUTPUT_BYTES * 2));
        let bounded = bound_output(&noisy);

        assert!(bounded.len() < noisy.len(), "output must be bounded");
        assert!(
            bounded.ends_with("TAIL-MARKER"),
            "the tail is where failures appear and must survive"
        );
        assert!(bounded.starts_with("[output truncated]"));
    }

    #[test]
    fn bounding_does_not_split_a_utf8_character() {
        let noisy = "é".repeat(MAX_CAPTURED_OUTPUT_BYTES);
        let bounded = bound_output(&noisy);
        assert!(bounded.contains('é'), "multi-byte output must stay valid");
    }

    struct StubRunner(serde_json::Value);

    impl ScheduleCommandRunner for StubRunner {
        fn run(&self, _command: ScheduledCommand<'_>) -> Result<ScheduleCommandResult> {
            Ok(ScheduleCommandResult::Envelope(self.0.clone()))
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
