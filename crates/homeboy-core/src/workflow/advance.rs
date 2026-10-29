//! The pure workflow state machine.
//!
//! `advance(definition, state, event, now) -> (state', commands)` is the only
//! place a workflow decides anything. It reads no clock, store, or process:
//! time arrives as `now_ms` and every fact about a step arrives as an event.
//! The driver commits the new state and the commands' effects in one store
//! transaction, so a crash either loses a whole decision or none of it, and
//! replaying the same events against a committed state is a no-op.
//!
//! Step submissions carry deterministic keys,
//! `<instance>/<step>/<iteration>/<attempt>` for runs and
//! `<instance>/<step>/<iteration>/item-<n>/<attempt>` for `foreach` items, so
//! a replayed submission converges on the same job. A step's outcome and
//! output are the result of its run in this instance; a definition that wants
//! another pass makes its final step re-admit the definition (see the
//! `workflow` command docs).
//!
//! A `foreach` step expands its items once its `needs` and `when` pass, runs
//! at most `max_parallel` of them at a time, and settles when every item has
//! settled: `succeeded` when its `join` passes, otherwise `partial_failure`
//! (some items succeeded) or `failed` (none did). Items are independent work,
//! so one item's failure neither cancels nor skips its siblings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::definition::{Join, StepDefinition, StepKind, WorkflowDefinition};
use super::gate::GateFacts;
use super::template::{self, TemplateValues};

pub const WORKFLOW_STATE_SCHEMA: &str = "homeboy/workflow-state/v1";

/// Step outcomes that fail the instance.
pub const FAILURE_OUTCOMES: &[&str] = &["failed", "timed_out", "cancelled", "partial_failure"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    BudgetExhausted,
}

impl InstanceStatus {
    pub fn is_terminal(self) -> bool {
        self != Self::Running
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    Pending,
    AwaitingApproval,
    Running,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepState {
    pub phase: StepPhase,
    /// Attempts submitted for the step.
    #[serde(default)]
    pub attempt: u32,
    /// The current (or last) submitted step key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// When the current attempt may start; its timeout counts from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    /// The outcome of the step's run in this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A `foreach` step's items, once expanded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemState>,
}

/// One item of a `foreach` step: its own attempts, key, and outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemState {
    pub item: Value,
    pub phase: StepPhase,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ItemState {
    fn pending(item: Value) -> Self {
        Self {
            item,
            phase: StepPhase::Pending,
            attempt: 0,
            key: None,
            started_at_ms: None,
            timed_out: false,
            outcome: None,
            output: None,
            run_id: None,
            error: None,
        }
    }
}

impl StepState {
    fn pending() -> Self {
        Self {
            phase: StepPhase::Pending,
            attempt: 0,
            key: None,
            started_at_ms: None,
            timed_out: false,
            outcome: None,
            output: None,
            run_id: None,
            prompt: None,
            error: None,
            items: Vec::new(),
        }
    }

    fn settled(&self) -> bool {
        self.phase == StepPhase::Done
    }

    fn succeeded(&self) -> bool {
        self.settled() && self.outcome.as_deref() == Some("succeeded")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstanceState {
    pub schema: String,
    pub instance: String,
    pub inputs: BTreeMap<String, Value>,
    pub status: InstanceStatus,
    /// The pass of the step graph this instance runs. One instance runs one
    /// pass; a definition that wants another pass re-admits itself, which
    /// chains a new instance.
    pub iteration: u32,
    /// Step jobs submitted so far.
    pub step_runs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    pub steps: BTreeMap<String, StepState>,
    /// A step failed: no new step starts unless it is `always`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl InstanceState {
    pub fn new(
        definition: &WorkflowDefinition,
        instance: impl Into<String>,
        inputs: BTreeMap<String, Value>,
    ) -> Self {
        Self {
            schema: WORKFLOW_STATE_SCHEMA.to_string(),
            instance: instance.into(),
            inputs,
            status: InstanceStatus::Running,
            iteration: 1,
            step_runs: 0,
            started_at_ms: None,
            steps: definition
                .steps
                .iter()
                .map(|step| (step.id.clone(), StepState::pending()))
                .collect(),
            failing: false,
            reason: None,
        }
    }

    /// Keys of submitted step jobs whose result this state still waits for.
    pub fn awaited_keys(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for step in self.steps.values() {
            if step.phase == StepPhase::Running {
                keys.extend(step.key.clone());
            }
            keys.extend(
                step.items
                    .iter()
                    .filter(|item| item.phase == StepPhase::Running)
                    .filter_map(|item| item.key.clone()),
            );
        }
        keys
    }

    /// The step (and `foreach` item index) a submitted step job belongs to.
    pub fn owner_of(&self, key: &str) -> Option<(&str, Option<usize>)> {
        self.steps.iter().find_map(|(id, step)| {
            if step.key.as_deref() == Some(key) {
                return Some((id.as_str(), None));
            }
            step.items
                .iter()
                .position(|item| item.key.as_deref() == Some(key))
                .map(|index| (id.as_str(), Some(index)))
        })
    }
}

/// How a step job ended, as the job ledger reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishedStatus {
    Succeeded,
    Failed,
    Cancelled,
}

impl FinishedStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub status: FinishedStatus,
    /// A domain outcome that overrides `status` (a Cook's run state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The instance's first advance.
    Start,
    /// Time passed.
    Tick,
    StepFinished(StepResult),
    Approved {
        step: String,
        iteration: u32,
    },
    /// The operator cancelled the instance: stop every step job it awaits.
    Cancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubmissionKind {
    Exec {
        program: String,
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Homeboy {
        args: Vec<String>,
    },
    Cook {
        args: Vec<String>,
        cook_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepSubmission {
    pub key: String,
    pub step: String,
    pub iteration: u32,
    pub attempt: u32,
    #[serde(flatten)]
    pub kind: SubmissionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    SubmitStep(StepSubmission),
    CancelStep {
        key: String,
        reason: String,
    },
    Complete {
        status: InstanceStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Decide the next state and the effects that must commit with it.
pub fn advance(
    definition: &WorkflowDefinition,
    state: &InstanceState,
    event: &Event,
    now_ms: u64,
) -> (InstanceState, Vec<Command>) {
    let mut machine = Machine {
        definition,
        state: state.clone(),
        commands: Vec::new(),
        now_ms,
    };
    if machine.state.status.is_terminal() {
        return (machine.state, Vec::new());
    }
    match event {
        Event::Start => {
            machine.state.started_at_ms.get_or_insert(now_ms);
        }
        Event::Tick => {}
        Event::StepFinished(result) => machine.finish(result),
        Event::Approved { step, iteration } => machine.approve(step, *iteration),
        Event::Cancel { reason } => {
            machine.cancel(reason.as_deref().unwrap_or("the workflow was cancelled"));
        }
    }
    if machine.state.started_at_ms.is_some() && !machine.state.status.is_terminal() {
        machine.enforce_deadline();
        machine.enforce_timeouts();
        machine.progress();
    }
    (machine.state, machine.commands)
}

struct Machine<'a> {
    definition: &'a WorkflowDefinition,
    state: InstanceState,
    commands: Vec<Command>,
    now_ms: u64,
}

impl GateFacts for InstanceState {
    fn outcome(&self, step: &str) -> Option<&str> {
        self.steps.get(step)?.outcome.as_deref()
    }

    fn output(&self, step: &str) -> Option<&Value> {
        self.steps.get(step)?.output.as_ref()
    }
}

impl TemplateValues for InstanceState {
    fn input(&self, name: &str) -> Option<&Value> {
        self.inputs.get(name)
    }

    fn step_output(&self, step: &str) -> Option<&Value> {
        self.steps.get(step)?.output.as_ref()
    }

    fn step_run_id(&self, step: &str) -> Option<&str> {
        self.steps.get(step)?.run_id.as_deref()
    }

    fn instance(&self) -> &str {
        &self.instance
    }
}

impl Machine<'_> {
    fn step_definition(&self, id: &str) -> &StepDefinition {
        self.definition
            .steps
            .iter()
            .find(|step| step.id == id)
            .expect("state steps match the pinned definition")
    }

    fn step(&mut self, id: &str) -> &mut StepState {
        self.state
            .steps
            .get_mut(id)
            .expect("state steps match the pinned definition")
    }

    fn finish(&mut self, result: &StepResult) {
        if let Some((id, index)) = self.state.steps.iter().find_map(|(id, step)| {
            step.items
                .iter()
                .position(|item| {
                    item.phase == StepPhase::Running
                        && item.key.as_deref() == Some(result.key.as_str())
                })
                .map(|index| (id.clone(), index))
        }) {
            self.finish_item(&id, index, result);
            return;
        }
        let Some(id) = self
            .state
            .steps
            .iter()
            .find(|(_, step)| {
                step.phase == StepPhase::Running && step.key.as_deref() == Some(result.key.as_str())
            })
            .map(|(id, _)| id.clone())
        else {
            // Not awaited: a replayed or superseded result.
            return;
        };
        let step = self.step(&id);
        let outcome = if step.timed_out && result.status == FinishedStatus::Cancelled {
            "timed_out".to_string()
        } else {
            result
                .outcome
                .clone()
                .unwrap_or_else(|| result.status.as_str().to_string())
        };
        step.error = result.error.clone();
        step.output = result.output.clone();
        step.run_id = result.run_id.clone();
        let attempt = step.attempt;
        let retry = self.step_definition(&id).retry.clone();
        if let Some(retry) = retry {
            if attempt <= retry.max && retry.on.contains(&outcome) {
                let backoff = retry
                    .backoff
                    .as_deref()
                    .and_then(|raw| crate::duration::parse_duration_parts(raw).ok())
                    .map(|duration| duration.as_millis() as u64);
                // The finished attempt is no longer awaited, even if the
                // budget refuses the retry.
                let step = self.step(&id);
                step.phase = StepPhase::Pending;
                step.outcome = Some(outcome);
                self.launch(&id, backoff.map(|backoff| self.now_ms + backoff));
                return;
            }
        }
        self.settle(&id, outcome);
    }

    fn approve(&mut self, id: &str, iteration: u32) {
        if iteration != self.state.iteration {
            return;
        }
        if self
            .state
            .steps
            .get(id)
            .is_some_and(|step| step.phase == StepPhase::AwaitingApproval)
        {
            self.settle(id, "succeeded".to_string());
        }
    }

    fn finish_item(&mut self, id: &str, index: usize, result: &StepResult) {
        let retry = self.step_definition(id).retry.clone();
        let item = &mut self.step(id).items[index];
        let outcome = if item.timed_out && result.status == FinishedStatus::Cancelled {
            "timed_out".to_string()
        } else {
            result
                .outcome
                .clone()
                .unwrap_or_else(|| result.status.as_str().to_string())
        };
        item.error = result.error.clone();
        item.output = result.output.clone();
        item.run_id = result.run_id.clone();
        item.outcome = Some(outcome.clone());
        if let Some(retry) = retry {
            if item.attempt <= retry.max && retry.on.contains(&outcome) {
                item.phase = StepPhase::Pending;
                let backoff = retry
                    .backoff
                    .as_deref()
                    .and_then(|raw| crate::duration::parse_duration_parts(raw).ok())
                    .map(|duration| duration.as_millis() as u64);
                self.launch_item(id, index, backoff.map(|backoff| self.now_ms + backoff));
                return;
            }
        }
        self.step(id).items[index].phase = StepPhase::Done;
    }

    /// Cancel every awaited step job and end the instance as cancelled.
    fn cancel(&mut self, reason: &str) {
        for key in self.state.awaited_keys() {
            self.commands.push(Command::CancelStep {
                key,
                reason: reason.to_string(),
            });
        }
        self.complete(InstanceStatus::Cancelled, Some(reason.to_string()));
    }

    /// Expand a `foreach` step into its items and start the first wave.
    fn expand(&mut self, id: &str) {
        let definition = self.step_definition(id).clone();
        let items = match definition.foreach.as_ref().expect("foreach step") {
            Value::Array(items) => Ok(items.clone()),
            Value::String(text) => template::resolve_value(text, &self.state).and_then(|value| {
                match value {
                    Value::Array(items) => Ok(items),
                    // A template that rendered JSON text is still an array.
                    Value::String(text) => match serde_json::from_str(&text) {
                        Ok(Value::Array(items)) => Ok(items),
                        _ => Err(format!("`foreach` resolved to `{text}`, not an array")),
                    },
                    other => Err(format!("`foreach` resolved to `{other}`, not an array")),
                }
            }),
            other => Err(format!("`foreach` is `{other}`, not an array")),
        };
        match items {
            Err(error) => {
                self.step(id).error = Some(error);
                self.settle(id, "failed".to_string());
            }
            Ok(items) => {
                let step = self.step(id);
                step.phase = StepPhase::Running;
                step.attempt = 0;
                step.key = None;
                step.output = None;
                step.run_id = None;
                step.items = items.into_iter().map(ItemState::pending).collect();
                self.pump(id);
            }
        }
    }

    /// Start pending items of a running `foreach` step up to `max_parallel`,
    /// then settle the step through its `join` once every item has settled.
    fn pump(&mut self, id: &str) {
        let definition = self.step_definition(id).clone();
        let limit = definition
            .max_parallel
            .map_or(usize::MAX, |max| max as usize);
        loop {
            if self.state.status.is_terminal() {
                return;
            }
            let step = &self.state.steps[id];
            let running = step
                .items
                .iter()
                .filter(|item| item.phase == StepPhase::Running)
                .count();
            let next = step
                .items
                .iter()
                .position(|item| item.phase == StepPhase::Pending && item.attempt == 0);
            match next {
                Some(index) if running < limit => self.launch_item(id, index, None),
                _ => break,
            }
        }
        let step = &self.state.steps[id];
        if !step.items.iter().all(|item| item.phase == StepPhase::Done) {
            return;
        }
        let total = step.items.len();
        let succeeded = step
            .items
            .iter()
            .filter(|item| item.outcome.as_deref() == Some("succeeded"))
            .count();
        let join = definition.join.unwrap_or(Join::All);
        let outcome = if join.passes(succeeded, total) {
            "succeeded"
        } else if succeeded == 0 {
            "failed"
        } else {
            "partial_failure"
        };
        let output = serde_json::json!({
            "total": total,
            "succeeded": succeeded,
            "join": join,
            "items": step.items.iter().map(|item| serde_json::json!({
                "item": item.item,
                "outcome": item.outcome,
                "run_id": item.run_id,
                "output": item.output,
                "error": item.error,
            })).collect::<Vec<_>>(),
        });
        self.step(id).output = Some(output);
        self.settle(id, outcome.to_string());
    }

    /// Submit the next attempt of one `foreach` item.
    fn launch_item(&mut self, id: &str, index: usize, not_before_ms: Option<u64>) {
        let definition = self.step_definition(id).clone();
        let scope = ItemScope {
            state: &self.state,
            item: &self.state.steps[id].items[index].item,
        };
        let kind = match compile(&definition, &scope) {
            Ok(kind) => kind,
            Err(error) => {
                let item = &mut self.step(id).items[index];
                item.error = Some(error);
                item.outcome = Some("failed".to_string());
                item.phase = StepPhase::Done;
                return;
            }
        };
        if !self.reserve_run() {
            return;
        }
        let iteration = self.state.iteration;
        let instance = self.state.instance.clone();
        let now_ms = self.now_ms;
        let item = &mut self.step(id).items[index];
        item.attempt += 1;
        item.timed_out = false;
        item.phase = StepPhase::Running;
        item.started_at_ms = Some(not_before_ms.unwrap_or(now_ms).max(now_ms));
        let attempt = item.attempt;
        let key = format!("{instance}/{id}/{iteration}/item-{index}/{attempt}");
        item.key = Some(key.clone());
        let kind = match kind {
            SubmissionKind::Cook { args, .. } => {
                let cook_id = format!("{instance}-{id}-i{iteration}-n{index}-a{attempt}");
                item.run_id = Some(cook_id.clone());
                SubmissionKind::Cook { args, cook_id }
            }
            other => other,
        };
        self.commands.push(Command::SubmitStep(StepSubmission {
            key,
            step: id.to_string(),
            iteration,
            attempt,
            kind,
            not_before_ms,
        }));
    }

    fn settle(&mut self, id: &str, outcome: String) {
        let failure = FAILURE_OUTCOMES.contains(&outcome.as_str());
        let step = self.step(id);
        step.phase = StepPhase::Done;
        step.outcome = Some(outcome);
        if failure {
            let error = step.error.clone().or_else(|| {
                step.items
                    .iter()
                    .find(|item| {
                        item.outcome
                            .as_deref()
                            .is_some_and(|outcome| FAILURE_OUTCOMES.contains(&outcome))
                    })
                    .and_then(|item| item.error.clone())
            });
            self.state.failing = true;
            self.state.reason.get_or_insert_with(|| match error {
                Some(error) => format!("step `{id}` did not succeed: {error}"),
                None => format!("step `{id}` did not succeed"),
            });
        }
    }

    fn skip(&mut self, id: &str) {
        let step = self.step(id);
        step.phase = StepPhase::Done;
        step.outcome = Some("skipped".to_string());
        step.output = None;
        step.run_id = None;
        step.items.clear();
    }

    fn enforce_deadline(&mut self) {
        let Some(deadline) = self
            .definition
            .budget
            .as_ref()
            .and_then(|budget| budget.deadline.as_deref())
            .and_then(|raw| crate::duration::parse_duration_parts(raw).ok())
        else {
            return;
        };
        let started = self.state.started_at_ms.expect("started instance");
        if self.now_ms >= started + deadline.as_millis() as u64 {
            self.exhaust("the workflow deadline passed");
        }
    }

    fn enforce_timeouts(&mut self) {
        if self.state.status.is_terminal() {
            return;
        }
        let mut cancels = Vec::new();
        for step_definition in &self.definition.steps {
            let Some(timeout) = step_definition.timeout() else {
                continue;
            };
            let step = &self.state.steps[&step_definition.id];
            let expired = |started: Option<u64>| {
                self.now_ms >= started.unwrap_or(self.now_ms) + timeout.as_millis() as u64
            };
            for (index, item) in step.items.iter().enumerate() {
                if item.phase == StepPhase::Running
                    && !item.timed_out
                    && expired(item.started_at_ms)
                {
                    cancels.push((step_definition.id.clone(), Some(index), item.key.clone()));
                }
            }
            if step.phase != StepPhase::Running || step.timed_out || !step.items.is_empty() {
                continue;
            }
            if expired(step.started_at_ms) {
                cancels.push((step_definition.id.clone(), None, step.key.clone()));
            }
        }
        for (id, index, key) in cancels {
            match index {
                Some(index) => self.step(&id).items[index].timed_out = true,
                None => self.step(&id).timed_out = true,
            }
            if let Some(key) = key {
                self.commands.push(Command::CancelStep {
                    key,
                    reason: format!("step `{id}` timed out"),
                });
            }
        }
    }

    fn exhaust(&mut self, reason: &str) {
        for key in self.state.awaited_keys() {
            self.commands.push(Command::CancelStep {
                key,
                reason: format!("workflow budget exhausted: {reason}"),
            });
        }
        self.complete(InstanceStatus::BudgetExhausted, Some(reason.to_string()));
    }

    fn complete(&mut self, status: InstanceStatus, reason: Option<String>) {
        self.state.status = status;
        if reason.is_some() {
            self.state.reason = reason.clone();
        }
        self.commands.push(Command::Complete {
            status,
            reason: self.state.reason.clone(),
        });
    }

    /// Reserve one step run, or exhaust the budget. Returns whether to submit.
    fn reserve_run(&mut self) -> bool {
        let limit = self
            .definition
            .budget
            .as_ref()
            .and_then(|budget| budget.max_step_runs);
        if limit.is_some_and(|limit| self.state.step_runs >= limit) {
            self.exhaust("max_step_runs reached");
            return false;
        }
        self.state.step_runs += 1;
        true
    }

    /// Submit the next attempt of a step.
    fn launch(&mut self, id: &str, not_before_ms: Option<u64>) {
        let definition = self.step_definition(id).clone();
        if definition.foreach.is_some() {
            self.expand(id);
            return;
        }
        let compiled = compile(&definition, &self.state);
        let kind = match compiled {
            Ok(kind) => kind,
            Err(error) => {
                self.step(id).error = Some(error);
                self.settle(id, "failed".to_string());
                return;
            }
        };
        if !self.reserve_run() {
            return;
        }
        let iteration = self.state.iteration;
        let instance = self.state.instance.clone();
        let now_ms = self.now_ms;
        let step = self.step(id);
        step.attempt += 1;
        step.timed_out = false;
        step.phase = StepPhase::Running;
        step.started_at_ms = Some(not_before_ms.unwrap_or(now_ms).max(now_ms));
        let attempt = step.attempt;
        let key = format!("{instance}/{id}/{iteration}/{attempt}");
        step.key = Some(key.clone());
        let kind = match kind {
            SubmissionKind::Cook { args, .. } => {
                let cook_id = format!("{instance}-{id}-i{iteration}-a{attempt}");
                step.run_id = Some(cook_id.clone());
                SubmissionKind::Cook { args, cook_id }
            }
            other => other,
        };
        self.commands.push(Command::SubmitStep(StepSubmission {
            key,
            step: id.to_string(),
            iteration,
            attempt,
            kind,
            not_before_ms,
        }));
    }

    fn progress(&mut self) {
        let fanning_out = self
            .state
            .steps
            .iter()
            .filter(|(_, step)| step.phase == StepPhase::Running && !step.items.is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in fanning_out {
            self.pump(&id);
        }
        self.schedule_ready_steps();
        if self.state.status.is_terminal() || !self.state.steps.values().all(StepState::settled) {
            return;
        }
        if self.state.failing {
            self.complete(InstanceStatus::Failed, None);
        } else {
            self.complete(InstanceStatus::Succeeded, None);
        }
    }

    /// Settle or start every pending step whose `needs` have settled, until
    /// nothing more changes.
    fn schedule_ready_steps(&mut self) {
        loop {
            let mut changed = false;
            for definition in &self.definition.steps {
                if self.state.status.is_terminal() {
                    return;
                }
                let id = definition.id.as_str();
                if self.state.steps[id].phase != StepPhase::Pending {
                    continue;
                }
                let needs = definition
                    .needs
                    .iter()
                    .map(|need| &self.state.steps[need])
                    .collect::<Vec<_>>();
                if !needs.iter().all(|need| need.settled()) {
                    continue;
                }
                let needs_succeeded = needs.iter().all(|need| need.succeeded());
                if (!needs_succeeded || self.state.failing) && !definition.always {
                    self.skip(id);
                    changed = true;
                    continue;
                }
                if let Some(when) = &definition.when {
                    if !when.evaluate(&self.state) {
                        self.skip(id);
                        changed = true;
                        continue;
                    }
                }
                if let StepKind::Approval(approval) = definition.kind() {
                    let prompt = template::render(&approval.prompt, &self.state)
                        .unwrap_or_else(|_| approval.prompt.clone());
                    let step = self.step(id);
                    step.phase = StepPhase::AwaitingApproval;
                    step.prompt = Some(prompt);
                } else {
                    self.launch(id, None);
                }
                changed = true;
            }
            if !changed {
                return;
            }
        }
    }
}

/// Template values inside one `foreach` item: the instance plus `{{item}}`.
struct ItemScope<'a> {
    state: &'a InstanceState,
    item: &'a Value,
}

impl TemplateValues for ItemScope<'_> {
    fn input(&self, name: &str) -> Option<&Value> {
        self.state.input(name)
    }

    fn step_output(&self, step: &str) -> Option<&Value> {
        self.state.step_output(step)
    }

    fn step_run_id(&self, step: &str) -> Option<&str> {
        self.state.step_run_id(step)
    }

    fn instance(&self) -> &str {
        &self.state.instance
    }

    fn item(&self) -> Option<&Value> {
        Some(self.item)
    }
}

/// Render a step into the work it submits.
fn compile(
    definition: &StepDefinition,
    values: &dyn TemplateValues,
) -> Result<SubmissionKind, String> {
    let render = |text: &str| template::render(text, values);
    Ok(match definition.kind() {
        StepKind::Exec(exec) => SubmissionKind::Exec {
            program: exec.program.clone(),
            args: exec
                .args
                .iter()
                .map(|arg| render(arg))
                .collect::<Result<_, _>>()?,
            cwd: exec.cwd.as_deref().map(render).transpose()?,
        },
        StepKind::Homeboy(argv) => SubmissionKind::Homeboy {
            args: argv
                .iter()
                .map(|arg| render(arg))
                .collect::<Result<_, _>>()?,
        },
        StepKind::Cook(cook) => {
            let mut args = Vec::new();
            for (key, value) in cook {
                let flag = format!("--{key}");
                match value {
                    Value::Bool(true) => args.push(flag),
                    Value::Bool(false) | Value::Null => {}
                    Value::String(text) => {
                        args.push(flag);
                        args.push(render(text)?);
                    }
                    Value::Number(number) => {
                        args.push(flag);
                        args.push(number.to_string());
                    }
                    Value::Array(values) => {
                        for value in values {
                            args.push(flag.clone());
                            args.push(render(value.as_str().unwrap_or_default())?);
                        }
                    }
                    Value::Object(_) => return Err(format!("cook `{key}` cannot be an object")),
                }
            }
            SubmissionKind::Cook {
                args,
                cook_id: String::new(),
            }
        }
        StepKind::Approval(_) => unreachable!("approval steps never submit work"),
    })
}

#[cfg(test)]
#[path = "advance_tests.rs"]
mod tests;
