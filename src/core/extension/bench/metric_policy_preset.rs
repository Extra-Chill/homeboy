use serde::{Deserialize, Serialize};

use crate::core::error::{Error, Result};

use super::gate::{BenchGate, BenchGateOp};
use super::parsing::{
    BenchMetricDirection, BenchMetricPhase, BenchMetricPolicy, BenchResults, RegressionTest,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchMetricPolicyPreset {
    pub preset: BenchMetricPolicyPresetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold_absolute: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub variance_aware: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_iterations_for_variance: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_test: Option<RegressionTest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<BenchMetricPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchMetricPolicyPresetKind {
    LatencyRegression,
    MemoryRegression,
    ColdWarmDelta,
    FlakeNoiseThreshold,
    AbsoluteBudget,
    MinCoverage,
    MaxFailureRate,
    MaxBlockedRate,
    MaxCriticalFindings,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn expand_metric_policy_presets(results: &mut BenchResults) -> Result<()> {
    for (metric, preset) in results.metric_policy_presets.clone() {
        match preset.preset {
            BenchMetricPolicyPresetKind::LatencyRegression
            | BenchMetricPolicyPresetKind::ColdWarmDelta
            | BenchMetricPolicyPresetKind::FlakeNoiseThreshold => {
                results
                    .metric_policies
                    .entry(metric)
                    .or_insert_with(|| preset.to_policy(BenchMetricDirection::LowerIsBetter, 5.0));
            }
            BenchMetricPolicyPresetKind::MemoryRegression => {
                results
                    .metric_policies
                    .entry(metric)
                    .or_insert_with(|| preset.to_policy(BenchMetricDirection::LowerIsBetter, 10.0));
            }
            BenchMetricPolicyPresetKind::AbsoluteBudget => {
                expand_absolute_budget_preset(results, &metric, &preset)?;
            }
            BenchMetricPolicyPresetKind::MinCoverage => {
                expand_thresholded_policy_preset(
                    results,
                    &metric,
                    &preset,
                    BenchMetricDirection::HigherIsBetter,
                    ThresholdBound::Min,
                )?;
            }
            BenchMetricPolicyPresetKind::MaxFailureRate
            | BenchMetricPolicyPresetKind::MaxBlockedRate
            | BenchMetricPolicyPresetKind::MaxCriticalFindings => {
                expand_thresholded_policy_preset(
                    results,
                    &metric,
                    &preset,
                    BenchMetricDirection::LowerIsBetter,
                    ThresholdBound::Max,
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ThresholdBound {
    Min,
    Max,
}

fn expand_thresholded_policy_preset(
    results: &mut BenchResults,
    metric: &str,
    preset: &BenchMetricPolicyPreset,
    direction: BenchMetricDirection,
    bound: ThresholdBound,
) -> Result<()> {
    results
        .metric_policies
        .entry(metric.to_string())
        .or_insert_with(|| preset.to_policy(direction, 0.0));

    match bound {
        ThresholdBound::Min if preset.min.is_some() => {
            expand_absolute_budget_preset(results, metric, preset)
        }
        ThresholdBound::Max if preset.max.is_some() => {
            expand_absolute_budget_preset(results, metric, preset)
        }
        ThresholdBound::Min if preset.max.is_some() => {
            Err(invalid_bound_error(metric, "min", "max"))
        }
        ThresholdBound::Max if preset.min.is_some() => {
            Err(invalid_bound_error(metric, "max", "min"))
        }
        _ => Ok(()),
    }
}

fn invalid_bound_error(metric: &str, expected: &str, got: &str) -> Error {
    Error::validation_invalid_argument(
        "metric_policy_presets",
        format!(
            "outcome preset for `{}` must declare `{}` threshold, not `{}`",
            metric, expected, got
        ),
        None,
        None,
    )
}

fn expand_absolute_budget_preset(
    results: &mut BenchResults,
    metric: &str,
    preset: &BenchMetricPolicyPreset,
) -> Result<()> {
    let (op, value) = absolute_budget_gate(metric, preset)?;
    for scenario in &mut results.scenarios {
        if scenario.metrics.get(metric).is_some()
            && !scenario.gates.iter().any(|gate| gate.metric == metric)
        {
            scenario.gates.push(BenchGate {
                metric: metric.to_string(),
                op,
                value,
            });
        }
    }
    Ok(())
}

fn absolute_budget_gate(
    metric: &str,
    preset: &BenchMetricPolicyPreset,
) -> Result<(BenchGateOp, f64)> {
    match (preset.max, preset.min) {
        (Some(_), Some(_)) => Err(Error::validation_invalid_argument(
            "metric_policy_presets",
            format!(
                "absolute budget preset for `{}` must declare either max or min, not both",
                metric
            ),
            None,
            None,
        )),
        (Some(max), None) => Ok((BenchGateOp::Lte, max)),
        (None, Some(min)) => Ok((BenchGateOp::Gte, min)),
        (None, None) => Err(Error::validation_invalid_argument(
            "metric_policy_presets",
            format!(
                "absolute budget preset for `{}` must declare max or min",
                metric
            ),
            None,
            None,
        )),
    }
}

impl BenchMetricPolicyPreset {
    fn to_policy(
        &self,
        direction: BenchMetricDirection,
        default_threshold_percent: f64,
    ) -> BenchMetricPolicy {
        BenchMetricPolicy {
            direction,
            regression_threshold_percent: Some(
                self.regression_threshold_percent
                    .unwrap_or(default_threshold_percent),
            ),
            regression_threshold_absolute: self.regression_threshold_absolute,
            variance_aware: self.variance_aware,
            min_iterations_for_variance: self.min_iterations_for_variance,
            regression_test: self.regression_test,
            phase: self.phase,
        }
    }
}
