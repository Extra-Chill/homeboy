//! Pure bench metric-policy-preset contract types.

use serde::{Deserialize, Serialize};

use crate::{BenchMetricDirection, BenchMetricPhase, BenchMetricPolicy, RegressionTest};

fn is_false(value: &bool) -> bool {
    !*value
}

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

impl BenchMetricPolicyPreset {
    pub fn to_policy(
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
