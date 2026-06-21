use std::collections::{BTreeMap, BTreeSet};

use homeboy::core::observation::{ObservationStore, RunListFilter, RunRecord};
use homeboy::core::Error;
use serde_json::Value;

use crate::commands::escape_markdown_table_cell;

use super::{require_run, run_detail, run_summary, CmdResult, RunDetail, RunSummary, RunsOutput};

mod types;
pub use types::*;

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

pub fn bench_compare(
    from_run_id: &str,
    to_run_id: &str,
    metrics: &[String],
) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let from_run = require_run(&store, from_run_id)?;
    let to_run = require_run(&store, to_run_id)?;
    require_kind(&from_run, "bench")?;
    require_kind(&to_run, "bench")?;
    require_comparable_bench_runs(&from_run, &to_run)?;

    let baseline = BenchComparisonSide {
        label: "baseline",
        component_state: component_state(&from_run),
        run: run_summary(from_run.clone()),
    };
    let candidate = BenchComparisonSide {
        label: "candidate",
        component_state: component_state(&to_run),
        run: run_summary(to_run.clone()),
    };
    let shared = shared_context(&from_run.metadata_json);
    let from_metrics = bench_numeric_metrics(&from_run.metadata_json);
    let to_metrics = bench_numeric_metrics(&to_run.metadata_json);
    let mut keys = BTreeSet::new();
    keys.extend(from_metrics.keys().cloned());
    keys.extend(to_metrics.keys().cloned());
    if !metrics.is_empty() {
        keys.retain(|(_, metric)| metrics.iter().any(|requested| requested == metric));
    }

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

    let reports = BenchCompareReports {
        markdown: render_bench_compare_markdown(
            &baseline,
            &candidate,
            &shared,
            &comparisons,
            &missing,
        ),
    };

    Ok((
        RunsOutput::BenchCompare(BenchCompareOutput {
            command: "bench.compare",
            baseline,
            candidate,
            shared,
            comparisons,
            missing,
            reports,
        }),
        0,
    ))
}

fn require_comparable_bench_runs(
    from_run: &RunRecord,
    to_run: &RunRecord,
) -> homeboy::core::Result<()> {
    let from = comparison_contract(&from_run.metadata_json);
    let to = comparison_contract(&to_run.metadata_json);
    let mut mismatches = Vec::new();
    if from.settings != to.settings {
        mismatches.push("settings".to_string());
    }
    if from.selected_scenarios != to.selected_scenarios {
        mismatches.push("selected_scenarios".to_string());
    }
    if from.workloads != to.workloads {
        mismatches.push("workloads".to_string());
    }
    if from.iterations != to.iterations {
        mismatches.push("iterations".to_string());
    }
    if from.runs != to.runs {
        mismatches.push("runs".to_string());
    }
    if from.concurrency != to.concurrency {
        mismatches.push("concurrency".to_string());
    }

    if mismatches.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "run_id",
        format!(
            "bench runs are not comparable; mismatched shared benchmark context: {}",
            mismatches.join(", ")
        ),
        Some(format!("{}..{}", from_run.id, to_run.id)),
        None,
    ))
}

#[derive(Debug, Clone, PartialEq)]
struct BenchComparisonContract {
    settings: BTreeMap<String, Value>,
    selected_scenarios: Vec<String>,
    workloads: Vec<BenchWorkloadFingerprint>,
    iterations: Option<u64>,
    runs: Option<u64>,
    concurrency: Option<u64>,
}

fn comparison_contract(metadata: &Value) -> BenchComparisonContract {
    let shared = shared_context(metadata);
    BenchComparisonContract {
        settings: shared.settings,
        selected_scenarios: shared.selected_scenarios,
        workloads: shared.workloads,
        iterations: shared.iterations,
        runs: shared.runs,
        concurrency: shared.concurrency,
    }
}

fn shared_context(metadata: &Value) -> BenchComparisonSharedContext {
    let run_metadata = bench_run_metadata(metadata);
    BenchComparisonSharedContext {
        settings: bench_settings(metadata),
        selected_scenarios: string_array(
            run_metadata.and_then(|value| value.get("selected_scenarios")),
        )
        .or_else(|| string_array(metadata.get("selected_scenarios")))
        .unwrap_or_default(),
        workloads: bench_workloads(run_metadata),
        provenance: bench_provenance(metadata, run_metadata),
        iterations: run_metadata
            .and_then(|value| value.get("iterations"))
            .and_then(Value::as_u64)
            .or_else(|| metadata.get("iterations").and_then(Value::as_u64)),
        runs: run_metadata
            .and_then(|value| value.get("runs"))
            .and_then(Value::as_u64),
        concurrency: run_metadata
            .and_then(|value| value.get("concurrency"))
            .and_then(Value::as_u64),
        shared_state: run_metadata
            .and_then(|value| value.get("shared_state"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn bench_provenance(metadata: &Value, run_metadata: Option<&Value>) -> Vec<BenchProvenanceEntry> {
    let mut entries = Vec::new();
    if let Some(entry) =
        provenance_entry("run", None, run_metadata.and_then(|v| v.get("provenance")))
    {
        entries.push(entry);
    }
    if let Some(entry) = provenance_entry("run", None, metadata.get("provenance")) {
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    if let Some(workloads) = run_metadata
        .and_then(|value| value.get("workloads"))
        .and_then(Value::as_array)
    {
        for workload in workloads {
            let scenario_id = workload
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(entry) =
                provenance_entry("workload", scenario_id, workload.get("provenance"))
            {
                entries.push(entry);
            }
        }
    }
    if let Some(scenarios) = metadata
        .get("results")
        .and_then(|results| results.get("scenarios"))
        .and_then(Value::as_array)
    {
        for scenario in scenarios {
            let scenario_id = scenario
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(entry) =
                provenance_entry("scenario", scenario_id, scenario.get("provenance"))
            {
                if !entries.contains(&entry) {
                    entries.push(entry);
                }
            }
        }
    }
    entries
}

fn provenance_entry(
    scope: &str,
    scenario_id: Option<String>,
    value: Option<&Value>,
) -> Option<BenchProvenanceEntry> {
    let value = value?;
    let labels = string_array(value.get("labels")).unwrap_or_default();
    let links = value
        .get("links")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(BenchProvenanceLinkEntry {
                        url: item.get("url")?.as_str()?.to_string(),
                        label: item
                            .get("label")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        source: item
                            .get("source")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        privacy: item
                            .get("privacy")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (!labels.is_empty() || !links.is_empty()).then(|| BenchProvenanceEntry {
        scope: scope.to_string(),
        scenario_id,
        links,
        labels,
    })
}

fn bench_run_metadata(metadata: &Value) -> Option<&Value> {
    metadata
        .get("run_metadata")
        .or_else(|| {
            metadata
                .get("results")
                .and_then(|results| results.get("run_metadata"))
        })
        .or_else(|| {
            metadata
                .get("payload")
                .and_then(|payload| payload.get("results"))
                .and_then(|results| results.get("run_metadata"))
        })
}

fn bench_settings(metadata: &Value) -> BTreeMap<String, Value> {
    [
        metadata.get("settings"),
        metadata.get("bench_settings"),
        metadata
            .get("run_metadata")
            .and_then(|value| value.get("settings")),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| value.as_object())
    .map(|object| {
        object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    })
    .unwrap_or_default()
}

fn bench_workloads(run_metadata: Option<&Value>) -> Vec<BenchWorkloadFingerprint> {
    let mut workloads = run_metadata
        .and_then(|value| value.get("workloads"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id")?.as_str()?.to_string();
                    let sha256 = item
                        .get("sha256")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    Some(BenchWorkloadFingerprint { id, sha256 })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    workloads.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.sha256.cmp(&b.sha256)));
    workloads
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value.and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    })
}

fn component_state(run: &RunRecord) -> BenchComponentState {
    BenchComponentState {
        component_id: run.component_id.clone(),
        rig_id: run.rig_id.clone(),
        git_sha: run.git_sha.clone(),
        cwd: run.cwd.clone(),
        component_path: run
            .metadata_json
            .get("component_path")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn render_bench_compare_markdown(
    baseline: &BenchComparisonSide,
    candidate: &BenchComparisonSide,
    shared: &BenchComparisonSharedContext,
    comparisons: &[BenchMetricComparison],
    missing: &[BenchMissingMetric],
) -> String {
    let mut out = String::new();
    out.push_str("# Bench Compare\n\n");
    out.push_str(&format!("- **Baseline run:** `{}`\n", baseline.run.id));
    out.push_str(&format!("- **Candidate run:** `{}`\n", candidate.run.id));
    if let Some(component_id) = &baseline.component_state.component_id {
        out.push_str(&format!("- **Component:** `{component_id}`\n"));
    }
    if let Some(rig_id) = &baseline.component_state.rig_id {
        out.push_str(&format!("- **Rig:** `{rig_id}`\n"));
    }
    if !shared.selected_scenarios.is_empty() {
        out.push_str(&format!(
            "- **Scenarios:** `{}`\n",
            shared.selected_scenarios.join(", ")
        ));
    }

    if !shared.provenance.is_empty() {
        out.push_str("\n## Provenance\n\n");
        for entry in &shared.provenance {
            let scope = match &entry.scenario_id {
                Some(scenario_id) => format!("{} `{}`", entry.scope, scenario_id),
                None => entry.scope.clone(),
            };
            for link in &entry.links {
                let label = link
                    .label
                    .as_deref()
                    .or(link.source.as_deref())
                    .unwrap_or("reference");
                let mut suffix = Vec::new();
                if let Some(source) = &link.source {
                    suffix.push(format!("source: {source}"));
                }
                if let Some(privacy) = &link.privacy {
                    suffix.push(format!("privacy: {privacy}"));
                }
                let suffix = if suffix.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", suffix.join(", "))
                };
                out.push_str(&format!(
                    "- **{}:** [{}]({}){}\n",
                    scope, label, link.url, suffix
                ));
            }
            for label in &entry.labels {
                out.push_str(&format!("- **{}:** {}\n", scope, label));
            }
        }
    }

    out.push_str("\n| Scenario | Metric | Baseline | Candidate | Delta | Change |\n");
    out.push_str("| --- | --- | ---: | ---: | ---: | ---: |\n");
    for row in comparisons {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            escape_markdown_table_cell(&row.scenario_id),
            escape_markdown_table_cell(&row.metric),
            format_number(row.from_value),
            format_number(row.to_value),
            format_number(row.delta),
            row.percent_change
                .map(|value| format!("{}%", format_number(value)))
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }
    if comparisons.is_empty() {
        out.push_str("| n/a | n/a | n/a | n/a | n/a | n/a |\n");
    }

    if !missing.is_empty() {
        out.push_str("\n## Missing Metrics\n\n");
        out.push_str("| Scenario | Metric | Missing From |\n");
        out.push_str("| --- | --- | --- |\n");
        for row in missing {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                escape_markdown_table_cell(&row.scenario_id),
                escape_markdown_table_cell(&row.metric),
                row.missing_from
            ));
        }
    }

    out
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.3}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::observation::NewRunRecord;
    use homeboy::test_support::with_isolated_home;

    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
        NewRunRecord::builder(kind)
            .component_id(component_id)
            .command(format!("homeboy {kind} {component_id}"))
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .git_sha(Some("abc123".to_string()))
            .rig_id(rig_id)
            .metadata(metadata)
            .build()
    }

    #[test]
    fn bench_compare_reports_deltas_and_missing_metrics() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let from = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "settings": { "profile": "cold" },
                        "run_metadata": {
                            "selected_scenarios": ["cold"],
                            "iterations": 3,
                            "runs": 1,
                            "concurrency": 1,
                            "shared_state": "/tmp/homeboy-bench-compare",
                            "provenance": {
                                "labels": ["source: zendesk"],
                                "links": [{
                                    "url": "https://automattic.zendesk.com/agent/tickets/9426116",
                                    "label": "Zendesk ticket 9426116",
                                    "source": "zendesk",
                                    "privacy": "internal"
                                }]
                            },
                            "workloads": [{
                                "id": "cold",
                                "sha256": "abc",
                                "provenance": {
                                    "labels": ["scenario: shortcode checkout place-order latency"],
                                    "links": [{
                                        "url": "https://wordpress.org/support/topic/checkout-is-very-slow/",
                                        "label": "WordPress.org support thread",
                                        "source": "wordpress.org"
                                    }]
                                }
                            }]
                        },
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 100.0, "only_from": 1.0 },
                            "metric_groups": { "warm": { "mean_ms": 50.0 } }
                        }]
                    }),
                ))
                .expect("from");
            let to = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "settings": { "profile": "cold" },
                        "run_metadata": {
                            "selected_scenarios": ["cold"],
                            "iterations": 3,
                            "runs": 1,
                            "concurrency": 1,
                            "shared_state": "/tmp/homeboy-bench-compare-candidate",
                            "workloads": [{ "id": "cold", "sha256": "abc" }]
                        },
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 125.0, "only_to": 2.0 },
                            "metric_groups": { "warm": { "mean_ms": 40.0 } }
                        }]
                    }),
                ))
                .expect("to");

            let (output, _) = bench_compare(&from.id, &to.id, &[]).expect("compare");
            let RunsOutput::BenchCompare(output) = output else {
                panic!("expected compare output");
            };
            assert_eq!(output.baseline.run.id, from.id);
            assert_eq!(output.candidate.run.id, to.id);
            assert_eq!(output.shared.selected_scenarios, vec!["cold".to_string()]);
            assert_eq!(output.shared.workloads[0].sha256.as_deref(), Some("abc"));
            assert_eq!(output.shared.provenance.len(), 2);
            assert_eq!(
                output.shared.provenance[0].links[0].url,
                "https://automattic.zendesk.com/agent/tickets/9426116"
            );
            let p95 = output
                .comparisons
                .iter()
                .find(|row| row.metric == "p95_ms")
                .expect("p95 row");
            assert_eq!(p95.delta, 25.0);
            assert_eq!(p95.percent_change, Some(25.0));
            assert!(output
                .comparisons
                .iter()
                .any(|row| row.metric == "warm.mean_ms" && row.delta == -10.0));
            assert!(output
                .missing
                .iter()
                .any(|row| row.metric == "only_from" && row.missing_from == "to_run"));
            assert!(output
                .missing
                .iter()
                .any(|row| row.metric == "only_to" && row.missing_from == "from_run"));
            assert!(output.reports.markdown.contains("# Bench Compare"));
            assert!(output
                .reports
                .markdown
                .contains("| cold | p95_ms | 100 | 125 | 25 | 25% |"));
            assert!(output.reports.markdown.contains("## Provenance"));
            assert!(output.reports.markdown.contains(
                "[Zendesk ticket 9426116](https://automattic.zendesk.com/agent/tickets/9426116)"
            ));
            assert!(output.reports.markdown.contains(
                "[WordPress.org support thread](https://wordpress.org/support/topic/checkout-is-very-slow/)"
            ));

            let (filtered, _) =
                bench_compare(&from.id, &to.id, &["p95_ms".to_string()]).expect("filtered compare");
            let RunsOutput::BenchCompare(filtered) = filtered else {
                panic!("expected filtered compare output");
            };
            assert_eq!(filtered.comparisons.len(), 1);
            assert_eq!(filtered.comparisons[0].metric, "p95_ms");
            assert!(filtered.missing.is_empty());
        });
    }

    #[test]
    fn bench_compare_rejects_mismatched_shared_context() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let from = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "settings": { "profile": "cold" },
                        "run_metadata": {
                            "selected_scenarios": ["cold"],
                            "iterations": 3,
                            "runs": 1,
                            "concurrency": 1,
                            "workloads": [{ "id": "cold", "sha256": "abc" }]
                        },
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 100.0 }
                        }]
                    }),
                ))
                .expect("from");
            let to = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "settings": { "profile": "warm" },
                        "run_metadata": {
                            "selected_scenarios": ["cold"],
                            "iterations": 3,
                            "runs": 1,
                            "concurrency": 1,
                            "workloads": [{ "id": "cold", "sha256": "def" }]
                        },
                        "scenario_metrics": [{
                            "scenario_id": "cold",
                            "metrics": { "p95_ms": 125.0 }
                        }]
                    }),
                ))
                .expect("to");

            let err = bench_compare(&from.id, &to.id, &[])
                .err()
                .expect("compare should reject mismatched context");
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("settings"));
            assert!(err.message.contains("workloads"));
        });
    }
}
