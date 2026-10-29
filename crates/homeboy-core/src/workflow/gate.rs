//! Workflow gates: a closed, deterministic predicate set.
//!
//! A gate is one of:
//!
//! - `{"step": id, "outcome": [..]}`: the step's outcome is in the set.
//! - `{"step": id, "output": "/pointer", <test>}`: a JSON-pointer test on the
//!   step's output, where `<test>` is exactly one of `equals`, `in`, `exists`,
//!   `lt`, `gt`.
//! - `{"all": [..]}`, `{"any": [..]}`, `{"not": gate}`.
//!
//! There is no expression language. Evaluation is pure: a gate only reads the
//! outcomes and outputs of steps in the same instance.

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

/// Facts a gate reads. Implementations come from the instance state.
pub trait GateFacts {
    /// The outcome of `step`, if it has one.
    fn outcome(&self, step: &str) -> Option<&str>;
    /// The output of `step`, if it produced one.
    fn output(&self, step: &str) -> Option<&Value>;
}

impl Gate {
    /// Whether the gate passes. A pure read of the supplied facts.
    pub fn evaluate(&self, facts: &dyn GateFacts) -> bool {
        match self {
            Gate::Outcome { step, outcome } => facts
                .outcome(step)
                .is_some_and(|actual| outcome.iter().any(|expected| expected == actual)),
            Gate::Output {
                step,
                pointer,
                test,
            } => {
                let value = facts
                    .output(step)
                    .and_then(|output| output.pointer(pointer));
                test.holds(value)
            }
            Gate::All(gates) => gates.iter().all(|gate| gate.evaluate(facts)),
            Gate::Any(gates) => gates.iter().any(|gate| gate.evaluate(facts)),
            Gate::Not(gate) => !gate.evaluate(facts),
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
        }
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
        if object.contains_key("command") {
            return Err(
                "command gates are not supported: run the command as an `exec` step and gate on its `outcome`"
                    .to_string(),
            );
        }
        let step = object
            .get("step")
            .and_then(Value::as_str)
            .ok_or("a gate needs one of `step`, `all`, `any`, or `not`")?
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
    }

    impl GateFacts for Facts {
        fn outcome(&self, step: &str) -> Option<&str> {
            self.outcomes.get(step).map(String::as_str)
        }
        fn output(&self, step: &str) -> Option<&Value> {
            self.outputs.get(step)
        }
    }

    fn gate(value: Value) -> Gate {
        Gate::try_from(value).unwrap()
    }

    #[test]
    fn outcome_and_output_gates_read_step_facts() {
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
                true,
            ),
            (json!({"step":"a","outcome":["succeeded"]}), false),
            (json!({"step":"b","outcome":["succeeded"]}), false),
            (json!({"step":"a","output":"/verdict","equals":"findings"}), true),
            (
                json!({"step":"a","output":"/verdict","in":["accepted","findings"]}),
                true,
            ),
            (json!({"step":"a","output":"/missing","exists":false}), true),
            (json!({"step":"a","output":"/list/0","exists":true}), true),
            (json!({"step":"a","output":"/count","lt":4}), true),
            (json!({"step":"a","output":"/count","gt":3}), false),
            (
                json!({"not":{"step":"a","outcome":["succeeded"]}}),
                true,
            ),
            (
                json!({"all":[{"step":"a","outcome":["candidate_recoverable"]},{"step":"a","output":"/count","lt":4}]}),
                true,
            ),
            (
                json!({"any":[{"step":"a","outcome":["succeeded"]},{"step":"a","output":"/count","lt":4}]}),
                true,
            ),
            (
                json!({"any":[{"step":"a","outcome":["succeeded"]},{"step":"a","output":"/count","gt":4}]}),
                false,
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(gate(value.clone()).evaluate(&facts), expected, "{value}");
        }
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
            json!({"command":["true"]}),
            json!({"step":"a","outcome":["succeeded"],"extra":true}),
            json!({"expr":"a == b"}),
        ] {
            assert!(Gate::try_from(bad.clone()).is_err(), "{bad}");
        }
        assert_eq!(
            Gate::try_from(json!({"command":["true"]})).unwrap_err(),
            "command gates are not supported: run the command as an `exec` step and gate on its `outcome`"
        );
        for good in [
            json!({"step":"a","output":"/x","equals":{"k":1}}),
            json!({"any":[{"step":"b","output":"/ok","exists":true},{"not":{"step":"b","outcome":["failed"]}}]}),
        ] {
            assert_eq!(Value::from(gate(good.clone())), good);
        }
    }
}
