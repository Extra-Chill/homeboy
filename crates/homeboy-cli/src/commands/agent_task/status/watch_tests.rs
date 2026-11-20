use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

fn args() -> StatusArgs {
    StatusArgs {
        run_id: "run-1".to_string(),
        watch: true,
        interval: "250ms".to_string(),
        timeout: "2m".to_string(),
        ..Default::default()
    }
}

fn result(state: &str, conclusion: WatchConclusion) -> WatchResult<(Value, i32)> {
    WatchResult {
        item: (json!({ "run_id": "run-1", "state": state }), 0),
        conclusion,
        poll_count: 3,
        waited: Duration::from_secs(1),
    }
}

#[test]
fn changed_status_emission_is_live_and_skips_identical_heartbeat_polls() {
    let mut progress = StatusWatchProgress::default();
    let mut emitted = Vec::new();
    let running = json!({ "run_id": "run-1", "state": "running", "progress": 1 });
    let succeeded = json!({ "run_id": "run-1", "state": "succeeded", "progress": 2 });

    progress.observe(&running, 1, |line| emitted.push(line));
    progress.observe(&running, 2, |line| emitted.push(line));
    progress.observe(&succeeded, 3, |line| emitted.push(line));

    let first: Value = serde_json::from_str(&emitted[0]).expect("first JSONL event");
    let second: Value = serde_json::from_str(&emitted[1]).expect("second JSONL event");
    assert_eq!(first["schema"], "homeboy/agent-task-status-watch-event/v2");
    assert_eq!(first["event"], "status_changed");
    assert_eq!(first["poll"], 1);
    assert_eq!(first["run_id"], "run-1");
    assert_eq!(first["state"], "running");
    assert_eq!(first["change"]["state"], "running");
    assert_eq!(first["change"]["change_basis"]["state"], "running");
    assert!(first["change"]["status"].is_null());
    assert_eq!(
        first["continuation_command"],
        "homeboy agent-task status run-1 --watch"
    );
    assert_eq!(second["poll"], 3);
    assert_eq!(second["state"], "succeeded");
    assert_eq!(progress.changes.len(), 2);
    assert_eq!(progress.changes[0]["poll"], 1);
    assert_eq!(progress.changes[1]["poll"], 3);
}

#[test]
fn changed_status_ignores_volatile_read_metadata() {
    let mut progress = StatusWatchProgress::default();
    let mut emitted = Vec::new();
    progress.observe(
        &json!({ "run_id": "run-1", "state": "running", "updated_at": "2026-08-13T00:00:00Z" }),
        1,
        |line| emitted.push(line),
    );
    progress.observe(
        &json!({ "run_id": "run-1", "state": "running", "updated_at": "2026-08-13T00:00:01Z" }),
        2,
        |line| emitted.push(line),
    );

    assert_eq!(emitted.len(), 1);
}

#[test]
fn changed_status_retention_and_live_output_are_bounded() {
    let mut progress = StatusWatchProgress::default();
    let mut emitted = Vec::new();
    for poll in 1..=(STATUS_WATCH_CHANGE_LIMIT as u64 + 2) {
        progress.observe(
            &json!({ "run_id": "run-1", "state": "running", "heartbeat_at": format!("2026-08-13T00:00:{poll:02}Z") }),
            poll,
            |line| emitted.push(line),
        );
    }
    let terminal_poll = STATUS_WATCH_CHANGE_LIMIT as u64 + 3;
    progress.observe(
        &json!({ "run_id": "run-1", "state": "succeeded", "heartbeat_at": "2026-08-13T00:01:00Z" }),
        terminal_poll,
        |line| emitted.push(line),
    );

    assert_eq!(progress.changes.len(), STATUS_WATCH_CHANGE_LIMIT);
    assert_eq!(progress.omitted, 3);
    assert_eq!(emitted.len(), STATUS_WATCH_CHANGE_LIMIT + 3);
    let first_overflow: Value =
        serde_json::from_str(&emitted[STATUS_WATCH_CHANGE_LIMIT]).expect("first overflow event");
    assert_eq!(first_overflow["poll"], STATUS_WATCH_CHANGE_LIMIT as u64 + 1);
    assert_eq!(first_overflow["change"]["state"], "running");
    assert!(first_overflow["change"]["change_basis"].is_object());
    let overflow: Value = serde_json::from_str(emitted.last().unwrap()).expect("overflow event");
    assert_eq!(overflow["retained_limit_reached"], true);
    assert_eq!(overflow["state"], "succeeded");
    assert_eq!(overflow["poll"], terminal_poll);
}

#[test]
fn watch_result_has_a_total_byte_budget() {
    let oversized = json!({
        "run_id": "run-1",
        "state": "running",
        "diagnostics": "x".repeat(STATUS_WATCH_BYTE_LIMIT * 2),
    });
    let (output, _) = watch_status_output(
        &args(),
        WatchResult {
            item: (oversized.clone(), 0),
            conclusion: WatchConclusion::TimedOut,
            poll_count: 1,
            waited: Duration::from_secs(1),
        },
        StatusWatchProgress {
            changes: vec![status_watch_change(&oversized, 1)],
            ..Default::default()
        },
    );

    assert!(serialized_len(&output) <= STATUS_WATCH_BYTE_LIMIT);
    assert_eq!(output["output_budget"]["truncated"], true);
    assert_eq!(output["latest"]["state"], "running");
    assert!(output["terminal_summary"].is_null());
}

#[test]
fn terminal_summary_uses_terminal_durable_state_counts_and_diagnostic() {
    let terminal = json!({
        "run_id": "run-1",
        "state": "cancelled",
        "phase": "terminal",
        "blocker": { "code": "controller_preflight", "message": "workspace is unavailable" },
        "gates": { "state": "blocked" },
        "metadata": { "large": "x".repeat(COMPACT_STATUS_BYTE_LIMIT) },
    });
    let (output, exit) = watch_status_output(
        &args(),
        WatchResult {
            item: (terminal.clone(), 0),
            conclusion: WatchConclusion::Terminal,
            poll_count: 2,
            waited: Duration::from_secs(1),
        },
        StatusWatchProgress {
            changes: vec![status_watch_change(&terminal, 1)],
            ..Default::default()
        },
    );

    assert_eq!(exit, 1);
    assert_eq!(output["terminal_summary"]["state"], "cancelled");
    assert_eq!(output["terminal_summary"]["phase"], "terminal");
    assert_eq!(
        output["terminal_summary"]["blocker"]["code"],
        "controller_preflight"
    );
    assert_eq!(output["terminal_summary"]["gates"]["state"], "blocked");
    assert!(serialized_len(&output) <= STATUS_WATCH_BYTE_LIMIT);
}

#[test]
fn canonical_gate_changes_emit_a_new_event() {
    let mut progress = StatusWatchProgress::default();
    let mut emitted = Vec::new();
    progress.observe(
        &json!({ "run_id": "run-1", "state": "running", "gates": { "state": "pending" } }),
        1,
        |line| emitted.push(line),
    );
    progress.observe(
        &json!({ "run_id": "run-1", "state": "running", "gates": { "state": "failed" } }),
        2,
        |line| emitted.push(line),
    );

    assert_eq!(emitted.len(), 2);
    let second: Value = serde_json::from_str(&emitted[1]).expect("gate change event");
    assert_eq!(second["change"]["gates"]["state"], "failed");
}

#[test]
fn fixed_watch_sections_fall_back_to_typed_refs_within_total_budget() {
    let mut output = json!({
        "run_id": "run-1",
        "changes": [],
        "changes_omitted": 0,
        "latest": { "schema": "homeboy/agent-task-status-watch-latest/v2", "state": "failed", "detail": "x".repeat(STATUS_WATCH_BYTE_LIMIT) },
        "terminal_summary": { "schema": "homeboy/agent-task-status-watch-terminal/v1", "detail": "x".repeat(STATUS_WATCH_BYTE_LIMIT) },
        "output_budget": {},
    });
    enforce_watch_output_budget(&mut output, "homeboy agent-task status run-1 --watch");

    assert!(serialized_len(&output) <= STATUS_WATCH_BYTE_LIMIT);
    assert_eq!(
        output["terminal_summary"]["schema"],
        "homeboy/agent-task-status-watch-terminal-ref/v2"
    );
    assert_eq!(
        output["latest"]["schema"],
        "homeboy/agent-task-status-watch-latest-ref/v2"
    );
}

struct ScriptedStatusPoller {
    snapshots: RefCell<VecDeque<Value>>,
}

impl ScriptedStatusPoller {
    fn new(snapshots: Vec<Value>) -> Self {
        Self {
            snapshots: RefCell::new(snapshots.into()),
        }
    }
}

impl WatchPoller for ScriptedStatusPoller {
    type Item = (Value, i32);

    fn poll(&self, _id: &str) -> homeboy::core::Result<Self::Item> {
        let mut snapshots = self.snapshots.borrow_mut();
        let snapshot = if snapshots.len() > 1 {
            snapshots.pop_front().expect("scripted snapshot")
        } else {
            snapshots.front().expect("scripted snapshot").clone()
        };
        Ok((snapshot, 0))
    }

    fn is_terminal(&self, item: &Self::Item) -> bool {
        status_is_terminal(&item.0)
    }
}

#[test]
fn scripted_watch_emits_progress_before_returning_timeout_partial_status() {
    let poller = ScriptedStatusPoller::new(vec![
        json!({ "run_id": "run-1", "state": "queued" }),
        json!({ "run_id": "run-1", "state": "running" }),
    ]);
    let clock = Cell::new(Duration::ZERO);
    let mut progress = StatusWatchProgress::default();
    let mut emitted = Vec::new();
    let result = watch_loop(
        &poller,
        "run-1",
        &WatchConfig {
            interval: Duration::from_secs(1),
            timeout: Some(Duration::from_secs(2)),
        },
        |duration| clock.set(clock.get() + duration),
        || clock.get(),
        |(snapshot, _), poll| progress.observe(snapshot, poll, |line| emitted.push(line)),
    )
    .expect("scripted status watch");
    let (output, exit) = watch_status_output(&args(), result, progress);

    assert_eq!(exit, TIMEOUT_EXIT_CODE);
    assert_eq!(output["latest"]["state"], "running");
    assert_eq!(output["changes"].as_array().unwrap().len(), 2);
    assert_eq!(
        emitted.len(),
        2,
        "progress was emitted before timeout returned"
    );
    let queued: Value = serde_json::from_str(&emitted[0]).expect("queued event");
    let running: Value = serde_json::from_str(&emitted[1]).expect("running event");
    assert_eq!(queued["state"], "queued");
    assert_eq!(running["state"], "running");
}

#[test]
fn terminal_success_returns_latest_status_and_zero() {
    let (output, exit) = watch_status_output(
        &args(),
        result("succeeded", WatchConclusion::Terminal),
        StatusWatchProgress {
            changes: vec![json!({ "poll": 1, "status": { "state": "succeeded" } })],
            ..Default::default()
        },
    );

    assert_eq!(exit, 0);
    assert_eq!(output["terminal"], true);
    assert_eq!(output["latest"]["state"], "succeeded");
    assert_eq!(output["changes"].as_array().unwrap().len(), 1);
    assert_eq!(
        output["continuation_command"],
        "homeboy agent-task status run-1 --watch --interval 250ms --timeout 2m"
    );
}

#[test]
fn terminal_failure_and_cancellation_exit_nonzero() {
    for state in ["failed", "cancelled"] {
        let (_, exit) = watch_status_output(
            &args(),
            result(state, WatchConclusion::Terminal),
            StatusWatchProgress::default(),
        );
        assert_eq!(exit, 1, "{state}");
    }
    // Promotion outcomes are candidate facts, not lifecycle state names.
    // The production owner projects gate failure as candidate_recoverable.
    for (state, outcome) in [
        ("candidate_recoverable", "gate_failed"),
        ("failed", "finalization_failed"),
    ] {
        let mut terminal = result(state, WatchConclusion::Terminal);
        terminal.item.0["blocker"] = json!({ "code": outcome });
        if outcome == "gate_failed" {
            terminal.item.0["candidate"] = json!({ "state": outcome });
        }
        assert!(status_is_terminal(&terminal.item.0), "{outcome}");
        let (_, exit) = watch_status_output(&args(), terminal, StatusWatchProgress::default());
        assert_eq!(exit, 1, "{outcome}");
    }
}

#[test]
fn recipe_only_watch_uses_the_successful_one_shot_status_exit() {
    let (output, exit) = watch_status_output(
        &args(),
        WatchResult {
            item: (
                json!({
                    "schema": "homeboy/agent-task-cook/v1",
                    "run_id": "run-1",
                    "status": "recipe_only_recovery_required"
                }),
                0,
            ),
            conclusion: WatchConclusion::Terminal,
            poll_count: 1,
            waited: Duration::ZERO,
        },
        StatusWatchProgress::default(),
    );

    assert_eq!(exit, 0);
    assert_eq!(output["latest"]["status"], "recipe_only_recovery_required");
}

#[test]
fn timeout_retains_partial_runner_status_and_uses_shared_exit_code() {
    let (output, exit) = watch_status_output(
        &args(),
        WatchResult {
            item: (
                json!({
                    "run_id": "run-1",
                    "state": "running",
                    "reconciled": true,
                    "runner_probe": { "performed": true }
                }),
                0,
            ),
            conclusion: WatchConclusion::TimedOut,
            poll_count: 5,
            waited: Duration::from_secs(120),
        },
        StatusWatchProgress::default(),
    );

    assert_eq!(exit, TIMEOUT_EXIT_CODE);
    assert_eq!(output["terminal"], false);
    assert_eq!(output["timed_out"], true);
    assert_eq!(output["latest"]["reconciled"], true);
    assert_eq!(output["latest"]["runner_probe"]["performed"], true);
}

#[test]
fn canonical_snapshot_is_retained_without_projection_loss() {
    let snapshot = json!({
        "schema": "homeboy/control-plane-run/v1",
        "run": "run-1",
        "state": "succeeded",
        "phase": "terminal"
    });
    let (output, exit) = watch_status_output(
        &args(),
        WatchResult {
            item: (snapshot.clone(), 0),
            conclusion: WatchConclusion::Terminal,
            poll_count: 1,
            waited: Duration::ZERO,
        },
        StatusWatchProgress {
            changes: vec![status_watch_change(&snapshot, 1)],
            ..Default::default()
        },
    );

    assert_eq!(exit, 0);
    assert_eq!(output["latest"]["run"], "run-1");
    assert_eq!(output["latest"]["phase"], "terminal");
}

#[test]
fn terminality_keeps_active_states_open_and_accepts_unknown_terminal_states() {
    for state in ["queued", "running", "in_flight"] {
        assert!(!status_is_terminal(&json!({ "state": state })), "{state}");
    }
    assert!(status_is_terminal(&json!({ "state": "succeeded" })));
    assert!(status_is_terminal(&json!({ "state": "failed" })));
    assert!(status_is_terminal(&json!({ "state": "cancelled" })));
}
