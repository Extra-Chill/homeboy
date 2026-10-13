//! Golden fixtures for `advance()`, crash-at-every-commit-boundary replay, and
//! submission-key idempotency.
//!
//! Each `golden/*.json` fixture carries a definition, its inputs, and an event
//! script. The test replays the script through `advance` and compares the full
//! transcript (commands plus a state projection after every event) with the
//! fixture's `transcript`. Regenerate after an intended behavior change with
//! `HOMEBOY_UPDATE_WORKFLOW_GOLDEN=1 cargo test -p homeboy-core workflow::advance`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;

const INSTANCE: &str = "wf-golden";

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workflow/golden")
}

fn parse_definition(value: &Value) -> WorkflowDefinition {
    let definition = WorkflowDefinition::from_value(value.clone()).expect("fixture definition");
    definition
        .validate(&|key| {
            [
                "repo",
                "prompt",
                "verify",
                "placement",
                "task-url",
                "backend",
            ]
            .contains(&key)
        })
        .expect("fixture definition validates");
    definition
}

fn projection(state: &InstanceState) -> Value {
    json!({
        "status": state.status,
        "iteration": state.iteration,
        "step_runs": state.step_runs,
        "failing": state.failing,
        "reason": state.reason,
        "steps": state.steps.iter().map(|(id, step)| {
            (id.clone(), json!({
                "phase": step.phase,
                "attempt": step.attempt,
                "outcome": step.outcome,
                "carried": step.carried,
                "key": step.key,
                "gates": step.gates,
            }))
        }).collect::<BTreeMap<_, _>>(),
        "until": state.until,
    })
}

fn run_script(fixture: &Value) -> (Vec<Value>, InstanceState) {
    let definition = parse_definition(&fixture["definition"]);
    let inputs: BTreeMap<String, Value> =
        serde_json::from_value(fixture["inputs"].clone()).unwrap_or_default();
    let mut state = InstanceState::new(&definition, INSTANCE, inputs);
    let mut transcript = Vec::new();
    for step in fixture["events"].as_array().expect("events") {
        let now_ms = step["now_ms"].as_u64().expect("now_ms");
        let event: Event = serde_json::from_value(step["event"].clone()).expect("event");
        let (next, commands) = advance(&definition, &state, &event, now_ms);
        // Replaying the same event against the committed state decides
        // nothing new: no commands and an identical state.
        let (replayed, replay_commands) = advance(&definition, &next, &event, now_ms);
        if !matches!(event, Event::Tick | Event::Start) {
            assert!(
                replay_commands.is_empty() && replayed == next,
                "replaying {event:?} was not idempotent: {replay_commands:?}"
            );
        }
        transcript.push(json!({
            "now_ms": now_ms,
            "event": event,
            "commands": commands,
            "state": projection(&next),
        }));
        state = next;
    }
    (transcript, state)
}

#[test]
fn golden_transcripts_match() {
    let update = std::env::var_os("HOMEBOY_UPDATE_WORKFLOW_GOLDEN").is_some();
    let mut files = std::fs::read_dir(golden_dir())
        .expect("golden dir")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    files.sort();
    assert!(files.len() >= 10, "golden fixtures are missing");
    let mut covered = BTreeSet::new();
    for path in files {
        let mut fixture: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let (transcript, state) = run_script(&fixture);
        for entry in &transcript {
            for command in entry["commands"].as_array().unwrap() {
                covered.insert(command["command"].as_str().unwrap().to_string());
                if command["command"] == "complete" {
                    covered.insert(format!("complete:{}", command["status"].as_str().unwrap()));
                }
                if let Some(kind) = command["kind"].as_str() {
                    covered.insert(format!("submit:{kind}"));
                }
            }
            for step in entry["state"]["steps"].as_object().unwrap().values() {
                covered.insert(format!("phase:{}", step["phase"].as_str().unwrap()));
                if let Some(outcome) = step["outcome"].as_str() {
                    covered.insert(format!("outcome:{outcome}"));
                }
            }
        }
        assert_eq!(
            fixture["expect_status"].as_str(),
            Some(state.status.as_str()),
            "{}",
            path.display()
        );
        if update {
            fixture["transcript"] = Value::Array(transcript);
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&fixture).unwrap() + "\n",
            )
            .unwrap();
        } else {
            assert_eq!(
                fixture["transcript"],
                Value::Array(transcript),
                "golden transcript drifted: {} (set HOMEBOY_UPDATE_WORKFLOW_GOLDEN=1 to regenerate after an intended change)",
                path.display()
            );
        }
    }
    // Every transition the S3 machine owns appears in at least one fixture.
    for expected in [
        "submit_step",
        "cancel_step",
        "complete",
        "complete:succeeded",
        "complete:failed",
        "complete:budget_exhausted",
        "submit:exec",
        "submit:homeboy",
        "submit:cook",
        "submit:gate",
        "phase:pending",
        "phase:awaiting_approval",
        "phase:running",
        "phase:done",
        "outcome:succeeded",
        "outcome:failed",
        "outcome:skipped",
        "outcome:timed_out",
        "outcome:candidate_recoverable",
    ] {
        assert!(covered.contains(expected), "no fixture covers `{expected}`");
    }
}

// ---------------------------------------------------------------------------
// Crash at every command boundary.
// ---------------------------------------------------------------------------

/// The durable ledger the driver commits to: the instance checkpoint plus the
/// step-job rows keyed by submission key, written in one transaction.
#[derive(Clone, Default)]
struct Ledger {
    checkpoint: Option<InstanceState>,
    rows: BTreeMap<String, Row>,
    /// How many times each key was inserted (must be exactly once).
    inserts: BTreeMap<String, u32>,
    /// How many times each step job's work actually ran.
    executions: BTreeMap<String, u32>,
}

#[derive(Clone)]
struct Row {
    submission: StepSubmission,
    started: bool,
    cancel_requested: bool,
    result: Option<StepResult>,
}

/// Scripted step work: what each submitted step job produces.
fn world(submission: &StepSubmission) -> StepResult {
    let output = match (submission.step.as_str(), submission.iteration) {
        ("evaluate", 1) => Some(json!({"verdict": "findings", "report": "r1.json"})),
        ("evaluate", _) => Some(json!({"verdict": "accepted"})),
        _ => None,
    };
    let (status, outcome) = match (submission.step.as_str(), submission.attempt) {
        // The fix Cook fails its first attempt and is retried.
        ("fix", 1) => (FinishedStatus::Failed, Some("failed".to_string())),
        ("fix", _) => (FinishedStatus::Succeeded, Some("succeeded".to_string())),
        _ => (FinishedStatus::Succeeded, None),
    };
    let run_id = match &submission.kind {
        SubmissionKind::Cook { cook_id, .. } => Some(cook_id.clone()),
        _ => None,
    };
    StepResult {
        key: submission.key.clone(),
        status,
        outcome,
        output,
        run_id,
    }
}

fn proof_definition() -> WorkflowDefinition {
    parse_definition(&json!({
        "schema": "homeboy/workflow/v1",
        "id": "proof",
        "inputs": {"url": {"required": true}},
        "budget": {"max_step_runs": 12},
        "iterate": {"until": {"step": "evaluate", "output": "/verdict", "equals": "accepted"}, "max_iterations": 3},
        "steps": [
            {"id": "capture", "when": {"not": {"step": "capture", "outcome": ["succeeded"]}},
             "exec": {"program": "true"}},
            {"id": "evaluate", "needs": ["capture"],
             "exec": {"program": "node", "args": ["evaluate", "{{inputs.url}}"]}},
            {"id": "fix", "needs": ["evaluate"],
             "when": {"all": [
                {"step": "evaluate", "output": "/verdict", "equals": "findings"},
                {"command": ["homeboy", "review", "lint", "{{inputs.url}}"]}
             ]},
             "retry": {"max": 1, "on": ["failed"]},
             "cook": {"repo": "scratch", "prompt": "fix {{steps.evaluate.output/report}}", "verify": ["true"]}},
            {"id": "approve", "needs": ["fix"], "always": true,
             "approval": {"prompt": "ship {{instance}}?"}}
        ]
    }))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Crash {
    /// The process died before the transaction committed.
    BeforeCommit,
    /// The transaction committed; the process died before it started the
    /// submitted jobs or signalled the cancelled ones.
    AfterCommit,
}

/// Drive the definition to completion. `crash` names the transaction (by its
/// 1-based order) at which the driver dies once.
fn drive(definition: &WorkflowDefinition, crash: Option<(usize, Crash)>) -> Ledger {
    let mut ledger = Ledger::default();
    let mut transaction = 0;
    let mut crash = crash;
    let mut now_ms = 0;
    let inputs = BTreeMap::from([("url".to_string(), json!("https://example.test"))]);
    for _ in 0..500 {
        now_ms += 1_000;
        // Recovery: the driver restarts from the committed checkpoint and
        // starts any admitted job that has not started.
        let state = ledger
            .checkpoint
            .clone()
            .unwrap_or_else(|| InstanceState::new(definition, INSTANCE, inputs.clone()));
        if state.status.is_terminal() {
            return ledger;
        }
        // Events are derived only from the durable ledger.
        let mut events = Vec::new();
        if state.started_at_ms.is_none() {
            events.push(Event::Start);
        }
        for key in state.awaited_keys() {
            if let Some(result) = ledger.rows.get(&key).and_then(|row| row.result.clone()) {
                events.push(Event::StepFinished(result));
            }
        }
        if let Some((id, _)) = state
            .steps
            .iter()
            .find(|(_, step)| step.phase == StepPhase::AwaitingApproval)
        {
            events.push(Event::Approved {
                step: id.clone(),
                iteration: state.iteration,
            });
        }
        events.push(Event::Tick);

        let mut state = state;
        let mut died = false;
        for event in events {
            let (next, commands) = advance(definition, &state, &event, now_ms);
            if next == state && commands.is_empty() {
                continue;
            }
            transaction += 1;
            let crash_here = crash
                .filter(|(at, _)| *at == transaction)
                .map(|(_, kind)| kind);
            if crash_here == Some(Crash::BeforeCommit) {
                crash = None;
                died = true;
                break;
            }
            // One transaction: checkpoint + rows + submission keys + cancels.
            for command in &commands {
                match command {
                    Command::SubmitStep(submission) => {
                        if !ledger.rows.contains_key(&submission.key) {
                            *ledger.inserts.entry(submission.key.clone()).or_default() += 1;
                            ledger.rows.insert(
                                submission.key.clone(),
                                Row {
                                    submission: submission.clone(),
                                    started: false,
                                    cancel_requested: false,
                                    result: None,
                                },
                            );
                        }
                    }
                    Command::CancelStep { key, .. } => {
                        if let Some(row) = ledger.rows.get_mut(key) {
                            row.cancel_requested = true;
                        }
                    }
                    Command::Complete { .. } => {}
                }
            }
            ledger.checkpoint = Some(next.clone());
            state = next;
            if crash_here == Some(Crash::AfterCommit) {
                crash = None;
                died = true;
                break;
            }
        }
        if died {
            continue;
        }
        // Step jobs run (the daemon starts queued rows on recovery too).
        for (key, row) in ledger.rows.iter_mut() {
            if row.result.is_some() {
                continue;
            }
            if row.cancel_requested {
                row.result = Some(StepResult {
                    key: key.clone(),
                    status: FinishedStatus::Cancelled,
                    outcome: None,
                    output: None,
                    run_id: None,
                });
                continue;
            }
            if !row.started {
                row.started = true;
                *ledger.executions.entry(key.clone()).or_default() += 1;
                row.result = Some(world(&row.submission));
            }
        }
    }
    panic!("workflow did not terminate");
}

#[test]
fn crash_at_every_command_boundary_converges_without_running_a_step_twice() {
    let definition = proof_definition();
    let baseline = drive(&definition, None);
    let final_state = baseline.checkpoint.clone().unwrap();
    assert_eq!(final_state.status, InstanceStatus::Succeeded);
    assert_eq!(final_state.iteration, 2);
    assert_eq!(
        final_state.steps["capture"].outcome.as_deref(),
        Some("succeeded")
    );
    assert!(
        final_state.steps["capture"].carried,
        "capture is carried into iteration 2"
    );
    let keys = baseline.inserts.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "wf-golden/capture/1/1",
            "wf-golden/evaluate/1/1",
            "wf-golden/evaluate/2/1",
            "wf-golden/fix/1/1",
            "wf-golden/fix/1/2",
            "wf-golden/fix/1/when-0",
        ]
    );
    let boundaries = count_transactions(&definition);
    assert!(
        boundaries > 8,
        "expected many commit boundaries, got {boundaries}"
    );
    for at in 1..=boundaries {
        for kind in [Crash::BeforeCommit, Crash::AfterCommit] {
            let ledger = drive(&definition, Some((at, kind)));
            let state = ledger.checkpoint.clone().unwrap();
            assert_eq!(
                projection(&state),
                projection(&final_state),
                "crash {kind:?} at transaction {at} diverged"
            );
            assert_eq!(state.history, final_state.history);
            for (key, inserts) in &ledger.inserts {
                assert_eq!(
                    *inserts, 1,
                    "{key} inserted {inserts} times ({kind:?} at {at})"
                );
            }
            for (key, runs) in &ledger.executions {
                assert_eq!(*runs, 1, "{key} ran {runs} times ({kind:?} at {at})");
            }
            assert_eq!(
                ledger.inserts.keys().collect::<Vec<_>>(),
                baseline.inserts.keys().collect::<Vec<_>>(),
                "crash {kind:?} at {at} submitted a different key set"
            );
        }
    }
}

/// The number of committed transactions in an uninterrupted run.
fn count_transactions(definition: &WorkflowDefinition) -> usize {
    let mut state = InstanceState::new(
        definition,
        INSTANCE,
        BTreeMap::from([("url".to_string(), json!("https://example.test"))]),
    );
    let mut rows: BTreeMap<String, (StepSubmission, Option<StepResult>)> = BTreeMap::new();
    let mut transactions = 0;
    let mut now_ms = 0;
    while !state.status.is_terminal() {
        now_ms += 1_000;
        let mut events = Vec::new();
        if state.started_at_ms.is_none() {
            events.push(Event::Start);
        }
        for key in state.awaited_keys() {
            if let Some((_, Some(result))) = rows.get(&key) {
                events.push(Event::StepFinished(result.clone()));
            }
        }
        if let Some((id, _)) = state
            .steps
            .iter()
            .find(|(_, step)| step.phase == StepPhase::AwaitingApproval)
        {
            events.push(Event::Approved {
                step: id.clone(),
                iteration: state.iteration,
            });
        }
        events.push(Event::Tick);
        for event in events {
            let (next, commands) = advance(definition, &state, &event, now_ms);
            if next == state && commands.is_empty() {
                continue;
            }
            transactions += 1;
            for command in commands {
                if let Command::SubmitStep(submission) = command {
                    rows.entry(submission.key.clone())
                        .or_insert((submission, None));
                }
            }
            state = next;
        }
        for (submission, result) in rows.values_mut() {
            if result.is_none() {
                *result = Some(world(submission));
            }
        }
    }
    transactions
}

#[test]
fn terminal_instances_ignore_every_event() {
    let definition = proof_definition();
    let mut state = drive(&definition, None).checkpoint.unwrap();
    state.status = InstanceStatus::Cancelled;
    for event in [
        Event::Start,
        Event::Tick,
        Event::Approved {
            step: "approve".into(),
            iteration: 2,
        },
        Event::StepFinished(StepResult {
            key: "wf-golden/fix/1/2".into(),
            status: FinishedStatus::Succeeded,
            outcome: None,
            output: None,
            run_id: None,
        }),
    ] {
        let (next, commands) = advance(&definition, &state, &event, 99_999);
        assert_eq!(next, state);
        assert!(commands.is_empty());
    }
}
