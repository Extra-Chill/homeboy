//! Generic bench metric comparison policy.

use serde::Serialize;

use super::parsing::{BenchMetricDirection, BenchMetricPolicy, BenchResults, RegressionTest};

/// Per-metric delta vs baseline. Extensions can opt into comparing any
/// numeric metric by declaring a policy in the bench results JSON.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricDelta {
    pub name: String,
    pub baseline_value: f64,
    pub current_value: f64,
    /// Current minus baseline. Positive is not always bad — consult
    /// `direction` to know whether larger or smaller is better.
    pub delta: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_pct: Option<f64>,
    pub direction: BenchMetricDirection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regression_test: Option<RegressionTest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_samples: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_samples: Option<usize>,
    /// Test statistic for distribution comparisons: z-score for
    /// Mann-Whitney U, D statistic for Kolmogorov-Smirnov.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistic: Option<f64>,
    pub regression: bool,
    pub improvement: bool,
}

impl MetricDelta {
    pub(crate) fn reason(&self, scenario_id: &str) -> String {
        let pct = self
            .delta_pct
            .map(|p| format!(" ({:+.1}%)", p))
            .unwrap_or_default();
        format!(
            "{}: {} {:.2} → {:.2}{}",
            scenario_id, self.name, self.baseline_value, self.current_value, pct
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedMetricPolicy {
    name: String,
    policy: BenchMetricPolicy,
    regression_threshold_absolute: f64,
    zero_baseline_is_neutral: bool,
}

impl ResolvedMetricPolicy {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn compare(&self, baseline_value: f64, current_value: f64) -> MetricDelta {
        let delta = current_value - baseline_value;
        let bad_delta = match self.policy.direction {
            BenchMetricDirection::LowerIsBetter => delta,
            BenchMetricDirection::HigherIsBetter => -delta,
        };
        let delta_pct = if baseline_value != 0.0 {
            Some((delta / baseline_value) * 100.0)
        } else {
            None
        };
        let bad_delta_pct = if baseline_value != 0.0 {
            Some((bad_delta / baseline_value.abs()) * 100.0)
        } else {
            None
        };

        let worse = bad_delta > 0.0;
        let regression = if self.zero_baseline_is_neutral && baseline_value == 0.0 {
            false
        } else {
            let exceeds_absolute = bad_delta > self.regression_threshold_absolute;
            let exceeds_percent = match self.policy.regression_threshold_percent {
                Some(threshold) => bad_delta_pct.map(|pct| pct > threshold).unwrap_or(worse),
                None => true,
            };
            worse && exceeds_absolute && exceeds_percent
        };

        MetricDelta {
            name: self.name.clone(),
            baseline_value,
            current_value,
            delta,
            delta_pct,
            direction: self.policy.direction,
            regression_test: None,
            baseline_samples: None,
            current_samples: None,
            statistic: None,
            regression,
            improvement: bad_delta < 0.0,
        }
    }

    pub(crate) fn compare_distribution(
        &self,
        baseline_samples: &[f64],
        current_samples: &[f64],
    ) -> Option<MetricDelta> {
        if baseline_samples.is_empty() || current_samples.is_empty() {
            return None;
        }

        let baseline_value = median(baseline_samples);
        let current_value = median(current_samples);
        let delta = current_value - baseline_value;
        let bad_delta = match self.policy.direction {
            BenchMetricDirection::LowerIsBetter => delta,
            BenchMetricDirection::HigherIsBetter => -delta,
        };
        let delta_pct = if baseline_value != 0.0 {
            Some((delta / baseline_value) * 100.0)
        } else {
            None
        };
        let bad_delta_pct = if baseline_value != 0.0 {
            Some((bad_delta / baseline_value.abs()) * 100.0)
        } else {
            None
        };

        let worse = bad_delta > 0.0;
        let exceeds_absolute = bad_delta > self.regression_threshold_absolute;
        let exceeds_percent = match self.policy.regression_threshold_percent {
            Some(threshold) => bad_delta_pct.map(|pct| pct > threshold).unwrap_or(worse),
            None => true,
        };
        let regression_test = self
            .policy
            .regression_test
            .unwrap_or(RegressionTest::MannWhitneyU);
        let statistic = match regression_test {
            RegressionTest::PointDelta => None,
            RegressionTest::MannWhitneyU => Some(mann_whitney_worse_z(
                baseline_samples,
                current_samples,
                self.policy.direction,
            )),
            RegressionTest::KolmogorovSmirnov => Some(kolmogorov_smirnov_worse_d(
                baseline_samples,
                current_samples,
                self.policy.direction,
            )),
        };
        let significant = match regression_test {
            RegressionTest::PointDelta => true,
            // One-sided 95% normal approximation.
            RegressionTest::MannWhitneyU => statistic.map(|z| z > 1.645).unwrap_or(false),
            RegressionTest::KolmogorovSmirnov => statistic
                .map(|d| {
                    d > kolmogorov_smirnov_critical_value(
                        baseline_samples.len(),
                        current_samples.len(),
                    )
                })
                .unwrap_or(false),
        };

        Some(MetricDelta {
            name: self.name.clone(),
            baseline_value,
            current_value,
            delta,
            delta_pct,
            direction: self.policy.direction,
            regression_test: Some(regression_test),
            baseline_samples: Some(baseline_samples.len()),
            current_samples: Some(current_samples.len()),
            statistic,
            regression: worse && exceeds_absolute && exceeds_percent && significant,
            improvement: bad_delta < 0.0,
        })
    }

    pub(crate) fn variance_aware(&self) -> bool {
        self.policy.variance_aware
    }

    fn custom(name: &str, policy: &BenchMetricPolicy) -> Self {
        Self {
            name: name.to_string(),
            policy: policy.clone(),
            regression_threshold_absolute: policy.regression_threshold_absolute.unwrap_or(0.0),
            zero_baseline_is_neutral: false,
        }
    }

    fn legacy_p95(default_threshold_percent: f64) -> Self {
        Self {
            name: "p95_ms".to_string(),
            policy: BenchMetricPolicy {
                direction: BenchMetricDirection::LowerIsBetter,
                regression_threshold_percent: Some(default_threshold_percent),
                regression_threshold_absolute: Some(0.0),
                variance_aware: false,
                min_iterations_for_variance: None,
                regression_test: None,
                phase: None,
            },
            regression_threshold_absolute: 0.0,
            zero_baseline_is_neutral: true,
        }
    }
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

fn worse_scale(value: f64, direction: BenchMetricDirection) -> f64 {
    match direction {
        BenchMetricDirection::LowerIsBetter => value,
        BenchMetricDirection::HigherIsBetter => -value,
    }
}

fn mann_whitney_worse_z(
    baseline_samples: &[f64],
    current_samples: &[f64],
    direction: BenchMetricDirection,
) -> f64 {
    let mut u = 0.0;
    for current in current_samples {
        let current = worse_scale(*current, direction);
        for baseline in baseline_samples {
            let baseline = worse_scale(*baseline, direction);
            if current > baseline {
                u += 1.0;
            } else if current == baseline {
                u += 0.5;
            }
        }
    }
    let n1 = baseline_samples.len() as f64;
    let n2 = current_samples.len() as f64;
    let mean = n1 * n2 / 2.0;
    let variance = n1 * n2 * (n1 + n2 + 1.0) / 12.0;
    if variance == 0.0 {
        0.0
    } else {
        (u - mean) / variance.sqrt()
    }
}

fn kolmogorov_smirnov_worse_d(
    baseline_samples: &[f64],
    current_samples: &[f64],
    direction: BenchMetricDirection,
) -> f64 {
    let mut points: Vec<f64> = baseline_samples
        .iter()
        .chain(current_samples.iter())
        .map(|value| worse_scale(*value, direction))
        .collect();
    points.sort_by(|a, b| a.total_cmp(b));
    points.dedup_by(|a, b| a == b);

    let n1 = baseline_samples.len() as f64;
    let n2 = current_samples.len() as f64;
    let baseline: Vec<f64> = baseline_samples
        .iter()
        .map(|value| worse_scale(*value, direction))
        .collect();
    let current: Vec<f64> = current_samples
        .iter()
        .map(|value| worse_scale(*value, direction))
        .collect();

    points.into_iter().fold(0.0, |max_d, point| {
        let f_baseline = baseline.iter().filter(|value| **value <= point).count() as f64 / n1;
        let f_current = current.iter().filter(|value| **value <= point).count() as f64 / n2;
        max_d.max(f_baseline - f_current)
    })
}

fn kolmogorov_smirnov_critical_value(n1: usize, n2: usize) -> f64 {
    1.36 * (((n1 + n2) as f64) / ((n1 * n2) as f64)).sqrt()
}

pub(crate) fn resolve_metric_policies(
    current: &BenchResults,
    default_threshold_percent: f64,
) -> Vec<ResolvedMetricPolicy> {
    if current.metric_policies.is_empty() {
        let has_p95 = current
            .scenarios
            .iter()
            .any(|scenario| scenario.metrics.get("p95_ms").is_some());
        if !has_p95 {
            return Vec::new();
        }
        return vec![ResolvedMetricPolicy::legacy_p95(default_threshold_percent)];
    }

    current
        .metric_policies
        .iter()
        .map(|(name, policy)| ResolvedMetricPolicy::custom(name, policy))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::parsing::{BenchMetrics, BenchScenario};
    use super::*;

    fn policy(direction: BenchMetricDirection) -> BenchMetricPolicy {
        BenchMetricPolicy {
            direction,
            regression_threshold_percent: Some(5.0),
            regression_threshold_absolute: None,
            variance_aware: false,
            min_iterations_for_variance: None,
            regression_test: None,
            phase: None,
        }
    }

    fn results(metrics: BTreeMap<String, f64>) -> BenchResults {
        BenchResults {
            component_id: "demo".to_string(),
            iterations: 10,
            scenarios: vec![BenchScenario {
                id: "scenario".to_string(),
                file: None,
                source: None,
                default_iterations: None,
                tags: Vec::new(),
                iterations: 10,
                metrics: BenchMetrics {
                    values: metrics,
                    distributions: BTreeMap::new(),
                },
                memory: None,
                runs: None,
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
        }
    }

    #[test]
    fn test_reason() {
        let delta = MetricDelta {
            name: "error_rate".to_string(),
            baseline_value: 0.01,
            current_value: 0.02,
            delta: 0.01,
            delta_pct: Some(100.0),
            direction: BenchMetricDirection::LowerIsBetter,
            regression_test: None,
            baseline_samples: None,
            current_samples: None,
            statistic: None,
            regression: true,
            improvement: false,
        };

        assert_eq!(
            delta.reason("http"),
            "http: error_rate 0.01 → 0.02 (+100.0%)"
        );
    }

    #[test]
    fn test_name() {
        let resolved = ResolvedMetricPolicy::legacy_p95(5.0);

        assert_eq!(resolved.name(), "p95_ms");
    }

    #[test]
    fn test_compare() {
        let resolved = ResolvedMetricPolicy::custom(
            "requests_per_second",
            &policy(BenchMetricDirection::HigherIsBetter),
        );
        let delta = resolved.compare(100.0, 90.0);

        assert!(delta.regression);
        assert_eq!(delta.delta, -10.0);
    }

    #[test]
    fn test_compare_distribution() {
        let resolved = ResolvedMetricPolicy::custom(
            "latency_ms",
            &BenchMetricPolicy {
                direction: BenchMetricDirection::LowerIsBetter,
                regression_threshold_percent: Some(5.0),
                regression_threshold_absolute: Some(0.0),
                variance_aware: true,
                min_iterations_for_variance: Some(3),
                regression_test: Some(RegressionTest::PointDelta),
                phase: None,
            },
        );

        let delta = resolved
            .compare_distribution(&[100.0, 110.0, 120.0], &[120.0, 130.0, 140.0])
            .expect("distribution delta");

        assert_eq!(delta.baseline_value, 110.0);
        assert_eq!(delta.current_value, 130.0);
        assert_eq!(delta.regression_test, Some(RegressionTest::PointDelta));
        assert_eq!(delta.baseline_samples, Some(3));
        assert_eq!(delta.current_samples, Some(3));
        assert!(delta.regression);
    }

    #[test]
    fn test_variance_aware() {
        let resolved = ResolvedMetricPolicy::custom(
            "latency_ms",
            &BenchMetricPolicy {
                direction: BenchMetricDirection::LowerIsBetter,
                regression_threshold_percent: None,
                regression_threshold_absolute: None,
                variance_aware: true,
                min_iterations_for_variance: Some(3),
                regression_test: Some(RegressionTest::MannWhitneyU),
                phase: None,
            },
        );

        assert!(resolved.variance_aware());
    }

    #[test]
    fn test_custom() {
        let resolved = ResolvedMetricPolicy::custom(
            "error_rate",
            &BenchMetricPolicy {
                direction: BenchMetricDirection::LowerIsBetter,
                regression_threshold_percent: None,
                regression_threshold_absolute: Some(0.01),
                variance_aware: false,
                min_iterations_for_variance: None,
                regression_test: None,
                phase: None,
            },
        );
        let delta = resolved.compare(0.0, 0.02);

        assert_eq!(resolved.name(), "error_rate");
        assert!(delta.regression);
    }

    #[test]
    fn test_legacy_p95() {
        let resolved = ResolvedMetricPolicy::legacy_p95(5.0);
        let delta = resolved.compare(0.0, 10.0);

        assert_eq!(resolved.name(), "p95_ms");
        assert!(!delta.regression);
    }

    #[test]
    fn test_resolve_metric_policies() {
        let mut metrics = BTreeMap::new();
        metrics.insert("p95_ms".to_string(), 100.0);

        let resolved = resolve_metric_policies(&results(metrics), 5.0);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name(), "p95_ms");
    }
}
