use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::observation::{ObservationStore, RunListFilter, RunRecord};
use homeboy::core::Error;
use serde::Serialize;
use serde_json::Value;

use super::{require_run, run_detail, run_summary, CmdResult, RunDetail, RunSummary, RunsOutput};

#[derive(Serialize)]
pub struct BenchHistoryOutput {
    pub command: &'static str,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    pub runs: Vec<RunDetail>,
}

#[derive(Serialize)]
pub struct BenchCompareOutput {
    pub command: &'static str,
    pub from_run: RunSummary,
    pub to_run: RunSummary,
    pub comparisons: Vec<BenchMetricComparison>,
    pub missing: Vec<BenchMissingMetric>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BenchMetricComparison {
    pub scenario_id: String,
    pub metric: String,
    pub from_value: f64,
    pub to_value: f64,
    pub delta: f64,
    pub percent_change: Option<f64>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct BenchMissingMetric {
    pub scenario_id: String,
    pub metric: String,
    pub missing_from: String,
}

pub fn bench_history(
    component_id: &str,
    scenario_id: Option<&str>,
    rig_id: Option<&str>,
    limit: i64,
) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let runs = store
        .list_runs(RunListFilter {
            kind: Some("bench".to_string()),
            component_id: Some(component_id.to_string()),
            rig_id: rig_id.map(str::to_string),
            limit: Some(limit.clamp(1, 1000)),
            ..RunListFilter::default()
        })?
        .into_iter()
        .filter(|run| scenario_id.is_none_or(|scenario| run_contains_scenario(run, scenario)))
        .take(limit.max(1) as usize)
        .map(|run| run_detail(&store, run))
        .collect::<homeboy::core::Result<Vec<_>>>()?;

    Ok((
        RunsOutput::BenchHistory(BenchHistoryOutput {
            command: "bench.history",
            component_id: component_id.to_string(),
            scenario_id: scenario_id.map(str::to_string),
            rig_id: rig_id.map(str::to_string),
            runs,
        }),
        0,
    ))
}

pub fn bench_compare(from_run_id: &str, to_run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let from_run = require_run(&store, from_run_id)?;
    let to_run = require_run(&store, to_run_id)?;
    require_kind(&from_run, "bench")?;
    require_kind(&to_run, "bench")?;

    let from_summary = run_summary(from_run.clone());
    let to_summary = run_summary(to_run.clone());
    let from_metrics = bench_numeric_metrics(&from_run.metadata_json);
    let to_metrics = bench_numeric_metrics(&to_run.metadata_json);
    let mut keys = BTreeSet::new();
    keys.extend(from_metrics.keys().cloned());
    keys.extend(to_metrics.keys().cloned());

    let mut comparisons = Vec::new();
    let mut missing = Vec::new();
    for key in keys {
        match (from_metrics.get(&key), to_metrics.get(&key)) {
            (Some(from), Some(to)) => comparisons.push(BenchMetricComparison {
                scenario_id: key.0,
                metric: key.1,
                from_value: *from,
                to_value: *to,
                delta: to - from,
                percent_change: if *from == 0.0 {
                    None
                } else {
                    Some(((to - from) / from) * 100.0)
                },
            }),
            (Some(_), None) => missing.push(BenchMissingMetric {
                scenario_id: key.0,
                metric: key.1,
                missing_from: "to_run".to_string(),
            }),
            (None, Some(_)) => missing.push(BenchMissingMetric {
                scenario_id: key.0,
                metric: key.1,
                missing_from: "from_run".to_string(),
            }),
            (None, None) => {}
        }
    }

    Ok((
        RunsOutput::BenchCompare(BenchCompareOutput {
            command: "bench.compare",
            from_run: from_summary,
            to_run: to_summary,
            comparisons,
            missing,
        }),
        0,
    ))
}

fn require_kind(run: &RunRecord, expected: &str) -> homeboy::core::Result<()> {
    if run.kind == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "run_id",
        format!(
            "run {} is kind '{}', expected '{expected}'",
            run.id, run.kind
        ),
        Some(run.id.clone()),
        None,
    ))
}

pub(crate) fn run_contains_scenario(run: &RunRecord, scenario_id: &str) -> bool {
    if run.metadata_json["selected_scenarios"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(scenario_id)))
    {
        return true;
    }
    bench_numeric_metrics(&run.metadata_json)
        .keys()
        .any(|(scenario, _)| scenario == scenario_id)
}

pub(crate) fn bench_numeric_metrics(metadata: &Value) -> BTreeMap<(String, String), f64> {
    let mut metrics = BTreeMap::new();
    if let Some(scenarios) = metadata["scenario_metrics"].as_array() {
        for scenario in scenarios {
            collect_scenario_metrics(scenario, &mut metrics);
        }
    }
    if metrics.is_empty() {
        if let Some(scenarios) = metadata["results"]["scenarios"].as_array() {
            for scenario in scenarios {
                collect_scenario_metrics(scenario, &mut metrics);
            }
        }
    }
    metrics
}

fn collect_scenario_metrics(scenario: &Value, metrics: &mut BTreeMap<(String, String), f64>) {
    let Some(scenario_id) = scenario["scenario_id"]
        .as_str()
        .or_else(|| scenario["id"].as_str())
    else {
        return;
    };

    collect_numeric_object(scenario_id, None, &scenario["metrics"], metrics);
    if let Some(groups) = scenario["metric_groups"].as_object() {
        for (group, values) in groups {
            collect_numeric_object(scenario_id, Some(group), values, metrics);
        }
    }
}

fn collect_numeric_object(
    scenario_id: &str,
    prefix: Option<&str>,
    value: &Value,
    metrics: &mut BTreeMap<(String, String), f64>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (name, value) in object {
        let Some(number) = value.as_f64() else {
            continue;
        };
        let metric = match prefix {
            Some(prefix) => format!("{prefix}.{name}"),
            None => name.clone(),
        };
        metrics.insert((scenario_id.to_string(), metric), number);
    }
}
