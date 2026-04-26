//! Generic bench metric comparison policy.

use serde::Serialize;

use super::parsing::{BenchMetricDirection, BenchMetricPolicy, BenchResults};

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
            regression,
            improvement: bad_delta < 0.0,
        }
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
                phase: None,
            },
            regression_threshold_absolute: 0.0,
            zero_baseline_is_neutral: true,
        }
    }
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
                iterations: 10,
                metrics: BenchMetrics { values: metrics },
                memory: None,
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
    fn test_custom() {
        let resolved = ResolvedMetricPolicy::custom(
            "error_rate",
            &BenchMetricPolicy {
                direction: BenchMetricDirection::LowerIsBetter,
                regression_threshold_percent: None,
                regression_threshold_absolute: Some(0.01),
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
