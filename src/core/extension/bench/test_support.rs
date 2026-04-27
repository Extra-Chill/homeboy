use std::collections::BTreeMap;

use crate::extension::bench::parsing::{BenchMetrics, BenchResults, BenchScenario};

pub(crate) fn scenario_with_iterations(
    id: &str,
    metrics: &[(&str, f64)],
    iterations: u64,
) -> BenchScenario {
    let mut values = BTreeMap::new();
    for (name, value) in metrics {
        values.insert((*name).to_string(), *value);
    }

    BenchScenario {
        id: id.to_string(),
        file: None,
        source: None,
        default_iterations: None,
        tags: Vec::new(),
        iterations,
        metrics: BenchMetrics {
            values,
            distributions: BTreeMap::new(),
        },
        memory: None,
        artifacts: BTreeMap::new(),
        runs: None,
        runs_summary: None,
    }
}

pub(crate) fn results_with_scenarios(
    component_id: &str,
    iterations: u64,
    scenarios: Vec<BenchScenario>,
) -> BenchResults {
    BenchResults {
        component_id: component_id.to_string(),
        iterations,
        scenarios,
        metric_policies: BTreeMap::new(),
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/test_support_test.rs"]
mod test_support_test;
