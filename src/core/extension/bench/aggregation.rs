use std::collections::BTreeMap;

use crate::core::error::{Error, Result};
use crate::core::extension::bench::distribution::{distribution, percentile};
use crate::core::extension::bench::parsing::{
    BenchMetrics, BenchResults, BenchRunSnapshot, BenchScenario,
};
use crate::core::observation::timeline::{ObservationSpanResult, ObservationSpanStatus};

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
        provenance: first.provenance.clone(),
        run_metadata: first.run_metadata.clone(),
        metadata: first.metadata.clone(),
        metric_groups: first.metric_groups.clone(),
        timeline: first.timeline.clone(),
        span_definitions: first.span_definitions.clone(),
        diagnostics: runs
            .iter()
            .flat_map(|result| result.diagnostics.clone())
            .collect(),
        child_command_failures: runs
            .iter()
            .flat_map(|result| result.child_command_failures.clone())
            .collect(),
        phase_events: runs
            .iter()
            .flat_map(|result| result.phase_events.clone())
            .collect(),
        phase_summaries: runs
            .iter()
            .flat_map(|result| result.phase_summaries.clone())
            .collect(),
        failure_classification: runs
            .iter()
            .find_map(|result| result.failure_classification.clone()),
        responsiveness: runs.iter().find_map(|result| result.responsiveness.clone()),
        budget_findings: runs
            .iter()
            .flat_map(|result| result.budget_findings.clone())
            .collect(),
        scenarios,
        metric_policies,
        metric_policy_presets: BTreeMap::new(),
    })
}

fn aggregate_scenario(mut template: BenchScenario, scenarios: Vec<BenchScenario>) -> BenchScenario {
    let mut metric_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut grouped_metric_values: BTreeMap<String, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    for scenario in &scenarios {
        for (name, value) in &scenario.metrics.values {
            metric_values.entry(name.clone()).or_default().push(*value);
        }
        for (group, values) in &scenario.metric_groups {
            let group_values = grouped_metric_values.entry(group.clone()).or_default();
            for (name, value) in values {
                group_values.entry(name.clone()).or_default().push(*value);
            }
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
    template.metric_groups = grouped_metric_values
        .into_iter()
        .map(|(group, values)| {
            let aggregated = values
                .into_iter()
                .map(|(name, samples)| (name, percentile(&samples, 50.0)))
                .collect();
            (group, aggregated)
        })
        .collect();
    template.span_results = aggregate_span_results(&template.span_results, &scenarios);
    template.memory = None;
    template.runs = Some(
        scenarios
            .iter()
            .map(|scenario| BenchRunSnapshot {
                metrics: scenario.metrics.clone(),
                metric_groups: scenario.metric_groups.clone(),
                timeline: scenario.timeline.clone(),
                span_definitions: scenario.span_definitions.clone(),
                span_results: scenario.span_results.clone(),
                memory: scenario.memory.clone(),
                artifacts: scenario.artifacts.clone(),
                diagnostics: scenario.diagnostics.clone(),
            })
            .collect(),
    );
    template.runs_summary = Some(summary);
    template
}

fn aggregate_span_results(
    template_results: &[ObservationSpanResult],
    scenarios: &[BenchScenario],
) -> Vec<ObservationSpanResult> {
    let mut by_id: BTreeMap<String, Vec<&ObservationSpanResult>> = BTreeMap::new();
    for scenario in scenarios {
        for span in &scenario.span_results {
            by_id.entry(span.id.clone()).or_default().push(span);
        }
    }

    let template_by_id = template_results
        .iter()
        .map(|span| (span.id.as_str(), span))
        .collect::<BTreeMap<_, _>>();

    by_id
        .into_iter()
        .map(|(id, spans)| {
            aggregate_span_result(&id, spans, template_by_id.get(id.as_str()).copied())
        })
        .collect()
}

fn aggregate_span_result(
    id: &str,
    spans: Vec<&ObservationSpanResult>,
    template: Option<&ObservationSpanResult>,
) -> ObservationSpanResult {
    let ok_durations = spans
        .iter()
        .filter_map(|span| {
            (span.status == ObservationSpanStatus::Ok)
                .then_some(span.duration_ms)
                .flatten()
                .map(|duration| duration as f64)
        })
        .collect::<Vec<_>>();
    let base = template.or_else(|| spans.first().copied());
    let from = base.map(|span| span.from.clone()).unwrap_or_default();
    let to = base.map(|span| span.to.clone()).unwrap_or_default();

    if !ok_durations.is_empty() {
        let duration_ms = percentile(&ok_durations, 50.0).round() as u64;
        return ObservationSpanResult {
            id: id.to_string(),
            from,
            to,
            status: ObservationSpanStatus::Ok,
            duration_ms: Some(duration_ms),
            from_t_ms: None,
            to_t_ms: None,
            missing: Vec::new(),
            message: None,
        };
    }

    let missing = spans
        .iter()
        .flat_map(|span| span.missing.clone())
        .collect::<Vec<_>>();
    ObservationSpanResult {
        id: id.to_string(),
        from,
        to,
        status: ObservationSpanStatus::Skipped,
        duration_ms: None,
        from_t_ms: None,
        to_t_ms: None,
        missing,
        message: Some("span missing from every aggregated run".to_string()),
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/aggregation_test.rs"]
mod aggregation_test;

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/runs_flag_test.rs"]
mod runs_flag_test;
