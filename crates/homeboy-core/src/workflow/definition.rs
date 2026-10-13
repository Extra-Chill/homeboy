//! The `homeboy/workflow/v1` definition format and its validation.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::gate::Gate;
use super::template::{self, Reference};
use crate::error::{Error, Result};

pub const WORKFLOW_SCHEMA: &str = "homeboy/workflow/v1";

/// Step outcomes a `retry.on` list may name.
pub const RETRYABLE_OUTCOMES: &[&str] = &["failed", "timed_out"];

/// `cook` keys the workflow owns. A Cook step always waits, is named by its
/// step key, and reports through the step's own output file.
const RESERVED_COOK_KEYS: &[&str] = &["wait", "cook-id", "run-id", "output", "preview"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub schema: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputSpec>,
    #[serde(default, skip_serializing_if = "EnvSpec::is_empty")]
    pub env: EnvSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterate: Option<Iterate>,
    /// Optional cadence: `workflow run` registers the definition with the
    /// daemon, which admits one instance per due slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<Every>,
    pub steps: Vec<StepDefinition>,
}

/// The shortest cadence: the daemon admits due cadences once per
/// orchestration tick, so a shorter interval could not be honoured.
pub const MIN_EVERY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Every {
    /// Time between due slots, for example `1h` or `24h`.
    pub interval: String,
    /// UTC time of day (`HH:MM` or `HH:MMZ`) the slot grid is anchored to.
    /// Without it the grid starts when the cadence is registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// A slot that comes due while the previous instance is still running is
    /// skipped. This is the only overlap policy.
    #[serde(default)]
    pub overlap: Overlap,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    #[default]
    Skip,
}

impl Every {
    pub fn interval(&self) -> Result<Duration> {
        let interval = crate::duration::parse_duration("every.interval", &self.interval)?;
        if interval < MIN_EVERY_INTERVAL {
            return Err(invalid("every.interval", "a cadence is at least 1m"));
        }
        Ok(interval)
    }

    /// Offset of `at` from UTC midnight.
    pub fn at_offset(&self) -> Result<Option<Duration>> {
        let Some(at) = &self.at else {
            return Ok(None);
        };
        let clock = at.strip_suffix('Z').unwrap_or(at);
        let parsed = clock
            .split_once(':')
            .filter(|(hours, minutes)| hours.len() == 2 && minutes.len() == 2)
            .and_then(|(hours, minutes)| {
                Some((hours.parse::<u64>().ok()?, minutes.parse::<u64>().ok()?))
            })
            .filter(|(hours, minutes)| *hours < 24 && *minutes < 60);
        let (hours, minutes) = parsed
            .ok_or_else(|| invalid("every.at", format!("`{at}` is not a UTC time like 03:00Z")))?;
        Ok(Some(Duration::from_secs(hours * 3600 + minutes * 60)))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// The admitted environment: nothing else is inherited by a step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvSpec {
    /// Names whose values are captured from the admitting environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public: Vec<String>,
    /// Agent-task secret references, resolved when each step spawns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
}

impl EnvSpec {
    fn is_empty(&self) -> bool {
        self.public.is_empty() && self.secrets.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_step_runs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Iterate {
    pub until: Gate,
    pub max_iterations: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDefinition {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Gate>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub always: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Retry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cook: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homeboy: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<ExecStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub max: u32,
    #[serde(default = "default_retry_on")]
    pub on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backoff: Option<String>,
}

fn default_retry_on() -> Vec<String> {
    RETRYABLE_OUTCOMES.iter().map(|s| s.to_string()).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecStep {
    pub program: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalStep {
    pub prompt: String,
}

/// What a step runs. Exactly one per step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepKind<'a> {
    Cook(&'a Map<String, Value>),
    Homeboy(&'a [String]),
    Exec(&'a ExecStep),
    Approval(&'a ApprovalStep),
}

impl StepDefinition {
    pub fn kind(&self) -> StepKind<'_> {
        match (&self.cook, &self.homeboy, &self.exec, &self.approval) {
            (Some(cook), None, None, None) => StepKind::Cook(cook),
            (None, Some(argv), None, None) => StepKind::Homeboy(argv),
            (None, None, Some(exec), None) => StepKind::Exec(exec),
            (None, None, None, Some(approval)) => StepKind::Approval(approval),
            _ => unreachable!("validated step kind"),
        }
    }

    fn kind_count(&self) -> usize {
        [
            self.cook.is_some(),
            self.homeboy.is_some(),
            self.exec.is_some(),
            self.approval.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
            .as_deref()
            .and_then(|raw| crate::duration::parse_duration_parts(raw).ok())
    }

    /// Every templated string the step carries, in a stable order.
    fn templated_strings(&self) -> Vec<&str> {
        let mut strings = Vec::new();
        match self.kind() {
            StepKind::Cook(cook) => {
                for value in cook.values() {
                    collect_strings(value, &mut strings);
                }
            }
            StepKind::Homeboy(argv) => strings.extend(argv.iter().map(String::as_str)),
            StepKind::Exec(exec) => {
                strings.extend(exec.args.iter().map(String::as_str));
                if let Some(cwd) = &exec.cwd {
                    strings.push(cwd);
                }
            }
            StepKind::Approval(approval) => strings.push(&approval.prompt),
        }
        if let Some(gate) = &self.when {
            for argv in gate.command_gates() {
                strings.extend(argv.iter().map(String::as_str));
            }
        }
        strings
    }
}

fn collect_strings<'a>(value: &'a Value, strings: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => strings.push(text),
        Value::Array(values) => {
            for value in values {
                collect_strings(value, strings);
            }
        }
        _ => {}
    }
}

/// Features of the workflow format that a later Homeboy release implements.
const LATER_TOP_LEVEL: &[(&str, &str)] = &[("notify", "`notify`")];
const LATER_STEP: &[(&str, &str)] = &[
    ("foreach", "`foreach` fan-out"),
    ("max_parallel", "`max_parallel`"),
    ("join", "`join`"),
];

impl WorkflowDefinition {
    /// Parse a definition without validating it.
    pub fn parse(text: &str) -> Result<Self> {
        let raw: Value = serde_json::from_str(text).map_err(|error| {
            invalid("definition", format!("workflow is not valid JSON: {error}"))
        })?;
        Self::from_value(raw)
    }

    pub fn from_value(raw: Value) -> Result<Self> {
        for (field, label) in LATER_TOP_LEVEL {
            if raw.get(field).is_some() {
                return Err(not_yet(label));
            }
        }
        if let Some(steps) = raw.get("steps").and_then(Value::as_array) {
            for step in steps {
                for (field, label) in LATER_STEP {
                    if step.get(field).is_some() {
                        return Err(not_yet(label));
                    }
                }
            }
        }
        serde_path_to_error::deserialize(raw)
            .map_err(|error| invalid("definition", format!("invalid workflow: {error}")))
    }

    /// Validate structure, the `needs` graph, every gate and template, and
    /// durations. `cook_flag` answers whether a `cook` key names a real
    /// `agent-task cook` flag; the CLI supplies its clap tree.
    pub fn validate(&self, cook_flag: &dyn Fn(&str) -> bool) -> Result<()> {
        if self.schema != WORKFLOW_SCHEMA {
            return Err(invalid(
                "schema",
                format!("workflow schema must be `{WORKFLOW_SCHEMA}`"),
            ));
        }
        if !valid_workflow_id(&self.id) {
            return Err(invalid(
                "id",
                "workflow id must be non-empty letters, digits, `_`, `-` or `.`",
            ));
        }
        if let Some(every) = &self.every {
            every.interval()?;
            every.at_offset()?;
        }
        for name in self.inputs.keys() {
            if !template::valid_name(name) {
                return Err(invalid("inputs", format!("invalid input name `{name}`")));
            }
        }
        let mut env_names = BTreeSet::new();
        let policy = crate::redaction::RedactionPolicy::default();
        for name in self.env.public.iter().chain(&self.env.secrets) {
            if name.is_empty() || name.contains('=') || name.contains('\0') {
                return Err(invalid("env", format!("invalid environment name `{name}`")));
            }
            if !env_names.insert(name.as_str()) {
                return Err(invalid(
                    "env",
                    format!("environment name `{name}` is declared twice"),
                ));
            }
        }
        for name in &self.env.public {
            if policy.is_credential_key_name(name) {
                return Err(invalid(
                    "env.public",
                    format!("`{name}` is a credential; declare it under `env.secrets`"),
                ));
            }
        }
        if let Some(budget) = &self.budget {
            if budget.max_step_runs == Some(0) {
                return Err(invalid("budget.max_step_runs", "must be at least 1"));
            }
            if let Some(deadline) = &budget.deadline {
                crate::duration::parse_duration("budget.deadline", deadline)?;
            }
        }
        if self.steps.is_empty() {
            return Err(invalid("steps", "a workflow needs at least one step"));
        }

        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if !template::valid_name(&step.id) {
                return Err(invalid(
                    "steps.id",
                    format!("invalid step id `{}`", step.id),
                ));
            }
            if !ids.insert(step.id.as_str()) {
                return Err(invalid(
                    "steps.id",
                    format!("step id `{}` is declared twice", step.id),
                ));
            }
        }
        for step in &self.steps {
            let field = format!("steps.{}", step.id);
            if step.kind_count() != 1 {
                return Err(invalid(
                    &field,
                    "a step runs exactly one of `cook`, `homeboy`, `exec`, or `approval`",
                ));
            }
            for need in &step.needs {
                if !ids.contains(need.as_str()) {
                    return Err(invalid(&field, format!("needs unknown step `{need}`")));
                }
            }
            if let Some(timeout) = &step.timeout {
                crate::duration::parse_duration(&format!("{field}.timeout"), timeout)?;
            }
            if let Some(retry) = &step.retry {
                for outcome in &retry.on {
                    if !RETRYABLE_OUTCOMES.contains(&outcome.as_str()) {
                        return Err(invalid(
                            &format!("{field}.retry.on"),
                            format!(
                                "`{outcome}` cannot be retried; use {}",
                                RETRYABLE_OUTCOMES.join(" or ")
                            ),
                        ));
                    }
                }
                if let Some(backoff) = &retry.backoff {
                    crate::duration::parse_duration(&format!("{field}.retry.backoff"), backoff)?;
                }
            }
            match step.kind() {
                StepKind::Homeboy([]) => {
                    return Err(invalid(&field, "`homeboy` needs at least one argument"));
                }
                StepKind::Exec(exec) if exec.program.trim().is_empty() => {
                    return Err(invalid(&field, "`exec.program` is required"));
                }
                StepKind::Exec(exec) if exec.program.contains("{{") => {
                    return Err(invalid(&field, "`exec.program` cannot be templated"));
                }
                StepKind::Cook(cook) => {
                    for (key, value) in cook {
                        if RESERVED_COOK_KEYS.contains(&key.as_str()) {
                            return Err(invalid(
                                &format!("{field}.cook"),
                                format!("`{key}` is owned by the workflow step"),
                            ));
                        }
                        if !cook_flag(key) {
                            return Err(invalid(
                                &format!("{field}.cook"),
                                format!("`{key}` is not an `agent-task cook` flag"),
                            ));
                        }
                        if !cook_value_is_valid(value) {
                            return Err(invalid(
                                &format!("{field}.cook.{key}"),
                                "values are strings, numbers, booleans, or arrays of strings",
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        self.validate_acyclic()?;

        for step in &self.steps {
            let field = format!("steps.{}", step.id);
            let upstream = self.upstream(&step.id);
            for text in step.templated_strings() {
                for reference in template::references(text).map_err(|e| invalid(&field, e))? {
                    self.check_reference(&reference, &upstream, &field)?;
                }
            }
            if let Some(gate) = &step.when {
                for referenced in gate.referenced_steps() {
                    // A step may gate on its own carried result.
                    if referenced != step.id && !upstream.contains(referenced) {
                        return Err(invalid(
                            &format!("{field}.when"),
                            format!(
                                "gate reads `{referenced}`, which is not upstream of this step"
                            ),
                        ));
                    }
                }
            }
        }
        if let Some(iterate) = &self.iterate {
            if iterate.max_iterations == 0 {
                return Err(invalid("iterate.max_iterations", "must be at least 1"));
            }
            for referenced in iterate.until.referenced_steps() {
                if !ids.contains(referenced) {
                    return Err(invalid(
                        "iterate.until",
                        format!("gate reads unknown step `{referenced}`"),
                    ));
                }
            }
            let all = ids.iter().map(|id| id.to_string()).collect::<BTreeSet<_>>();
            for argv in iterate.until.command_gates() {
                for text in argv {
                    for reference in
                        template::references(text).map_err(|e| invalid("iterate.until", e))?
                    {
                        self.check_reference(&reference, &all, "iterate.until")?;
                    }
                }
            }
        }
        Ok(())
    }

    fn check_reference(
        &self,
        reference: &Reference,
        upstream: &BTreeSet<String>,
        field: &str,
    ) -> Result<()> {
        match reference {
            Reference::Input(name) if !self.inputs.contains_key(name) => Err(invalid(
                field,
                format!("template reads undeclared input `{name}`"),
            )),
            Reference::StepOutput { step, .. } | Reference::StepRunId(step)
                if !upstream.contains(step) =>
            {
                Err(invalid(
                    field,
                    format!("template reads step `{step}`, which is not upstream (declare it in `needs`)"),
                ))
            }
            _ => Ok(()),
        }
    }

    fn step(&self, id: &str) -> Option<&StepDefinition> {
        self.steps.iter().find(|step| step.id == id)
    }

    /// Every step `id` transitively needs.
    pub fn upstream(&self, id: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = self
            .step(id)
            .map(|step| step.needs.clone())
            .unwrap_or_default();
        while let Some(next) = stack.pop() {
            if seen.insert(next.clone()) {
                if let Some(step) = self.step(&next) {
                    stack.extend(step.needs.iter().cloned());
                }
            }
        }
        seen
    }

    fn validate_acyclic(&self) -> Result<()> {
        // Kahn's algorithm over `needs`.
        let mut remaining: BTreeMap<&str, BTreeSet<&str>> = self
            .steps
            .iter()
            .map(|step| {
                (
                    step.id.as_str(),
                    step.needs.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        loop {
            let ready = remaining
                .iter()
                .filter(|(_, needs)| needs.is_empty())
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            if ready.is_empty() {
                break;
            }
            for id in ready {
                remaining.remove(id);
                for needs in remaining.values_mut() {
                    needs.remove(id);
                }
            }
        }
        if remaining.is_empty() {
            Ok(())
        } else {
            Err(invalid(
                "steps.needs",
                format!(
                    "the `needs` graph has a cycle through: {}",
                    remaining.keys().copied().collect::<Vec<_>>().join(", ")
                ),
            ))
        }
    }

    /// Canonical SHA-256 of the definition, independent of key order.
    pub fn digest(&self) -> String {
        let value = serde_json::to_value(self).expect("definition serializes");
        homeboy_engine_primitives::content_hash::sha256_hex(canonical_json(&value).as_bytes())
    }

    /// Resolve `--input` values against the declared inputs.
    pub fn resolve_inputs(
        &self,
        provided: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>> {
        if let Some(unknown) = provided
            .keys()
            .find(|name| !self.inputs.contains_key(*name))
        {
            return Err(invalid(
                "input",
                format!("workflow `{}` declares no input `{unknown}`", self.id),
            ));
        }
        let mut resolved = BTreeMap::new();
        for (name, spec) in &self.inputs {
            let value = match (provided.get(name), &spec.default) {
                (Some(value), _) => value.clone(),
                (None, Some(default)) => default.clone(),
                (None, None) if spec.required => {
                    return Err(invalid("input", format!("input `{name}` is required")))
                }
                (None, None) => Value::Null,
            };
            resolved.insert(name.clone(), value);
        }
        Ok(resolved)
    }

    /// `wf-<id>-<sha256(definition digest, inputs, instance name)[..12]>`.
    /// The same definition and inputs name the same instance.
    pub fn instance_id(&self, inputs: &BTreeMap<String, Value>, name: Option<&str>) -> String {
        let identity = serde_json::json!({
            "definition": self.digest(),
            "inputs": inputs,
            "instance": name,
        });
        let hash = homeboy_engine_primitives::content_hash::sha256_hex(
            canonical_json(&identity).as_bytes(),
        );
        format!("wf-{}-{}", self.id, &hash[..12])
    }
}

/// A workflow id also names its registry file and its built-ins
/// (`homeboy.retention`), so it may contain `.`; step and input names may not,
/// because templates use `.` as a separator.
fn valid_workflow_id(id: &str) -> bool {
    !id.starts_with('.') && template::valid_name(&id.replace('.', "_"))
}

fn cook_value_is_valid(value: &Value) -> bool {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => true,
        Value::Array(values) => values.iter().all(Value::is_string),
        _ => false,
    }
}

/// JSON with object keys sorted at every level.
pub fn canonical_json(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let sorted = object
                    .iter()
                    .map(|(key, value)| (key.clone(), sort(value)))
                    .collect::<BTreeMap<_, _>>();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(values) => Value::Array(values.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    sort(value).to_string()
}

fn invalid(field: &str, message: impl Into<String>) -> Error {
    Error::validation_invalid_argument(field, message, None, None)
}

fn not_yet(label: &str) -> Error {
    Error::validation_invalid_argument(
        "definition",
        format!("{label} is not supported by this workflow version yet"),
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn any_flag(key: &str) -> bool {
        [
            "repo",
            "prompt",
            "verify",
            "placement",
            "task-url",
            "backend",
        ]
        .contains(&key)
    }

    fn definition(value: Value) -> Result<WorkflowDefinition> {
        let definition = WorkflowDefinition::from_value(value)?;
        definition.validate(&any_flag)?;
        Ok(definition)
    }

    fn base() -> Value {
        json!({
            "schema": WORKFLOW_SCHEMA,
            "id": "proof",
            "inputs": {"url": {"required": true}},
            "iterate": {"until": {"step": "evaluate", "output": "/verdict", "equals": "accepted"}, "max_iterations": 3},
            "steps": [
                {"id": "evaluate", "exec": {"program": "node", "args": ["eval.mjs", "{{inputs.url}}"]}},
                {"id": "fix", "needs": ["evaluate"],
                 "when": {"step": "evaluate", "output": "/verdict", "equals": "findings"},
                 "retry": {"max": 1},
                 "cook": {"repo": "scratch", "prompt": "fix {{steps.evaluate.output/report}}", "verify": ["true"]}}
            ]
        })
    }

    #[test]
    fn a_valid_definition_validates_and_names_stable_instances() {
        let definition = definition(base()).unwrap();
        let inputs = definition
            .resolve_inputs(&BTreeMap::from([("url".to_string(), json!("u"))]))
            .unwrap();
        let id = definition.instance_id(&inputs, None);
        assert!(id.starts_with("wf-proof-") && id.len() == "wf-proof-".len() + 12);
        assert_eq!(id, definition.instance_id(&inputs, None));
        assert_ne!(id, definition.instance_id(&inputs, Some("again")));
        assert!(definition.resolve_inputs(&BTreeMap::new()).is_err());
    }

    #[test]
    fn invalid_definitions_are_rejected_with_their_reason() {
        type Case = (fn(&mut Value), &'static str);
        let cases: Vec<Case> = vec![
            (|v| v["steps"][1]["needs"] = json!(["fix"]), "cycle"),
            (
                |v| v["steps"][0]["needs"] = json!(["missing"]),
                "unknown step",
            ),
            (
                |v| v["steps"][1]["cook"]["bogus"] = json!("x"),
                "not an `agent-task cook` flag",
            ),
            (
                |v| v["steps"][1]["cook"]["wait"] = json!(true),
                "owned by the workflow",
            ),
            (
                |v| v["steps"][0]["exec"]["args"] = json!(["{{inputs.nope}}"]),
                "undeclared input",
            ),
            (
                |v| v["steps"][0]["exec"]["args"] = json!(["{{steps.fix.output/x}}"]),
                "not upstream",
            ),
            (
                |v| v["steps"][1]["homeboy"] = json!(["status"]),
                "exactly one",
            ),
            (
                |v| v["steps"][1]["retry"]["on"] = json!(["cancelled"]),
                "cannot be retried",
            ),
            (|v| v["steps"][0]["timeout"] = json!("90"), "unit required"),
            (|v| v["notify"] = json!({"on": ["failed"]}), "not supported"),
            (|v| v["every"] = json!({"interval": "30s"}), "at least 1m"),
            (
                |v| v["every"] = json!({"interval": "1h", "at": "25:00Z"}),
                "not a UTC time",
            ),
            (
                |v| v["every"] = json!({"interval": "1h", "overlap": "allow"}),
                "unknown variant",
            ),
            (|v| v["id"] = json!(".hidden"), "workflow id"),
            (
                |v| v["steps"][0]["foreach"] = json!("{{inputs.url}}"),
                "not supported",
            ),
            (
                |v| v["env"] = json!({"public": ["GITHUB_TOKEN"]}),
                "credential",
            ),
            (|v| v["iterate"]["max_iterations"] = json!(0), "at least 1"),
            (
                |v| v["steps"][1]["when"] = json!({"step": "other", "outcome": ["succeeded"]}),
                "not upstream",
            ),
            (|v| v["steps"][0]["id"] = json!("fix"), "declared twice"),
            (|v| v["schema"] = json!("homeboy/workflow/v0"), "schema"),
        ];
        for (mutate, reason) in cases {
            let mut value = base();
            mutate(&mut value);
            let error = definition(value).expect_err(reason);
            assert!(
                error.message.contains(reason) || error.to_string().contains(reason),
                "expected `{reason}`, got `{}`",
                error.message
            );
        }
    }

    #[test]
    fn every_parses_its_interval_and_utc_anchor() {
        let mut value = base();
        value["id"] = json!("homeboy.retention");
        value["every"] = json!({"interval": "2h", "at": "03:30Z", "overlap": "skip"});
        let definition = definition(value).unwrap();
        let every = definition.every.as_ref().unwrap();
        assert_eq!(every.interval().unwrap(), Duration::from_secs(7200));
        assert_eq!(
            every.at_offset().unwrap(),
            Some(Duration::from_secs(3 * 3600 + 30 * 60))
        );
        assert_eq!(every.overlap, Overlap::Skip);
    }

    #[test]
    fn digest_ignores_key_order() {
        let a = definition(base()).unwrap();
        let reordered: Value =
            serde_json::from_str(&serde_json::to_string_pretty(&base()).unwrap()).unwrap();
        assert_eq!(a.digest(), definition(reordered).unwrap().digest());
    }
}
