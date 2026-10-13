//! Workflow gates: a closed, deterministic predicate set.
//!
//! A gate is one of:
//!
//! - `{"step": id, "outcome": [..]}`: the step's latest outcome is in the set.
//! - `{"step": id, "output": "/pointer", <test>}`: a JSON-pointer test on the
//!   step's latest output, where `<test>` is exactly one of `equals`, `in`,
//!   `exists`, `lt`, `gt`.
//! - `{"command": [argv..]}`: the command exits 0. It runs as a step job, so
//!   it is logged and auditable like any other step.
//! - `{"all": [..]}`, `{"any": [..]}`, `{"not": gate}`.
//!
//! There is no expression language. Evaluation is pure: command gates report
//! `Pending` until their step job's result is supplied by the caller.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Value", into = "Value")]
pub enum Gate {
    Outcome {
        step: String,
        outcome: Vec<String>,
    },
    Output {
        step: String,
        pointer: String,
        test: OutputTest,
    },
    Command {
        argv: Vec<String>,
    },
    All(Vec<Gate>),
    Any(Vec<Gate>),
    Not(Box<Gate>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutputTest {
    Equals(Value),
    In(Vec<Value>),
    Exists(bool),
    Lt(f64),
    Gt(f64),
}

/// The value of a gate at one point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateValue {
    True,
    False,
    /// The gate cannot be decided until these command gates (by pre-order
    /// index) have run.
    Pending(Vec<u32>),
}

/// Facts a gate reads. Implementations come from the instance state.
pub trait GateFacts {
    /// The latest terminal outcome of `step`, if it has one.
    fn outcome(&self, step: &str) -> Option<&str>;
    /// The latest output of `step`, if it produced one.
    fn output(&self, step: &str) -> Option<&Value>;
    /// The result of command gate `index`, once its step job finished.
    fn command(&self, index: u32) -> Option<bool>;
}

impl Gate {
    /// Evaluate left to right with short-circuiting, so a command gate is only
    /// launched once every gate before it in its combinator is decided.
    pub fn evaluate(&self, facts: &dyn GateFacts) -> GateValue {
        let mut index = 0;
        self.evaluate_at(facts, &mut index)
    }

    fn evaluate_at(&self, facts: &dyn GateFacts, index: &mut u32) -> GateValue {
        match self {
            Gate::Outcome { step, outcome } => bool_value(
                facts
                    .outcome(step)
                    .is_some_and(|actual| outcome.iter().any(|expected| expected == actual)),
            ),
            Gate::Output {
                step,
                pointer,
                test,
            } => {
                let value = facts
                    .output(step)
                    .and_then(|output| output.pointer(pointer));
                bool_value(test.holds(value))
            }
            Gate::Command { .. } => {
                let own = *index;
                *index += 1;
                match facts.command(own) {
                    Some(passed) => bool_value(passed),
                    None => GateValue::Pending(vec![own]),
                }
            }
            Gate::All(gates) => {
                let mut result = GateValue::True;
                for gate in gates {
                    if result != GateValue::True {
                        // Decided or blocked: later children only consume indices.
                        gate.skip(index);
                        continue;
                    }
                    result = gate.evaluate_at(facts, index);
                }
                result
            }
            Gate::Any(gates) => {
                let mut result = GateValue::False;
                for gate in gates {
                    if result != GateValue::False {
                        gate.skip(index);
                        continue;
                    }
                    result = gate.evaluate_at(facts, index);
                }
                result
            }
            Gate::Not(gate) => match gate.evaluate_at(facts, index) {
                GateValue::True => GateValue::False,
                GateValue::False => GateValue::True,
                pending => pending,
            },
        }
    }

    fn skip(&self, index: &mut u32) {
        *index += self.command_gates().len() as u32;
    }

    /// Every command gate in pre-order. Its position is its stable index.
    pub fn command_gates(&self) -> Vec<&[String]> {
        let mut commands = Vec::new();
        self.collect_commands(&mut commands);
        commands
    }

    fn collect_commands<'a>(&'a self, commands: &mut Vec<&'a [String]>) {
        match self {
            Gate::Command { argv } => commands.push(argv),
            Gate::All(gates) | Gate::Any(gates) => {
                for gate in gates {
                    gate.collect_commands(commands);
                }
            }
            Gate::Not(gate) => gate.collect_commands(commands),
            Gate::Outcome { .. } | Gate::Output { .. } => {}
        }
    }

    /// Every step this gate reads.
    pub fn referenced_steps(&self) -> BTreeSet<&str> {
        let mut steps = BTreeSet::new();
        self.collect_steps(&mut steps);
        steps
    }

    fn collect_steps<'a>(&'a self, steps: &mut BTreeSet<&'a str>) {
        match self {
            Gate::Outcome { step, .. } | Gate::Output { step, .. } => {
                steps.insert(step);
            }
            Gate::All(gates) | Gate::Any(gates) => {
                for gate in gates {
                    gate.collect_steps(steps);
                }
            }
            Gate::Not(gate) => gate.collect_steps(steps),
            Gate::Command { .. } => {}
        }
    }
}

fn bool_value(value: bool) -> GateValue {
    if value {
        GateValue::True
    } else {
        GateValue::False
    }
}

impl OutputTest {
    fn holds(&self, value: Option<&Value>) -> bool {
        match self {
            OutputTest::Exists(expected) => value.is_some() == *expected,
            OutputTest::Equals(expected) => value == Some(expected),
            OutputTest::In(set) => value.is_some_and(|value| set.contains(value)),
            OutputTest::Lt(bound) => value
                .and_then(Value::as_f64)
                .is_some_and(|value| value < *bound),
            OutputTest::Gt(bound) => value
                .and_then(Value::as_f64)
                .is_some_and(|value| value > *bound),
        }
    }
}

impl TryFrom<Value> for Gate {
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let Value::Object(object) = value else {
            return Err("a gate must be a JSON object".to_string());
        };
        let keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
        let only = |allowed: &[&str]| -> Result<(), String> {
            let extra = keys
                .iter()
                .filter(|key| !allowed.contains(key))
                .copied()
                .collect::<Vec<_>>();
            if extra.is_empty() {
                Ok(())
            } else {
                Err(format!("unknown gate field(s): {}", extra.join(", ")))
            }
        };
        if let Some(gates) = object.get("all").or_else(|| object.get("any")) {
            only(&["all", "any"])?;
            if keys.len() != 1 {
                return Err("a gate combines with exactly one of `all` or `any`".to_string());
            }
            let gates = gates
                .as_array()
                .ok_or("`all`/`any` takes an array of gates")?
                .iter()
                .cloned()
                .map(Gate::try_from)
                .collect::<Result<Vec<_>, _>>()?;
            if gates.is_empty() {
                return Err("`all`/`any` needs at least one gate".to_string());
            }
            return Ok(if object.contains_key("all") {
                Gate::All(gates)
            } else {
                Gate::Any(gates)
            });
        }
        if let Some(gate) = object.get("not") {
            only(&["not"])?;
            return Ok(Gate::Not(Box::new(Gate::try_from(gate.clone())?)));
        }
        if let Some(argv) = object.get("command") {
            only(&["command"])?;
            let argv = argv
                .as_array()
                .ok_or("`command` takes an argv array")?
                .iter()
                .map(|arg| {
                    arg.as_str()
                        .map(str::to_string)
                        .ok_or("`command` arguments must be strings")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if argv.is_empty() || argv[0].trim().is_empty() {
                return Err("`command` needs a program".to_string());
            }
            return Ok(Gate::Command { argv });
        }
        let step = object
            .get("step")
            .and_then(Value::as_str)
            .ok_or("a gate needs one of `step`, `command`, `all`, `any`, or `not`")?
            .to_string();
        if let Some(outcome) = object.get("outcome") {
            only(&["step", "outcome"])?;
            let outcome = outcome
                .as_array()
                .ok_or("`outcome` takes an array of outcomes")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or("outcomes must be strings")
                })
                .collect::<Result<Vec<_>, _>>()?;
            if outcome.is_empty() {
                return Err("`outcome` needs at least one outcome".to_string());
            }
            return Ok(Gate::Outcome { step, outcome });
        }
        let pointer = object
            .get("output")
            .and_then(Value::as_str)
            .ok_or("a step gate needs `outcome` or `output`")?
            .to_string();
        if !pointer.is_empty() && !pointer.starts_with('/') {
            return Err(format!(
                "`output` must be a JSON pointer starting with `/`, got `{pointer}`"
            ));
        }
        only(&["step", "output", "equals", "in", "exists", "lt", "gt"])?;
        let tests = ["equals", "in", "exists", "lt", "gt"]
            .into_iter()
            .filter(|key| object.contains_key(*key))
            .collect::<Vec<_>>();
        let [test] = tests.as_slice() else {
            return Err(
                "an output gate takes exactly one of `equals`, `in`, `exists`, `lt`, `gt`"
                    .to_string(),
            );
        };
        let operand = object[*test].clone();
        let test = match *test {
            "equals" => OutputTest::Equals(operand),
            "in" => OutputTest::In(
                operand
                    .as_array()
                    .cloned()
                    .ok_or("`in` takes an array of values")?,
            ),
            "exists" => OutputTest::Exists(operand.as_bool().ok_or("`exists` takes a boolean")?),
            "lt" => OutputTest::Lt(operand.as_f64().ok_or("`lt` takes a number")?),
            _ => OutputTest::Gt(operand.as_f64().ok_or("`gt` takes a number")?),
        };
        Ok(Gate::Output {
            step,
            pointer,
            test,
        })
    }
}

impl From<Gate> for Value {
    fn from(gate: Gate) -> Self {
        match gate {
            Gate::Outcome { step, outcome } => json!({ "step": step, "outcome": outcome }),
            Gate::Output {
                step,
                pointer,
                test,
            } => {
                let (name, operand) = match test {
                    OutputTest::Equals(value) => ("equals", value),
                    OutputTest::In(values) => ("in", Value::Array(values)),
                    OutputTest::Exists(value) => ("exists", json!(value)),
                    OutputTest::Lt(value) => ("lt", json!(value)),
                    OutputTest::Gt(value) => ("gt", json!(value)),
                };
                json!({ "step": step, "output": pointer, name: operand })
            }
            Gate::Command { argv } => json!({ "command": argv }),
            Gate::All(gates) => {
                json!({ "all": gates.into_iter().map(Value::from).collect::<Vec<_>>() })
            }
            Gate::Any(gates) => {
                json!({ "any": gates.into_iter().map(Value::from).collect::<Vec<_>>() })
            }
            Gate::Not(gate) => json!({ "not": Value::from(*gate) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Facts {
        outcomes: BTreeMap<String, String>,
        outputs: BTreeMap<String, Value>,
        commands: BTreeMap<u32, bool>,
    }

    impl GateFacts for Facts {
        fn outcome(&self, step: &str) -> Option<&str> {
            self.outcomes.get(step).map(String::as_str)
        }
        fn output(&self, step: &str) -> Option<&Value> {
            self.outputs.get(step)
        }
        fn command(&self, index: u32) -> Option<bool> {
            self.commands.get(&index).copied()
        }
    }

    fn gate(value: Value) -> Gate {
        Gate::try_from(value).unwrap()
    }

    #[test]
    fn outcome_and_output_gates_read_latest_facts() {
        let mut facts = Facts::default();
        facts
            .outcomes
            .insert("a".into(), "candidate_recoverable".into());
        facts.outputs.insert(
            "a".into(),
            json!({"verdict": "findings", "count": 3, "list": [1]}),
        );
        let cases = [
            (
                json!({"step":"a","outcome":["succeeded","candidate_recoverable"]}),
                GateValue::True,
            ),
            (
                json!({"step":"a","outcome":["succeeded"]}),
                GateValue::False,
            ),
            (
                json!({"step":"b","outcome":["succeeded"]}),
                GateValue::False,
            ),
            (
                json!({"step":"a","output":"/verdict","equals":"findings"}),
                GateValue::True,
            ),
            (
                json!({"step":"a","output":"/verdict","in":["accepted","findings"]}),
                GateValue::True,
            ),
            (
                json!({"step":"a","output":"/missing","exists":false}),
                GateValue::True,
            ),
            (
                json!({"step":"a","output":"/list/0","exists":true}),
                GateValue::True,
            ),
            (
                json!({"step":"a","output":"/count","lt":4}),
                GateValue::True,
            ),
            (
                json!({"step":"a","output":"/count","gt":3}),
                GateValue::False,
            ),
            (
                json!({"not":{"step":"a","outcome":["succeeded"]}}),
                GateValue::True,
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(gate(value.clone()).evaluate(&facts), expected, "{value}");
        }
    }

    #[test]
    fn command_gates_short_circuit_in_pre_order() {
        let any = gate(json!({"any": [
            {"command": ["true"]},
            {"all": [{"command": ["a"]}, {"command": ["b"]}]}
        ]}));
        let mut facts = Facts::default();
        assert_eq!(any.evaluate(&facts), GateValue::Pending(vec![0]));
        facts.commands.insert(0, false);
        assert_eq!(any.evaluate(&facts), GateValue::Pending(vec![1]));
        facts.commands.insert(1, true);
        assert_eq!(any.evaluate(&facts), GateValue::Pending(vec![2]));
        facts.commands.insert(2, false);
        assert_eq!(any.evaluate(&facts), GateValue::False);
        facts.commands.insert(0, true);
        assert_eq!(any.evaluate(&facts), GateValue::True);
        assert_eq!(any.command_gates().len(), 3);
    }

    #[test]
    fn malformed_gates_are_rejected_and_valid_gates_round_trip() {
        for bad in [
            json!("x"),
            json!({"step":"a"}),
            json!({"step":"a","output":"/x"}),
            json!({"step":"a","output":"/x","equals":1,"gt":2}),
            json!({"step":"a","output":"x","equals":1}),
            json!({"all":[]}),
            json!({"command":[]}),
            json!({"step":"a","outcome":["succeeded"],"extra":true}),
            json!({"expr":"a == b"}),
        ] {
            assert!(Gate::try_from(bad.clone()).is_err(), "{bad}");
        }
        for good in [
            json!({"step":"a","output":"/x","equals":{"k":1}}),
            json!({"any":[{"command":["homeboy","review","lint"]},{"not":{"step":"b","outcome":["failed"]}}]}),
        ] {
            assert_eq!(Value::from(gate(good.clone())), good);
        }
    }
}
