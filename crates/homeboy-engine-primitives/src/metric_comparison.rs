//! Shared metric comparison policy for workload, bench and hotspot inputs.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    LowerIsBetter,
    HigherIsBetter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegressionTest {
    PointDelta,
    MannWhitneyU,
    KolmogorovSmirnov,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricChange {
    Regressed,
    Improved,
    Unchanged,
    Added,
    Missing,
}

/// One comparison payload, including missing/report-only metrics and optional
/// distribution evidence. All comparison surfaces render this superset.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricDelta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regression_threshold: Option<f64>,
    pub change: MetricChange,
    pub gated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regression_test: Option<RegressionTest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_samples: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_samples: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistic: Option<f64>,
}

impl MetricDelta {
    pub fn regressed(&self) -> bool {
        self.change == MetricChange::Regressed
    }
    pub fn improved(&self) -> bool {
        self.change == MetricChange::Improved
    }
    pub fn reason(&self, scenario_id: &str) -> String {
        let pct = self
            .percent
            .map(|p| format!(" ({:+.1}%)", p))
            .unwrap_or_default();
        format!(
            "{}: {} {:.2} → {:.2}{}",
            scenario_id,
            self.name,
            self.baseline.unwrap_or_default(),
            self.candidate.unwrap_or_default(),
            pct
        )
    }
}

/// Threshold boundary rules differ by instrument, but are evaluated here only.
#[derive(Debug, Clone, Copy, Default)]
pub enum Boundary {
    #[default]
    Strict,
    Inclusive,
    /// Rate comparisons tolerate one rounding epsilon at the boundary.
    Rate,
}

#[derive(Debug, Clone, Default)]
pub struct MetricPolicy {
    pub direction: Option<Direction>,
    pub percent: Option<f64>,
    pub absolute: f64,
    pub boundary: Boundary,
    pub zero_baseline_is_neutral: bool,
    pub threshold_improvements: bool,
    /// Preserve signed-baseline percentages for the legacy bench renderer.
    pub signed_percent: bool,
}

impl MetricPolicy {
    fn exceeds(&self, value: f64, threshold: f64) -> bool {
        match self.boundary {
            Boundary::Strict => value > threshold,
            Boundary::Inclusive => value >= threshold,
            Boundary::Rate => value + f64::EPSILON >= threshold,
        }
    }

    pub fn compare(
        &self,
        name: &str,
        baseline: Option<f64>,
        candidate: Option<f64>,
    ) -> MetricDelta {
        let delta = baseline.zip(candidate).map(|(b, c)| c - b);
        let percent = baseline
            .zip(delta)
            .filter(|(b, _)| *b != 0.0)
            .map(|(b, d)| d / if self.signed_percent { b } else { b.abs() } * 100.0);
        let change = match (baseline, candidate, self.direction) {
            (None, Some(_), _) => MetricChange::Added,
            (Some(_), None, _) => MetricChange::Missing,
            (Some(b), Some(c), Some(direction)) => {
                let bad = match direction {
                    Direction::LowerIsBetter => c - b,
                    Direction::HigherIsBetter => b - c,
                };
                let beyond = self.exceeds(bad.abs(), self.absolute)
                    && self
                        .percent
                        .map(|t| b == 0.0 || self.exceeds(bad.abs() / b.abs() * 100.0, t))
                        .unwrap_or(true);
                if bad > 0.0 && beyond && !(b == 0.0 && self.zero_baseline_is_neutral) {
                    MetricChange::Regressed
                } else if bad < 0.0 && (!self.threshold_improvements || beyond) {
                    MetricChange::Improved
                } else {
                    MetricChange::Unchanged
                }
            }
            _ => MetricChange::Unchanged,
        };
        MetricDelta {
            name: name.into(),
            baseline,
            candidate,
            delta,
            percent,
            direction: self.direction,
            regression_threshold: self.percent,
            change,
            gated: self.direction.is_some(),
            regression_test: None,
            baseline_samples: None,
            current_samples: None,
            statistic: None,
        }
    }
}

/// The shared aggregate gate result. Presence and sample sufficiency are
/// preconditions supplied by the instrument, not inferred from a numeric delta.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub regressions: Vec<String>,
    pub advisories: Vec<String>,
    pub improvements: Vec<String>,
}
impl Verdict {
    pub fn regressed(&self) -> bool {
        !self.regressions.is_empty()
    }
    pub fn improved(&self) -> bool {
        !self.improvements.is_empty()
    }
    pub fn status(&self) -> &'static str {
        if self.regressed() {
            "worse"
        } else if self.improved() {
            "better"
        } else {
            "same"
        }
    }
}

/// Rendering of cohort evidence; distinct from the aggregate comparison verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortStatus {
    Regressed,
    Clean,
    InsufficientSamples,
}
impl Verdict {
    pub fn cohort_status(&self, enough_samples: bool) -> CohortStatus {
        if self.regressed() {
            CohortStatus::Regressed
        } else if enough_samples {
            CohortStatus::Clean
        } else {
            CohortStatus::InsufficientSamples
        }
    }
}

impl MetricPolicy {
    pub fn compare_distribution(
        &self,
        name: &str,
        baseline: &[f64],
        current: &[f64],
        test: RegressionTest,
    ) -> Option<MetricDelta> {
        if baseline.is_empty() || current.is_empty() {
            return None;
        }
        let mut result = self.compare(name, Some(median(baseline)), Some(median(current)));
        let direction = self.direction?;
        let statistic = match test {
            RegressionTest::PointDelta => None,
            RegressionTest::MannWhitneyU => {
                Some(mann_whitney_worse_z(baseline, current, direction))
            }
            RegressionTest::KolmogorovSmirnov => {
                Some(kolmogorov_smirnov_worse_d(baseline, current, direction))
            }
        };
        let significant = match test {
            RegressionTest::PointDelta => true,
            RegressionTest::MannWhitneyU => statistic.is_some_and(|z| z > 1.645),
            RegressionTest::KolmogorovSmirnov => statistic.is_some_and(|d| {
                d > kolmogorov_smirnov_critical_value(baseline.len(), current.len())
            }),
        };
        if result.regressed() && !significant {
            result.change = MetricChange::Unchanged;
        }
        result.regression_test = Some(test);
        result.baseline_samples = Some(baseline.len());
        result.current_samples = Some(current.len());
        result.statistic = statistic;
        Some(result)
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

fn worse_scale(value: f64, direction: Direction) -> f64 {
    match direction {
        Direction::LowerIsBetter => value,
        Direction::HigherIsBetter => -value,
    }
}

fn mann_whitney_worse_z(
    baseline_samples: &[f64],
    current_samples: &[f64],
    direction: Direction,
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
    direction: Direction,
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
