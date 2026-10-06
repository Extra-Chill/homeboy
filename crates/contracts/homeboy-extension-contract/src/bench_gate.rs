//! Pure bench gate contract types + their evaluation logic.

use serde::{Deserialize, Serialize};

use crate::BenchMetrics;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchGate {
    pub metric: String,
    pub op: BenchGateOp,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BenchGateOp {
    Eq,
    Gte,
    Lte,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchGateResult {
    pub metric: String,
    pub op: BenchGateOp,
    pub expected: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BenchGate {
    pub fn evaluate(&self, scenario_id: &str, metrics: &BenchMetrics) -> BenchGateResult {
        let actual = metrics.get(&self.metric);
        self.evaluate_actual(&format!("scenario `{}`", scenario_id), actual)
    }

    pub fn evaluate_actual(&self, scope: &str, actual: Option<f64>) -> BenchGateResult {
        let passed = actual
            .map(|value| match self.op {
                BenchGateOp::Eq => value == self.value,
                BenchGateOp::Gte => value >= self.value,
                BenchGateOp::Lte => value <= self.value,
            })
            .unwrap_or(false);
        let reason = if passed {
            None
        } else {
            Some(match actual {
                Some(value) => format!(
                    "{} gate failed: {} {} {} (actual {})",
                    scope,
                    self.metric,
                    self.op.as_str(),
                    self.value,
                    value
                ),
                None => format!("{} gate failed: metric `{}` is missing", scope, self.metric),
            })
        };

        BenchGateResult {
            metric: self.metric.clone(),
            op: self.op,
            expected: self.value,
            actual,
            passed,
            reason,
        }
    }
}

impl BenchGateOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BenchGateOp::Eq => "eq",
            BenchGateOp::Gte => "gte",
            BenchGateOp::Lte => "lte",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_results_and_diagnostics_use_fixed_operator_labels() {
        for (op, label, failing_actual, passing_actual, reason) in [
            (
                BenchGateOp::Eq,
                "eq",
                0.0,
                1.0,
                "scenario `candidate` gate failed: rate eq 1 (actual 0)",
            ),
            (
                BenchGateOp::Gte,
                "gte",
                0.0,
                2.0,
                "scenario `candidate` gate failed: rate gte 1 (actual 0)",
            ),
            (
                BenchGateOp::Lte,
                "lte",
                2.0,
                0.0,
                "scenario `candidate` gate failed: rate lte 1 (actual 2)",
            ),
        ] {
            let gate: BenchGate = serde_json::from_value(serde_json::json!({
                "metric": "rate", "op": label, "value": 1.0
            }))
            .unwrap();
            assert_eq!(gate.op, op);

            for (actual, passed) in [(failing_actual, false), (passing_actual, true)] {
                let metrics: BenchMetrics =
                    serde_json::from_value(serde_json::json!({ "rate": actual })).unwrap();
                let result = serde_json::to_value(gate.evaluate("candidate", &metrics)).unwrap();
                let mut expected = serde_json::json!({
                    "metric": "rate",
                    "op": label,
                    "expected": 1.0,
                    "actual": actual,
                    "passed": passed
                });
                if !passed {
                    expected["reason"] = serde_json::json!(reason);
                }
                assert_eq!(result, expected);
            }
        }
    }
}
