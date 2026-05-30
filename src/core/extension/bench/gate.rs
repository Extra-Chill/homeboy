use serde::{Deserialize, Serialize};

use crate::core::budget::BudgetFinding;

use super::parsing::{BenchMetrics, BenchResults};

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
    fn evaluate(&self, scenario_id: &str, metrics: &BenchMetrics) -> BenchGateResult {
        let actual = metrics.get(&self.metric);
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
                    "scenario `{}` gate failed: {} {} {} (actual {})",
                    scenario_id,
                    self.metric,
                    self.op.as_str(),
                    self.value,
                    value
                ),
                None => format!(
                    "scenario `{}` gate failed: metric `{}` is missing",
                    scenario_id, self.metric
                ),
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
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BenchGateOp::Eq => "eq",
            BenchGateOp::Gte => "gte",
            BenchGateOp::Lte => "lte",
        }
    }
}

/// Evaluate semantic gates in place and return every failure reason.
pub fn evaluate_gates(results: &mut BenchResults) -> Vec<String> {
    let mut failures = Vec::new();
    for scenario in &mut results.scenarios {
        scenario.gate_results = scenario
            .gates
            .iter()
            .map(|gate| gate.evaluate(&scenario.id, &scenario.metrics))
            .collect();
        scenario.passed = scenario.gate_results.iter().all(|result| result.passed);
        results.budget_findings.extend(
            scenario
                .gate_results
                .iter()
                .filter(|result| !result.passed)
                .map(|result| {
                    BudgetFinding::failure(
                        format!("bench.gate.{}", result.metric),
                        format!("bench:{}", scenario.id),
                        result.reason.clone().unwrap_or_else(|| {
                            format!(
                                "scenario `{}` gate failed: {} {} {}",
                                scenario.id,
                                result.metric,
                                result.op.as_str(),
                                result.expected
                            )
                        }),
                        result.actual,
                        result.expected,
                        "value",
                        Some(result.metric.clone()),
                    )
                    .to_homeboy_finding()
                }),
        );
        failures.extend(
            scenario
                .gate_results
                .iter()
                .filter_map(|result| result.reason.clone()),
        );
    }
    failures.extend(
        results
            .budget_findings
            .iter()
            .filter(|finding| is_budget_gate_failure(finding))
            .map(|finding| finding.message.clone()),
    );
    failures.sort();
    failures.dedup();
    failures
}

fn is_budget_gate_failure(finding: &crate::core::finding::HomeboyFinding) -> bool {
    finding.severity.as_deref() == Some("error")
        || finding
            .metadata
            .get("passed")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
}
