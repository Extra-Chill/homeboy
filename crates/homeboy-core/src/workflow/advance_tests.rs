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

#[test]
fn foreach_failure_keeps_item_error_and_first_failure_reason() {
    let definition = parse_definition(&json!({
        "schema": "homeboy/workflow/v1",
        "id": "fan-error",
        "steps": [{
            "id": "work", "foreach": ["a", "b"], "max_parallel": 2,
            "exec": {"program": "sh", "args": ["-c", "false"]}
        }]
    }));
    let initial = InstanceState::new(&definition, INSTANCE, BTreeMap::new());
    let (mut state, _) = advance(&definition, &initial, &Event::Start, 0);
    let keys: Vec<_> = state.steps["work"]
        .items
        .iter()
        .map(|item| item.key.clone().unwrap())
        .collect();
    for (index, key) in keys.into_iter().enumerate() {
        let error = format!("failure {index}");
        (state, _) = advance(
            &definition,
            &state,
            &Event::StepFinished(StepResult {
                key,
                status: FinishedStatus::Failed,
                error: Some(error.clone()),
                outcome: None,
                output: None,
                run_id: None,
            }),
            1,
        );
        assert_eq!(
            state.steps["work"].items[index].error.as_deref(),
            Some(error.as_str())
        );
    }
    assert_eq!(
        state.reason.as_deref(),
        Some("step `work` did not succeed: failure 0")
    );
}

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
            let mut projected = json!({
                "phase": step.phase,
                "attempt": step.attempt,
                "outcome": step.outcome,
                "key": step.key,
            });
            if !step.items.is_empty() {
                projected["items"] = step.items.iter().map(|item| json!({
                    "phase": item.phase,
                    "attempt": item.attempt,
                    "outcome": item.outcome,
                    "key": item.key,
                })).collect();
            }
            if let Some(error) = &step.error {
                projected["error"] = json!(error);
            }
            (id.clone(), projected)
        }).collect::<BTreeMap<_, _>>(),
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
    assert!(files.len() >= 18, "golden fixtures are missing");
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
                for item in step["items"].as_array().into_iter().flatten() {
                    if let Some(outcome) = item["outcome"].as_str() {
                        covered.insert(format!("item:{outcome}"));
                    }
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
    // Every transition the machine owns appears in at least one fixture.
    for expected in [
        "submit_step",
        "cancel_step",
        "complete",
        "complete:succeeded",
        "complete:failed",
        "complete:budget_exhausted",
        "complete:cancelled",
        "outcome:partial_failure",
        "item:succeeded",
        "item:failed",
        "item:timed_out",
        "submit:exec",
        "submit:homeboy",
        "submit:cook",
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
    let output = (submission.step == "evaluate").then(|| {
        json!({"verdict": "findings", "report": "r1.json"})
    });
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
        error: None,
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
        "steps": [
            {"id": "capture", "exec": {"program": "true"}},
            {"id": "evaluate", "needs": ["capture"],
             "exec": {"program": "node", "args": ["evaluate", "{{inputs.url}}"]}},
            {"id": "fix", "needs": ["evaluate"],
             "when": {"step": "evaluate", "output": "/verdict", "equals": "findings"},
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

/// One scripted scenario: a definition, its inputs, and what its step jobs do.
struct Scenario {
    definition: WorkflowDefinition,
    inputs: BTreeMap<String, Value>,
    world: fn(&StepSubmission) -> StepResult,
}

fn proof_scenario() -> Scenario {
    Scenario {
        definition: proof_definition(),
        inputs: BTreeMap::from([("url".to_string(), json!("https://example.test"))]),
        world,
    }
}

/// A cook-batch shaped fan-out: prepare emits three children, the Cook
/// fan-out runs two at a time and needs two successes, the always-on report
/// follows. Child 1 fails its first attempt and is retried once.
fn fan_out_scenario() -> Scenario {
    fn fan_world(submission: &StepSubmission) -> StepResult {
        let output = (submission.step == "prepare").then(|| {
            json!({"children": [
                {"issue_url": "https://example.test/issues/1"},
                {"issue_url": "https://example.test/issues/2"},
                {"issue_url": "https://example.test/issues/3"},
            ]})
        });
        let failed = submission.key.contains("/item-1/1") || submission.key.contains("/item-2/");
        let run_id = match &submission.kind {
            SubmissionKind::Cook { cook_id, .. } => Some(cook_id.clone()),
            _ => None,
        };
        StepResult {
            error: None,
            key: submission.key.clone(),
            status: if failed {
                FinishedStatus::Failed
            } else {
                FinishedStatus::Succeeded
            },
            outcome: run_id
                .as_ref()
                .map(|_| if failed { "failed" } else { "succeeded" }.to_string()),
            output,
            run_id,
        }
    }
    Scenario {
        definition: parse_definition(&json!({
            "schema": "homeboy/workflow/v1",
            "id": "batch",
            "inputs": {"issues": {"required": true}},
            "steps": [
                {"id": "prepare", "homeboy": ["agent-task", "batch-prepare", "{{inputs.issues}}"]},
                {"id": "cook", "needs": ["prepare"],
                 "foreach": "{{steps.prepare.output/children}}", "max_parallel": 2,
                 "join": {"min_succeeded": 2}, "retry": {"max": 1, "on": ["failed"]},
                 "cook": {"repo": "scratch", "task-url": "{{item.issue_url}}", "verify": ["true"]}},
                {"id": "report", "needs": ["cook"], "always": true, "exec": {"program": "true"}}
            ]
        })),
        inputs: BTreeMap::from([("issues".to_string(), json!("1 2 3"))]),
        world: fan_world,
    }
}

/// Drive the scenario to completion. `crash` names the transaction (by its
/// 1-based order) at which the driver dies once.
fn drive(scenario: &Scenario, crash: Option<(usize, Crash)>) -> Ledger {
    let definition = &scenario.definition;
    let world = scenario.world;
    let inputs = scenario.inputs.clone();
    let mut ledger = Ledger::default();
    let mut transaction = 0;
    let mut crash = crash;
    let mut now_ms = 0;
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
                    error: None,
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
    let scenario = proof_scenario();
    let baseline = drive(&scenario, None);
    let final_state = baseline.checkpoint.clone().unwrap();
    assert_eq!(final_state.status, InstanceStatus::Succeeded);
    assert_eq!(final_state.iteration, 1);
    assert_eq!(
        final_state.steps["capture"].outcome.as_deref(),
        Some("succeeded")
    );
    let keys = baseline.inserts.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "wf-golden/capture/1/1",
            "wf-golden/evaluate/1/1",
            "wf-golden/fix/1/1",
            "wf-golden/fix/1/2",
        ]
    );
    assert_crash_recovery_converges(&scenario, &baseline);
}

#[test]
fn fan_out_crash_at_every_command_boundary_converges_without_running_an_item_twice() {
    let scenario = fan_out_scenario();
    let baseline = drive(&scenario, None);
    let final_state = baseline.checkpoint.clone().unwrap();
    assert_eq!(final_state.status, InstanceStatus::Succeeded);
    let cook = &final_state.steps["cook"];
    assert_eq!(cook.outcome.as_deref(), Some("succeeded"));
    assert_eq!(cook.output.as_ref().unwrap()["succeeded"], 2);
    assert_eq!(
        cook.items
            .iter()
            .map(|item| (item.outcome.as_deref(), item.attempt))
            .collect::<Vec<_>>(),
        vec![
            (Some("succeeded"), 1),
            (Some("succeeded"), 2),
            (Some("failed"), 2)
        ]
    );
    assert_eq!(
        baseline.inserts.keys().cloned().collect::<Vec<_>>(),
        vec![
            "wf-golden/cook/1/item-0/1",
            "wf-golden/cook/1/item-1/1",
            "wf-golden/cook/1/item-1/2",
            "wf-golden/cook/1/item-2/1",
            "wf-golden/cook/1/item-2/2",
            "wf-golden/prepare/1/1",
            "wf-golden/report/1/1",
        ]
    );
    assert_crash_recovery_converges(&scenario, &baseline);
}

/// Crash before and after every commit of an uninterrupted run: recovery
/// converges on the same final state, inserts every step key exactly once,
/// and runs each step job's work exactly once.
fn assert_crash_recovery_converges(scenario: &Scenario, baseline: &Ledger) {
    let final_state = baseline.checkpoint.clone().unwrap();
    let boundaries = count_transactions(scenario);
    assert!(
        boundaries > 5,
        "expected many commit boundaries, got {boundaries}"
    );
    for at in 1..=boundaries {
        for kind in [Crash::BeforeCommit, Crash::AfterCommit] {
            let ledger = drive(scenario, Some((at, kind)));
            let state = ledger.checkpoint.clone().unwrap();
            assert_eq!(
                projection(&state),
                projection(&final_state),
                "crash {kind:?} at transaction {at} diverged"
            );
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
fn count_transactions(scenario: &Scenario) -> usize {
    let definition = &scenario.definition;
    let world = scenario.world;
    let mut state = InstanceState::new(definition, INSTANCE, scenario.inputs.clone());
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
    let scenario = proof_scenario();
    let definition = &scenario.definition;
    let mut state = drive(&scenario, None).checkpoint.unwrap();
    state.status = InstanceStatus::Cancelled;
    for event in [
        Event::Start,
        Event::Tick,
        Event::Approved {
            step: "approve".into(),
            iteration: 1,
        },
        Event::StepFinished(StepResult {
            error: None,
            key: "wf-golden/fix/1/2".into(),
            status: FinishedStatus::Succeeded,
            outcome: None,
            output: None,
            run_id: None,
        }),
        Event::Cancel { reason: None },
    ] {
        let (next, commands) = advance(definition, &state, &event, 99_999);
        assert_eq!(next, state);
        assert!(commands.is_empty());
    }
}
