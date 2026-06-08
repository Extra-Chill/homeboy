use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};

use super::budget_findings;
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
                    budget_findings::failure(
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
            .filter(|finding| budget_findings::is_gate_failure(finding))
            .map(|finding| finding.message.clone()),
    );
    failures.sort();
    failures.dedup();
    failures
}

impl From<BenchGateResult> for HomeboyGateResult {
    fn from(result: BenchGateResult) -> Self {
        let status = if result.passed {
            HomeboyGateStatus::Passed
        } else {
            HomeboyGateStatus::Failed
        };
        let summary = result.reason.clone().unwrap_or_else(|| {
            format!(
                "metric gate passed: {} {} {}",
                result.metric,
                result.op.as_str(),
                result.expected
            )
        });

        HomeboyGateResult::new(
            format!("bench.gate.{}", result.metric),
            result.metric.clone(),
            HomeboyGateKind::Metric,
            status,
        )
        .summary(summary)
        .evidence(json!({
            "metric": result.metric,
            "op": result.op,
            "expected": result.expected,
            "actual": result.actual,
            "passed": result.passed,
            "reason": result.reason,
        }))
        .retryable(status == HomeboyGateStatus::Failed)
        .provenance(json!({
            "source_type": "BenchGateResult",
        }))
    }
}

#[cfg(test)]
mod normalization_tests {
    use super::*;
    use crate::core::gate::HOMEBOY_GATE_RESULT_SCHEMA;

    #[test]
    fn bench_gate_result_normalizes_to_homeboy_gate_result() {
        let result: HomeboyGateResult = BenchGateResult {
            metric: "p95_ms".to_string(),
            op: BenchGateOp::Lte,
            expected: 120.0,
            actual: Some(140.0),
            passed: false,
            reason: Some(
                "scenario `homepage` gate failed: p95_ms lte 120 (actual 140)".to_string(),
            ),
        }
        .into();

        assert_eq!(result.schema, HOMEBOY_GATE_RESULT_SCHEMA);
        assert_eq!(result.id, "bench.gate.p95_ms");
        assert_eq!(result.kind, HomeboyGateKind::Metric);
        assert_eq!(result.status, HomeboyGateStatus::Failed);
        assert_eq!(result.retryable, Some(true));
        assert_eq!(result.evidence["metric"], "p95_ms");
        assert_eq!(result.evidence["actual"], 140.0);
        assert_eq!(result.provenance["source_type"], "BenchGateResult");
    }

    #[test]
    fn successful_bench_gate_result_normalizes_to_passed_gate_result() {
        let result: HomeboyGateResult = BenchGateResult {
            metric: "success_rate".to_string(),
            op: BenchGateOp::Gte,
            expected: 1.0,
            actual: Some(1.0),
            passed: true,
            reason: None,
        }
        .into();

        assert_eq!(result.id, "bench.gate.success_rate");
        assert_eq!(result.kind, HomeboyGateKind::Metric);
        assert_eq!(result.status, HomeboyGateStatus::Passed);
        assert_eq!(result.retryable, Some(false));
        assert_eq!(result.evidence["passed"], true);
        assert!(result.summary.contains("metric gate passed"));
    }
}
