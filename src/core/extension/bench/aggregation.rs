use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::extension::bench::distribution::{distribution, percentile};
use crate::extension::bench::parsing::{
    BenchMetrics, BenchResults, BenchRunSnapshot, BenchScenario,
};

pub fn aggregate_runs(runs: &[BenchResults]) -> Result<BenchResults> {
    let first = runs
        .first()
        .ok_or_else(|| Error::internal_unexpected("cannot aggregate zero bench runs"))?;
    let mut metric_policies = BTreeMap::new();
    let mut grouped: BTreeMap<String, (BenchScenario, Vec<BenchScenario>)> = BTreeMap::new();

    for result in runs {
        if result.component_id != first.component_id {
            return Err(Error::validation_invalid_argument(
                "bench_results.component_id",
                format!(
                    "bench run component_id mismatch: expected `{}`, got `{}`",
                    first.component_id, result.component_id
                ),
                None,
                None,
            ));
        }
        if result.iterations != first.iterations {
            return Err(Error::validation_invalid_argument(
                "bench_results.iterations",
                format!(
                    "bench run iterations mismatch: expected `{}`, got `{}`",
                    first.iterations, result.iterations
                ),
                None,
                None,
            ));
        }
        for (key, policy) in &result.metric_policies {
            metric_policies
                .entry(key.clone())
                .or_insert_with(|| policy.clone());
        }
        for scenario in &result.scenarios {
            grouped
                .entry(scenario.id.clone())
                .and_modify(|(_, scenarios)| scenarios.push(scenario.clone()))
                .or_insert_with(|| (scenario.clone(), vec![scenario.clone()]));
        }
    }

    let scenarios = grouped
        .into_values()
        .map(|(template, scenarios)| aggregate_scenario(template, scenarios))
        .collect();

    Ok(BenchResults {
        component_id: first.component_id.clone(),
        iterations: first.iterations,
        run_metadata: first.run_metadata.clone(),
        scenarios,
        metric_policies,
    })
}

fn aggregate_scenario(mut template: BenchScenario, scenarios: Vec<BenchScenario>) -> BenchScenario {
    let mut metric_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for scenario in &scenarios {
        for (name, value) in &scenario.metrics.values {
            metric_values.entry(name.clone()).or_default().push(*value);
        }
    }

    let mut values = BTreeMap::new();
    let mut distributions = BTreeMap::new();
    let mut summary = BTreeMap::new();
    for (name, samples) in metric_values {
        values.insert(name.clone(), percentile(&samples, 50.0));
        distributions.insert(name.clone(), samples.clone());
        summary.insert(name, distribution(&samples));
    }

    template.metrics = BenchMetrics {
        values,
        distributions,
    };
    template.memory = None;
    template.runs = Some(
        scenarios
            .iter()
            .map(|scenario| BenchRunSnapshot {
                metrics: scenario.metrics.clone(),
                memory: scenario.memory.clone(),
                artifacts: scenario.artifacts.clone(),
            })
            .collect(),
    );
    template.runs_summary = Some(summary);
    template
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/aggregation_test.rs"]
mod aggregation_test;

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/runs_flag_test.rs"]
mod runs_flag_test;
