use serde::{Deserialize, Serialize};
use serde_json::json;

use homeboy_core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};
pub use homeboy_extension_contract::bench_gate::{BenchGate, BenchGateOp, BenchGateResult};

use super::budget_findings;
use super::parsing::{BenchMetrics, BenchResults};

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

pub fn normalized_gate_results(results: &BenchResults) -> Vec<HomeboyGateResult> {
    results
        .scenarios
        .iter()
        .flat_map(|scenario| {
            scenario
                .gate_results
                .iter()
                .cloned()
                .map(|result| normalized_gate_result_for_scenario(&scenario.id, result))
        })
        .collect()
}

fn normalized_gate_result_for_scenario(
    scenario_id: &str,
    result: BenchGateResult,
) -> HomeboyGateResult {
    let status = if result.passed {
        HomeboyGateStatus::Passed
    } else {
        HomeboyGateStatus::Failed
    };
    let summary = result.reason.clone().unwrap_or_else(|| {
        format!(
            "scenario `{}` metric gate passed: {} {} {}",
            scenario_id,
            result.metric,
            result.op.as_str(),
            result.expected
        )
    });

    HomeboyGateResult::new(
        format!("bench.gate.{}.{}", scenario_id, result.metric),
        format!("{}.{}", scenario_id, result.metric),
        HomeboyGateKind::Metric,
        status,
    )
    .summary(summary)
    .evidence(json!({
        "scenario_id": scenario_id,
        "metric": result.metric,
        "op": result.op,
        "expected": result.expected,
        "actual": result.actual,
        "passed": result.passed,
        "reason": result.reason,
    }))
    .retryable(status == HomeboyGateStatus::Failed)
    .agent_feedback(if status == HomeboyGateStatus::Failed {
        format!(
            "Bench gate `{}` failed for scenario `{}`. Use the metric evidence to adjust the candidate while preserving the benchmark target.",
            result.metric, scenario_id
        )
    } else {
        String::new()
    })
    .provenance(json!({
        "source_type": "BenchGateResult",
        "scenario_id": scenario_id,
    }))
}

#[cfg(test)]
mod normalization_tests {
    use super::*;

    #[test]
    fn normalized_gate_results_are_scenario_scoped_and_agent_actionable() {
        let mut results = crate::bench::parsing::parse_bench_results_str(
            r#"{
                "component_id": "homeboy",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "baseline",
                        "iterations": 1,
                        "metrics": { "success_rate": 1.0 },
                        "gates": [
                            { "metric": "success_rate", "op": "eq", "value": 1.0 }
                        ]
                    },
                    {
                        "id": "candidate",
                        "iterations": 1,
                        "metrics": { "success_rate": 0.0 },
                        "gates": [
                            { "metric": "success_rate", "op": "eq", "value": 1.0 }
                        ]
                    }
                ]
            }"#,
        )
        .expect("bench results");

        let failures = evaluate_gates(&mut results);
        assert_eq!(failures.len(), 1);

        let normalized = normalized_gate_results(&results);
        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0].id, "bench.gate.baseline.success_rate");
        assert_eq!(normalized[1].id, "bench.gate.candidate.success_rate");
        assert_eq!(normalized[0].status, HomeboyGateStatus::Passed);
        assert_eq!(normalized[1].status, HomeboyGateStatus::Failed);
        assert_eq!(normalized[1].retryable, Some(true));
        assert!(normalized[1]
            .agent_feedback
            .contains("Bench gate `success_rate` failed"));
        assert_eq!(normalized[1].evidence["scenario_id"], "candidate");
        assert_eq!(normalized[1].evidence["actual"], 0.0);
    }
}
