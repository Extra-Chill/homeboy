use std::collections::{BTreeMap, BTreeSet};

use clap::Args;
use serde::Serialize;
use serde_json::Value;

use homeboy::observation::{ObservationStore, RunListFilter, RunRecord};

use crate::commands::CmdResult;

use super::{run_contains_scenario, RunsOutput, DEFAULT_LIMIT};

#[derive(Args, Clone, Default)]
pub struct RunsDistributionArgs {
    /// Run kind: bench, rig, trace, etc.
    #[arg(long)]
    pub kind: Option<String>,
    /// Component ID
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Rig ID
    #[arg(long)]
    pub rig: Option<String>,
    /// Benchmark scenario ID. Only applies to bench metadata.
    #[arg(long = "scenario")]
    pub scenario_id: Option<String>,
    /// Run status
    #[arg(long)]
    pub status: Option<String>,
    /// Dot-separated metadata path to aggregate
    #[arg(long = "field", required = true)]
    pub fields: Vec<String>,
    /// Maximum runs to inspect before scenario filtering
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: i64,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsDistributionOutput {
    pub command: &'static str,
    pub filters: RunsDistributionFilters,
    pub fields: Vec<FieldDistributionOutput>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct RunsDistributionFilters {
    pub kind: Option<String>,
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub scenario_id: Option<String>,
    pub status: Option<String>,
    pub limit: i64,
    pub inspected_run_count: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct FieldDistributionOutput {
    pub field: String,
    pub matched_run_count: usize,
    pub missing_run_count: usize,
    pub total_value_count: usize,
    pub unique_value_count: usize,
    pub values: Vec<CategoryDistributionValue>,
    pub repeated_values: Vec<CategoryDistributionValue>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CategoryDistributionValue {
    pub value: String,
    pub count: usize,
    pub percent: f64,
    pub run_count: usize,
}

pub fn runs_distribution(
    args: RunsDistributionArgs,
    command: &'static str,
) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let limit = args.limit.clamp(1, 1000);
    let runs = store
        .list_runs(RunListFilter {
            kind: args.kind.clone(),
            component_id: args.component_id.clone(),
            status: args.status.clone(),
            rig_id: args.rig.clone(),
            limit: Some(limit),
        })?
        .into_iter()
        .filter(|run| {
            args.scenario_id
                .as_deref()
                .is_none_or(|scenario| run_contains_scenario(run, scenario))
        })
        .collect::<Vec<_>>();

    let fields = args
        .fields
        .iter()
        .map(|field| distribution_for_field(field, &runs))
        .collect();

    Ok((
        RunsOutput::Distribution(RunsDistributionOutput {
            command,
            filters: RunsDistributionFilters {
                kind: args.kind,
                component_id: args.component_id,
                rig_id: args.rig,
                scenario_id: args.scenario_id,
                status: args.status,
                limit,
                inspected_run_count: runs.len(),
            },
            fields,
        }),
        0,
    ))
}

fn distribution_for_field(field: &str, runs: &[RunRecord]) -> FieldDistributionOutput {
    let mut counts = BTreeMap::<String, (usize, BTreeSet<String>)>::new();
    let mut matched_run_count = 0;
    let mut total_value_count = 0;

    for run in runs {
        let values = metadata_path_values(&run.metadata_json, field);
        if values.is_empty() {
            continue;
        }
        matched_run_count += 1;
        total_value_count += values.len();

        for value in values {
            let entry = counts.entry(value).or_insert_with(|| (0, BTreeSet::new()));
            entry.0 += 1;
            entry.1.insert(run.id.clone());
        }
    }

    let mut values = counts
        .into_iter()
        .map(|(value, (count, run_ids))| CategoryDistributionValue {
            value,
            count,
            percent: if total_value_count == 0 {
                0.0
            } else {
                (count as f64 / total_value_count as f64) * 100.0
            },
            run_count: run_ids.len(),
        })
        .collect::<Vec<_>>();
    values.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    let repeated_values = values
        .iter()
        .filter(|value| value.count > 1)
        .cloned()
        .collect::<Vec<_>>();

    FieldDistributionOutput {
        field: field.to_string(),
        matched_run_count,
        missing_run_count: runs.len().saturating_sub(matched_run_count),
        total_value_count,
        unique_value_count: values.len(),
        values,
        repeated_values,
    }
}

fn metadata_path_values(metadata: &Value, field: &str) -> Vec<String> {
    let parts = field
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    collect_metadata_path_values(metadata, &parts, &mut values);
    values
}

fn collect_metadata_path_values(value: &Value, parts: &[&str], values: &mut Vec<String>) {
    if parts.is_empty() {
        match value.as_array() {
            Some(items) => values.extend(items.iter().filter_map(scalar_metadata_label)),
            None => values.extend(scalar_metadata_label(value)),
        }
        return;
    }

    if let Some(items) = value.as_array() {
        for item in items {
            collect_metadata_path_values(item, parts, values);
        }
        return;
    }

    if let Some(next) = value.get(parts[0]) {
        collect_metadata_path_values(next, &parts[1..], values);
    }
}

fn scalar_metadata_label(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_bool().map(|bool_value| bool_value.to_string()))
        .or_else(|| value.as_number().map(ToString::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    use homeboy::observation::{NewRunRecord, RunStatus};
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
    fn distribution_counts_scalar_metadata_values() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            for family in ["serif", "sans", "serif"] {
                let run = store
                    .start_run(sample_run(
                        "bench",
                        "studio",
                        "rig-a",
                        serde_json::json!({ "fingerprint": { "font": family } }),
                    ))
                    .expect("run");
                store
                    .finish_run(&run.id, RunStatus::Pass, None)
                    .expect("finish");
            }

            let output = distribution_output(RunsDistributionArgs {
                kind: Some("bench".to_string()),
                component_id: Some("studio".to_string()),
                fields: vec!["fingerprint.font".to_string()],
                limit: 20,
                ..RunsDistributionArgs::default()
            });

            let field = &output.fields[0];
            assert_eq!(field.matched_run_count, 3);
            assert_eq!(field.missing_run_count, 0);
            assert_eq!(field.total_value_count, 3);
            assert_eq!(field.unique_value_count, 2);
            assert_eq!(field.values[0].value, "serif");
            assert_eq!(field.values[0].count, 2);
            assert_eq!(field.values[0].run_count, 2);
            assert!((field.values[0].percent - (100.0 * 2.0 / 3.0)).abs() < 0.000_000_001);
            assert_eq!(field.repeated_values.len(), 1);
        });
    }

    #[test]
    fn distribution_flattens_array_metadata_values() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            for motifs in [
                serde_json::json!(["grid", "cards"]),
                serde_json::json!(["grid", "hero"]),
            ] {
                let run = store
                    .start_run(sample_run(
                        "bench",
                        "studio",
                        "rig-a",
                        serde_json::json!({ "fingerprint": { "motifs": motifs } }),
                    ))
                    .expect("run");
                store
                    .finish_run(&run.id, RunStatus::Pass, None)
                    .expect("finish");
            }

            let output = distribution_output(RunsDistributionArgs {
                kind: Some("bench".to_string()),
                component_id: Some("studio".to_string()),
                fields: vec!["fingerprint.motifs".to_string()],
                limit: 20,
                ..RunsDistributionArgs::default()
            });

            let field = &output.fields[0];
            assert_eq!(field.matched_run_count, 2);
            assert_eq!(field.total_value_count, 4);
            assert_eq!(field.unique_value_count, 3);
            assert_eq!(field.values[0].value, "grid");
            assert_eq!(field.values[0].count, 2);
            assert_eq!(field.values[0].run_count, 2);
        });
    }

    #[test]
    fn distribution_reports_missing_fields() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            for metadata in [
                serde_json::json!({ "category": "timeout" }),
                serde_json::json!({ "other": "value" }),
            ] {
                let run = store
                    .start_run(sample_run("bench", "studio", "rig-a", metadata))
                    .expect("run");
                store
                    .finish_run(&run.id, RunStatus::Pass, None)
                    .expect("finish");
            }

            let output = distribution_output(RunsDistributionArgs {
                kind: Some("bench".to_string()),
                component_id: Some("studio".to_string()),
                fields: vec!["category".to_string()],
                limit: 20,
                ..RunsDistributionArgs::default()
            });

            let field = &output.fields[0];
            assert_eq!(field.matched_run_count, 1);
            assert_eq!(field.missing_run_count, 1);
            assert_eq!(field.values[0].value, "timeout");
        });
    }

    #[test]
    fn distribution_applies_run_filters_and_scenario_filter() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let matching = store
                .start_run(sample_run(
                    "bench",
                    "studio",
                    "rig-a",
                    serde_json::json!({
                        "category": "kept",
                        "selected_scenarios": ["build"]
                    }),
                ))
                .expect("matching");
            store
                .finish_run(&matching.id, RunStatus::Pass, None)
                .expect("finish matching");
            let wrong_status = store
                .start_run(sample_run(
                    "bench",
                    "studio",
                    "rig-a",
                    serde_json::json!({
                        "category": "failed",
                        "selected_scenarios": ["build"]
                    }),
                ))
                .expect("wrong status");
            store
                .finish_run(&wrong_status.id, RunStatus::Fail, None)
                .expect("finish wrong status");
            let wrong_scenario = store
                .start_run(sample_run(
                    "bench",
                    "studio",
                    "rig-a",
                    serde_json::json!({
                        "category": "other-scenario",
                        "selected_scenarios": ["deploy"]
                    }),
                ))
                .expect("wrong scenario");
            store
                .finish_run(&wrong_scenario.id, RunStatus::Pass, None)
                .expect("finish wrong scenario");
            let wrong_component = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "rig-a",
                    serde_json::json!({
                        "category": "other-component",
                        "selected_scenarios": ["build"]
                    }),
                ))
                .expect("wrong component");
            store
                .finish_run(&wrong_component.id, RunStatus::Pass, None)
                .expect("finish wrong component");

            let output = distribution_output(RunsDistributionArgs {
                kind: Some("bench".to_string()),
                component_id: Some("studio".to_string()),
                rig: Some("rig-a".to_string()),
                scenario_id: Some("build".to_string()),
                status: Some("pass".to_string()),
                fields: vec!["category".to_string()],
                limit: 20,
            });

            let field = &output.fields[0];
            assert_eq!(output.filters.inspected_run_count, 1);
            assert_eq!(field.values.len(), 1);
            assert_eq!(field.values[0].value, "kept");
        });
    }

    #[test]
    fn distribution_traverses_nested_arrays_in_metadata_paths() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            for motifs in [
                serde_json::json!(["terminal_window", "glow_overlay"]),
                serde_json::json!(["terminal_window", "cards_grid"]),
            ] {
                let run = store
                    .start_run(sample_run(
                        "bench",
                        "studio",
                        "rig-a",
                        serde_json::json!({
                            "selected_scenarios": ["site-build"],
                            "scenario_metrics": [{
                                "scenario_id": "site-build",
                                "metadata": { "design": { "motifs": motifs } }
                            }]
                        }),
                    ))
                    .expect("run");
                store
                    .finish_run(&run.id, RunStatus::Pass, None)
                    .expect("finish");
            }

            let output = distribution_output(RunsDistributionArgs {
                kind: Some("bench".to_string()),
                component_id: Some("studio".to_string()),
                scenario_id: Some("site-build".to_string()),
                fields: vec!["scenario_metrics.metadata.design.motifs".to_string()],
                limit: 20,
                ..RunsDistributionArgs::default()
            });

            let field = &output.fields[0];
            assert_eq!(field.matched_run_count, 2);
            assert_eq!(field.total_value_count, 4);
            assert_eq!(field.unique_value_count, 3);
            assert_eq!(field.values[0].value, "terminal_window");
            assert_eq!(field.values[0].count, 2);
            assert_eq!(field.values[0].run_count, 2);
        });
    }

    #[test]
    fn distribution_reports_repeated_categories_by_occurrence() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "studio",
                    "rig-a",
                    serde_json::json!({ "choices": ["sqlite", "sqlite", "mysql"] }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Pass, None)
                .expect("finish");

            let output = distribution_output(RunsDistributionArgs {
                kind: Some("bench".to_string()),
                component_id: Some("studio".to_string()),
                fields: vec!["choices".to_string()],
                limit: 20,
                ..RunsDistributionArgs::default()
            });

            let repeated = &output.fields[0].repeated_values;
            assert_eq!(repeated.len(), 1);
            assert_eq!(repeated[0].value, "sqlite");
            assert_eq!(repeated[0].count, 2);
            assert_eq!(repeated[0].run_count, 1);
        });
    }

    fn distribution_output(args: RunsDistributionArgs) -> RunsDistributionOutput {
        let (output, _) = runs_distribution(args, "runs.distribution").expect("distribution");
        let RunsOutput::Distribution(output) = output else {
            panic!("expected distribution output");
        };
        output
    }
}
