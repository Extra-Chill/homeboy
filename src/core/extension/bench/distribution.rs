//! Cross-run bench distribution summaries.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchRunDistribution {
    pub n: u64,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub stdev: f64,
    pub cv_pct: f64,
    pub p50: f64,
    pub p95: f64,
}

pub(crate) fn distribution(samples: &[f64]) -> BenchRunDistribution {
    let n = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / n;
    let variance = samples
        .iter()
        .map(|value| {
            let delta = value - mean;
            delta * delta
        })
        .sum::<f64>()
        / n;
    let stdev = variance.sqrt();
    let cv_pct = if mean == 0.0 {
        0.0
    } else {
        stdev / mean * 100.0
    };

    BenchRunDistribution {
        n: samples.len() as u64,
        min: samples.iter().copied().fold(f64::INFINITY, f64::min),
        max: samples.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        mean,
        stdev,
        cv_pct,
        p50: percentile(samples, 50.0),
        p95: percentile(samples, 95.0),
    }
}

pub(crate) fn percentile(samples: &[f64], pct: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = pct / 100.0 * (sorted.len() as f64 - 1.0);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = rank - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

#[cfg(test)]
#[path = "../../../../tests/core/extension/bench/distribution_test.rs"]
mod distribution_test;
