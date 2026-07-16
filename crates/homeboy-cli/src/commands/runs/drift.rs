//! Generic distribution-drift primitive.
//!
//! Asks: "Across the last `--window` of imported runs, did any value of
//! `--metric` exceed `--threshold` of total observations?" Optional
//! `--baseline` compares the window distribution to a longer baseline window.
//!
//! Schema-blind. Statistics only. The caller decides what "drift" means
//! semantically.

use clap::{Args, ValueEnum};
use serde::Serialize;

use homeboy::core::observation::{ObservationStore, RunListFilter};
use homeboy::core::Error;

use super::common::{
    compile_jsonpath, distribution_share, load_artifact_rows, DistributionSnapshot,
};
use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Debug)]
pub struct RunsDriftArgs {
    /// Component ID (matches the synthetic Homeboy run's component_id).
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Run kind (e.g. `gh-actions`).
    #[arg(long)]
    pub kind: Option<String>,
    /// JSONPath expression naming the metric to track.
    /// Example: `--metric '$.theme'` or `--metric '$.fonts[*].family'`.
    #[arg(long, required = true)]
    pub metric: String,
    /// Window duration to evaluate (e.g. 24h, 7d).
    #[arg(long, default_value = "7d")]
    pub window: String,
    /// Share threshold in `0.0..=1.0`. Values whose share of the window
    /// exceeds this threshold are flagged as `dominant=true`. The default
    /// (0.0) reports every value.
    #[arg(long, default_value_t = 0.0)]
    pub threshold: f64,
    /// Optional baseline window (e.g. 30d) compared against `--window`.
    #[arg(long)]
    pub baseline: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = DriftFormat::Json)]
    pub format: DriftFormat,
}

#[derive(ValueEnum, Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DriftFormat {
    Json,
    Table,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsDriftOutput {
    pub command: &'static str,
    pub filters: RunsDriftFilters,
    pub metric: String,
    pub threshold: f64,
    pub window_observations: usize,
    pub window_missing_rows: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_observations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_missing_rows: Option<usize>,
    pub values: Vec<DriftValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsDriftFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub window: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct DriftValue {
    pub value: String,
    pub window_count: usize,
    pub window_share: f64,
    pub dominant: bool,
    /// Baseline share of `value` in the longer baseline window. `None` when
    /// `--baseline` was not specified or `value` was not observed in the
    /// baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_share: Option<f64>,
    /// `window_share - baseline_share` when both are available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_delta: Option<f64>,
}

pub fn runs_drift(args: RunsDriftArgs) -> CmdResult<RunsOutput> {
    if args.metric.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "metric",
            "--metric must not be empty",
            None,
            None,
        ));
    }
    if !(0.0..=1.0).contains(&args.threshold) {
        return Err(Error::validation_invalid_argument(
            "threshold",
            "--threshold must be in the range 0.0..=1.0",
            Some(args.threshold.to_string()),
            None,
        ));
    }

    let metric_path = compile_jsonpath(&args.metric)?;
    let store = ObservationStore::open_initialized()?;

    let filter = RunListFilter {
        kind: args.kind.clone(),
        component_id: args.component_id.clone(),
        status: None,
        rig_id: None,
        limit: Some(5000),
    };

    let window_rows = load_artifact_rows(&store, filter.clone(), Some(&args.window))?.rows;
    let window_snap = distribution_share(&window_rows, &metric_path);

    let baseline_snap = if let Some(window) = args.baseline.as_deref() {
        let rows = load_artifact_rows(&store, filter, Some(window))?.rows;
        Some(distribution_share(&rows, &metric_path))
    } else {
        None
    };

    let values = build_drift_values(&window_snap, baseline_snap.as_ref(), args.threshold);

    let mut output = RunsDriftOutput {
        command: "runs.drift",
        filters: RunsDriftFilters {
            component_id: args.component_id.clone(),
            kind: args.kind.clone(),
            window: args.window.clone(),
            baseline: args.baseline.clone(),
        },
        metric: args.metric.clone(),
        threshold: args.threshold,
        window_observations: window_snap.total_observations,
        window_missing_rows: window_snap.missing_row_count,
        baseline_observations: baseline_snap.as_ref().map(|s| s.total_observations),
        baseline_missing_rows: baseline_snap.as_ref().map(|s| s.missing_row_count),
        values,
        table: None,
    };

    if let DriftFormat::Table = args.format {
        output.table = Some(render_table(&output));
    }

    Ok((RunsOutput::Drift(output), 0))
}

fn build_drift_values(
    window: &DistributionSnapshot,
    baseline: Option<&DistributionSnapshot>,
    threshold: f64,
) -> Vec<DriftValue> {
    window
        .values
        .iter()
        .map(|(value, count, share)| {
            let baseline_share = baseline.and_then(|snap| {
                snap.values
                    .iter()
                    .find(|(v, _, _)| v == value)
                    .map(|(_, _, s)| *s)
            });
            let share_delta = baseline_share.map(|baseline_share| share - baseline_share);
            DriftValue {
                value: value.clone(),
                window_count: *count,
                window_share: *share,
                dominant: *share >= threshold,
                baseline_share,
                share_delta,
            }
        })
        .collect()
}

fn render_table(output: &RunsDriftOutput) -> String {
    let baseline = output.baseline_share_present();
    let mut lines = if baseline {
        vec![
            "value | window_count | window_share | baseline_share | share_delta | dominant"
                .to_string(),
            "---   | ---          | ---          | ---            | ---         | ---".to_string(),
        ]
    } else {
        vec![
            "value | window_count | window_share | dominant".to_string(),
            "---   | ---          | ---          | ---".to_string(),
        ]
    };
    for value in &output.values {
        let line = if baseline {
            format!(
                "{} | {} | {:.4} | {} | {} | {}",
                value.value,
                value.window_count,
                value.window_share,
                value
                    .baseline_share
                    .map(|s| format!("{s:.4}"))
                    .unwrap_or_else(|| "-".into()),
                value
                    .share_delta
                    .map(|s| format!("{s:+.4}"))
                    .unwrap_or_else(|| "-".into()),
                value.dominant
            )
        } else {
            format!(
                "{} | {} | {:.4} | {}",
                value.value, value.window_count, value.window_share, value.dominant
            )
        };
        lines.push(line);
    }
    lines.join("\n")
}

impl RunsDriftOutput {
    fn baseline_share_present(&self) -> bool {
        self.baseline_observations.is_some()
            || self.values.iter().any(|v| v.baseline_share.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(values: Vec<(&str, usize, f64)>, total: usize, missing: usize) -> DistributionSnapshot {
        DistributionSnapshot {
            total_observations: total,
            missing_row_count: missing,
            values: values
                .into_iter()
                .map(|(v, c, s)| (v.to_string(), c, s))
                .collect(),
        }
    }

    #[test]
    fn build_drift_values_marks_dominant_above_threshold() {
        let window = snap(vec![("alpha", 8, 0.8), ("beta", 2, 0.2)], 10, 0);
        let values = build_drift_values(&window, None, 0.5);
        assert!(values.iter().find(|v| v.value == "alpha").unwrap().dominant);
        assert!(!values.iter().find(|v| v.value == "beta").unwrap().dominant);
    }

    #[test]
    fn build_drift_values_attaches_baseline_share_and_delta() {
        let window = snap(vec![("alpha", 8, 0.8), ("beta", 2, 0.2)], 10, 0);
        let baseline = snap(vec![("alpha", 5, 0.5), ("beta", 5, 0.5)], 10, 0);
        let values = build_drift_values(&window, Some(&baseline), 0.6);
        let alpha = values.iter().find(|v| v.value == "alpha").unwrap();
        assert_eq!(alpha.baseline_share, Some(0.5));
        assert!((alpha.share_delta.unwrap() - 0.3).abs() < 1e-9);
        assert!(alpha.dominant);
        let beta = values.iter().find(|v| v.value == "beta").unwrap();
        assert!(!beta.dominant);
        assert_eq!(beta.baseline_share, Some(0.5));
        assert!((beta.share_delta.unwrap() - (-0.3)).abs() < 1e-9);
    }

    #[test]
    fn build_drift_values_handles_value_missing_from_baseline() {
        let window = snap(vec![("alpha", 5, 1.0)], 5, 0);
        let baseline = snap(vec![("beta", 5, 1.0)], 5, 0);
        let values = build_drift_values(&window, Some(&baseline), 0.5);
        let alpha = values.iter().find(|v| v.value == "alpha").unwrap();
        assert_eq!(alpha.baseline_share, None);
        assert_eq!(alpha.share_delta, None);
    }

    #[test]
    fn render_table_changes_columns_when_baseline_present() {
        let window = snap(vec![("alpha", 5, 1.0)], 5, 0);
        let baseline = snap(vec![("alpha", 5, 1.0)], 5, 0);
        let values = build_drift_values(&window, Some(&baseline), 0.5);
        let output = RunsDriftOutput {
            command: "runs.drift",
            filters: RunsDriftFilters {
                component_id: None,
                kind: None,
                window: "7d".into(),
                baseline: Some("30d".into()),
            },
            metric: "$.theme".into(),
            threshold: 0.5,
            window_observations: 5,
            window_missing_rows: 0,
            baseline_observations: Some(5),
            baseline_missing_rows: Some(0),
            values,
            table: None,
        };
        let rendered = render_table(&output);
        assert!(rendered.contains("baseline_share"));
        assert!(rendered.contains("share_delta"));
        assert!(rendered.contains("alpha"));
    }
}
