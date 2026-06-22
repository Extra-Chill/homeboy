//! Execution results parsing and workload status evaluation.

use std::path::Path;

use crate::core::error::Result;
use crate::core::extension::bench::parsing::{self, BenchResults, BenchScenario};

use super::scenario::{apply_scenario_filter, normalize_workload_json_scenario_ids};

pub(crate) fn parse_execution_results_file(
    results_file: &Path,
    scenario_ids: &[String],
    runner_success: bool,
    rig_id: Option<&str>,
) -> Result<Option<BenchResults>> {
    if !results_file.exists() {
        return Ok(None);
    }

    if runner_success {
        let mut results = parsing::parse_bench_results_file_with_artifact_context_and_scenarios(
            results_file,
            rig_id,
            scenario_ids,
        )?;
        normalize_workload_json_scenario_ids(&mut results);
        return Ok(Some(apply_scenario_filter(results, scenario_ids)?));
    }

    let mut results = parsing::parse_bench_results_file_with_artifact_context_and_scenarios(
        results_file,
        rig_id,
        scenario_ids,
    )
    .ok();
    if let Some(results) = &mut results {
        normalize_workload_json_scenario_ids(results);
        if is_unmeasured_inventory_results(results) {
            return Ok(None);
        }
    }
    Ok(results)
}

pub(crate) fn is_unmeasured_inventory_results(results: &BenchResults) -> bool {
    results.iterations == 0
        && !results.scenarios.is_empty()
        && results.scenarios.iter().all(|scenario| {
            scenario.iterations == 0
                && scenario.metrics.values.is_empty()
                && scenario.metrics.distributions.is_empty()
                && scenario.metric_groups.is_empty()
                && scenario.memory.is_none()
                && scenario.artifacts.is_empty()
                && scenario.runs.is_none()
        })
}

pub(crate) fn workload_status_failures(results: &BenchResults) -> Vec<String> {
    results
        .scenarios
        .iter()
        .filter_map(|scenario| {
            if !scenario.passed {
                return Some(format!(
                    "bench scenario `{}` reported passed=false",
                    scenario.id
                ));
            }

            let failed = child_status_count(scenario, "failed").unwrap_or(0.0);
            let unsupported = child_status_count(scenario, "unsupported").unwrap_or(0.0);
            let warning = child_status_count(scenario, "warning").unwrap_or(0.0);
            let passed = child_status_count(scenario, "passed");
            let total = scenario.metrics.get("adapter_count");

            if failed > 0.0 || unsupported > 0.0 || warning > 0.0 {
                return Some(format!(
                    "bench scenario `{}` reported failed_count={} unsupported_count={} warning_count={} passed_count={}",
                    scenario.id,
                    format_count(failed),
                    format_count(unsupported),
                    format_count(warning),
                    passed
                        .map(format_count)
                        .unwrap_or_else(|| "<unknown>".to_string())
                ));
            }

            if passed == Some(0.0) && total.is_some_and(|count| count > 0.0) {
                return Some(format!(
                    "bench scenario `{}` reported no passing children out of adapter_count={} (failed_count={} unsupported_count={} warning_count={})",
                    scenario.id,
                    format_count(total.unwrap_or(0.0)),
                    format_count(failed),
                    format_count(unsupported),
                    format_count(warning)
                ));
            }

            None
        })
        .collect()
}

pub(crate) fn child_status_count(scenario: &BenchScenario, status: &str) -> Option<f64> {
    scenario
        .metrics
        .get(&format!("{status}_count"))
        .or_else(|| metadata_result_count(scenario, status))
}

pub(crate) fn metadata_result_count(scenario: &BenchScenario, status: &str) -> Option<f64> {
    scenario
        .metadata
        .get("result_counts")?
        .as_object()?
        .get(status)?
        .as_f64()
}

pub(crate) fn format_count(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}
