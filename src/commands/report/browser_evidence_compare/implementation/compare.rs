use std::collections::{BTreeMap, BTreeSet};

use super::super::types::{
    ArtifactComparison, ArtifactRef, AssertionComparison, AssertionStats, BrowserEvidenceVariant,
    BrowserEvidenceVariantComparison, MetricComparison, MetricStats,
};
use super::BrowserEvidenceSample;

pub(in crate::commands::report::browser_evidence_compare) fn compare_variants(
    baseline: &[BrowserEvidenceSample],
    candidate: &[BrowserEvidenceSample],
) -> Vec<BrowserEvidenceVariantComparison> {
    let mut keys = BTreeSet::new();
    for sample in baseline.iter().chain(candidate.iter()) {
        keys.insert(variant_for_sample(sample));
    }

    keys.into_iter()
        .map(|variant| {
            let baseline_samples = baseline
                .iter()
                .filter(|sample| variant_for_sample(sample) == variant)
                .collect::<Vec<_>>();
            let candidate_samples = candidate
                .iter()
                .filter(|sample| variant_for_sample(sample) == variant)
                .collect::<Vec<_>>();
            compare_variant(variant, &baseline_samples, &candidate_samples)
        })
        .collect()
}

fn compare_variant(
    variant: BrowserEvidenceVariant,
    baseline: &[&BrowserEvidenceSample],
    candidate: &[&BrowserEvidenceSample],
) -> BrowserEvidenceVariantComparison {
    let baseline_assertions = assertion_sum(baseline);
    let candidate_assertions = assertion_sum(candidate);
    let notes = baseline
        .iter()
        .chain(candidate.iter())
        .flat_map(|sample| sample.notes.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    BrowserEvidenceVariantComparison {
        variant,
        baseline_repeats: baseline.len(),
        candidate_repeats: candidate.len(),
        assertions: AssertionComparison {
            pass_delta: candidate_assertions.passed as i64 - baseline_assertions.passed as i64,
            fail_delta: candidate_assertions.failed as i64 - baseline_assertions.failed as i64,
            baseline: baseline_assertions,
            candidate: candidate_assertions,
        },
        request_totals: compare_metric_values(
            &baseline
                .iter()
                .filter_map(|sample| sample.request_total)
                .collect::<Vec<_>>(),
            &candidate
                .iter()
                .filter_map(|sample| sample.request_total)
                .collect::<Vec<_>>(),
        ),
        request_by_host: compare_metric_maps(baseline, candidate, |sample| &sample.request_by_host),
        request_by_type: compare_metric_maps(baseline, candidate, |sample| &sample.request_by_type),
        browser_metrics: compare_metric_maps(baseline, candidate, |sample| &sample.browser_metrics),
        lifecycle_metrics: compare_metric_maps(baseline, candidate, |sample| {
            &sample.lifecycle_metrics
        }),
        console_errors: compare_metric_values(
            &baseline
                .iter()
                .filter_map(|sample| sample.console_errors)
                .collect::<Vec<_>>(),
            &candidate
                .iter()
                .filter_map(|sample| sample.console_errors)
                .collect::<Vec<_>>(),
        ),
        page_errors: compare_metric_values(
            &baseline
                .iter()
                .filter_map(|sample| sample.page_errors)
                .collect::<Vec<_>>(),
            &candidate
                .iter()
                .filter_map(|sample| sample.page_errors)
                .collect::<Vec<_>>(),
        ),
        artifacts: ArtifactComparison {
            baseline: artifact_refs(baseline),
            candidate: artifact_refs(candidate),
        },
        visual_compare: None,
        notes,
    }
}

fn variant_for_sample(sample: &BrowserEvidenceSample) -> BrowserEvidenceVariant {
    BrowserEvidenceVariant {
        scenario: sample
            .scenario
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        profile: sample
            .profile
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        matrix: sample.matrix.clone(),
    }
}

fn artifact_refs(samples: &[&BrowserEvidenceSample]) -> Vec<ArtifactRef> {
    samples
        .iter()
        .flat_map(|sample| {
            let mut artifacts = sample.artifacts.iter().cloned().collect::<Vec<_>>();
            if let Some(source) = &sample.source_artifact {
                artifacts.push(source.clone());
            }
            artifacts
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn assertion_sum(samples: &[&BrowserEvidenceSample]) -> AssertionStats {
    samples
        .iter()
        .fold(AssertionStats::default(), |mut acc, sample| {
            acc.total += sample.assertions.total;
            acc.passed += sample.assertions.passed;
            acc.failed += sample.assertions.failed;
            acc.skipped += sample.assertions.skipped;
            acc.advisory_failed += sample.assertions.advisory_failed;
            acc.failed_advisory_assertions
                .extend(sample.assertions.failed_advisory_assertions.clone());
            acc
        })
}

fn compare_metric_maps(
    baseline: &[&BrowserEvidenceSample],
    candidate: &[&BrowserEvidenceSample],
    map: fn(&BrowserEvidenceSample) -> &BTreeMap<String, f64>,
) -> BTreeMap<String, MetricComparison> {
    let keys = baseline
        .iter()
        .chain(candidate.iter())
        .flat_map(|sample| map(sample).keys().cloned())
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .map(|key| {
            let baseline_values = baseline
                .iter()
                .filter_map(|sample| map(sample).get(&key).copied())
                .collect::<Vec<_>>();
            let candidate_values = candidate
                .iter()
                .filter_map(|sample| map(sample).get(&key).copied())
                .collect::<Vec<_>>();
            (
                key,
                compare_metric_values(&baseline_values, &candidate_values),
            )
        })
        .collect()
}

fn compare_metric_values(baseline: &[f64], candidate: &[f64]) -> MetricComparison {
    let baseline_stats = metric_stats(baseline);
    let candidate_stats = metric_stats(candidate);
    let median_delta = baseline_stats
        .as_ref()
        .zip(candidate_stats.as_ref())
        .map(|(baseline, candidate)| candidate.median - baseline.median);
    let median_delta_pct = baseline_stats
        .as_ref()
        .zip(candidate_stats.as_ref())
        .and_then(|(baseline, candidate)| {
            (baseline.median.abs() > f64::EPSILON)
                .then(|| ((candidate.median - baseline.median) / baseline.median) * 100.0)
        });
    MetricComparison {
        baseline: baseline_stats,
        candidate: candidate_stats,
        median_delta,
        median_delta_pct,
    }
}

fn metric_stats(values: &[f64]) -> Option<MetricStats> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Some(MetricStats {
        n: sorted.len(),
        min: sorted[0],
        median: median(&sorted),
        max: *sorted.last().unwrap_or(&sorted[0]),
    })
}

fn median(sorted: &[f64]) -> f64 {
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}
