//! Generic bench metric comparison policy.

use homeboy_engine_primitives::metric_comparison::Direction;
use homeboy_engine_primitives::metric_comparison::RegressionTest;
use std::collections::BTreeMap;

use homeboy_engine_primitives::metric_comparison::{MetricDelta, MetricPolicy};

use super::parsing::BenchMetricPolicy;

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
        self.comparison_policy()
            .compare(&self.name, Some(baseline_value), Some(current_value))
    }

    fn comparison_policy(&self) -> MetricPolicy {
        MetricPolicy {
            direction: Some(self.policy.direction),
            percent: self.policy.regression_threshold_percent,
            absolute: self.regression_threshold_absolute,
            zero_baseline_is_neutral: self.zero_baseline_is_neutral,
            signed_percent: true,
            ..Default::default()
        }
    }

    pub(crate) fn compare_distribution(
        &self,
        baseline: &[f64],
        current: &[f64],
    ) -> Option<MetricDelta> {
        // Distribution gates historically treat a zero baseline as comparable.
        let mut policy = self.comparison_policy();
        policy.zero_baseline_is_neutral = false;
        policy.compare_distribution(
            &self.name,
            baseline,
            current,
            self.policy
                .regression_test
                .unwrap_or(RegressionTest::MannWhitneyU),
        )
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

    fn default_p95(default_threshold_percent: f64) -> Self {
        Self {
            name: "p95_ms".to_string(),
            policy: BenchMetricPolicy {
                direction: Direction::LowerIsBetter,
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

/// Resolve the policies a run is compared under. Runner-declared policies
/// win; without them a run that reports `p95_ms` keeps the original p95 gate.
pub(crate) fn resolve_metric_policies(
    metric_policies: &BTreeMap<String, BenchMetricPolicy>,
    reports_p95: bool,
    default_threshold_percent: f64,
) -> Vec<ResolvedMetricPolicy> {
    if metric_policies.is_empty() {
        if !reports_p95 {
            return Vec::new();
        }
        return vec![ResolvedMetricPolicy::default_p95(default_threshold_percent)];
    }

    metric_policies
        .iter()
        .map(|(name, policy)| ResolvedMetricPolicy::custom(name, policy))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::parsing::{BenchMetrics, BenchResults, BenchScenario};
    use super::*;

    fn policy(direction: Direction) -> BenchMetricPolicy {
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
            provenance: Default::default(),
            run_metadata: None,
            metadata: BTreeMap::new(),
            metric_groups: BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: BTreeMap::new(),
            diagnostics: Vec::new(),
            child_command_failures: Vec::new(),
            phase_events: Vec::new(),
            phase_summaries: Vec::new(),
            failure_classification: None,
            responsiveness: None,
            budget_findings: Vec::new(),
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
                metric_groups: BTreeMap::new(),
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                gates: Vec::new(),
                gate_results: Vec::new(),
                metadata: BTreeMap::new(),
                provenance: Default::default(),
                passed: true,
                memory: None,
                artifacts: BTreeMap::new(),
                diagnostics: Vec::new(),
                runs: None,
                runs_summary: None,
            }],
            metric_policies: BTreeMap::new(),
            metric_policy_presets: BTreeMap::new(),
        }
    }

    #[test]
    fn test_compare() {
        let resolved =
            ResolvedMetricPolicy::custom("requests_per_second", &policy(Direction::HigherIsBetter));
        let delta = resolved.compare(100.0, 90.0);

        assert!(delta.regressed());
        assert_eq!(delta.delta, Some(-10.0));
    }

    #[test]
    fn test_compare_distribution() {
        let resolved = ResolvedMetricPolicy::custom(
            "latency_ms",
            &BenchMetricPolicy {
                direction: Direction::LowerIsBetter,
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

        assert_eq!(delta.baseline, Some(110.0));
        assert_eq!(delta.candidate, Some(130.0));
        assert_eq!(delta.regression_test, Some(RegressionTest::PointDelta));
        assert_eq!(delta.baseline_samples, Some(3));
        assert_eq!(delta.current_samples, Some(3));
        assert!(delta.regressed());
    }

    #[test]
    fn test_variance_aware() {
        let resolved = ResolvedMetricPolicy::custom(
            "latency_ms",
            &BenchMetricPolicy {
                direction: Direction::LowerIsBetter,
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
                direction: Direction::LowerIsBetter,
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
        assert!(delta.regressed());
    }

    #[test]
    fn test_resolve_metric_policies() {
        let mut metrics = BTreeMap::new();
        metrics.insert("p95_ms".to_string(), 100.0);

        let results = results(metrics);
        let reports_p95 = results
            .scenarios
            .iter()
            .any(|scenario| scenario.metrics.get("p95_ms").is_some());
        let resolved = resolve_metric_policies(&results.metric_policies, reports_p95, 5.0);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name(), "p95_ms");
    }
}
