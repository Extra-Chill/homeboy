use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PerformanceMetricPoint {
    pub subject_id: String,
    pub metric: String,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PerformanceMetricFamilyHotspot {
    pub family: String,
    pub total: f64,
    pub metric_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PerformanceHotspotSummary {
    pub slowest_timing_metrics: Vec<PerformanceMetricPoint>,
    pub hottest_metric_families: Vec<PerformanceMetricFamilyHotspot>,
}

pub fn summarize_performance_hotspots(
    points: &[PerformanceMetricPoint],
    timing_limit: usize,
    family_limit: usize,
) -> PerformanceHotspotSummary {
    PerformanceHotspotSummary {
        slowest_timing_metrics: top_slowest_metrics(points, timing_limit),
        hottest_metric_families: top_metric_families(points, family_limit),
    }
}

fn top_slowest_metrics(
    points: &[PerformanceMetricPoint],
    limit: usize,
) -> Vec<PerformanceMetricPoint> {
    let mut timing = points
        .iter()
        .filter(|point| is_timing_metric(&point.metric))
        .cloned()
        .collect::<Vec<_>>();
    timing.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.subject_id.cmp(&b.subject_id))
            .then_with(|| a.metric.cmp(&b.metric))
    });
    timing.truncate(limit);
    timing
}

fn top_metric_families(
    points: &[PerformanceMetricPoint],
    limit: usize,
) -> Vec<PerformanceMetricFamilyHotspot> {
    let mut totals: BTreeMap<String, f64> = BTreeMap::new();
    let mut metric_counts: HashMap<String, usize> = HashMap::new();
    for point in points
        .iter()
        .filter(|point| is_family_metric(&point.metric))
    {
        let family = metric_family(&point.metric);
        *totals.entry(family.clone()).or_default() += point.value;
        *metric_counts.entry(family).or_default() += 1;
    }

    let mut families = totals
        .into_iter()
        .map(|(family, total)| PerformanceMetricFamilyHotspot {
            metric_count: metric_counts.get(&family).copied().unwrap_or(0),
            family,
            total,
        })
        .collect::<Vec<_>>();
    families.sort_by(|a, b| {
        b.total
            .total_cmp(&a.total)
            .then_with(|| a.family.cmp(&b.family))
    });
    families.truncate(limit);
    families
}

fn is_timing_metric(metric: &str) -> bool {
    metric == "duration"
        || metric == "elapsed"
        || metric.ends_with("_duration")
        || metric.ends_with("_elapsed")
        || metric.ends_with("_ms")
        || metric.contains("_ms_")
        || metric.ends_with(".ms")
        || metric.contains(".ms_")
}

fn is_family_metric(metric: &str) -> bool {
    let normalized = metric.to_ascii_lowercase();
    normalized.contains("query")
        || normalized.contains("queries")
        || normalized.ends_with("_count")
        || normalized.ends_with(".count")
}

fn metric_family(metric: &str) -> String {
    if let Some((group, _)) = metric.split_once('.') {
        return group.to_string();
    }

    for suffix in [
        "_queries_per_item",
        "_queries_per_run",
        "_queries_per_sec",
        "_query_count",
        "_queries",
        "_count",
        "_ms_per_item",
        "_ms_per_run",
        "_ms",
    ] {
        if let Some(prefix) = metric.strip_suffix(suffix) {
            if !prefix.is_empty() {
                return prefix.to_string();
            }
        }
    }

    metric.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(subject_id: &str, metric: &str, value: f64) -> PerformanceMetricPoint {
        PerformanceMetricPoint {
            subject_id: subject_id.to_string(),
            metric: metric.to_string(),
            value,
        }
    }

    #[test]
    fn summarizes_schema_blind_timing_metrics_and_metric_families() {
        let summary = summarize_performance_hotspots(
            &[
                point("fast-path", "create_ms_per_item", 125.0),
                point("fast-path", "create_queries_per_item", 9.0),
                point("fast-path", "query_families.select_count", 14.0),
                point("fast-path", "rows_count", 3.0),
                point("slow-path", "create_ms_per_item", 980.0),
                point("slow-path", "create_queries_per_item", 27.0),
                point("slow-path", "query_families.select_count", 44.0),
                point("slow-path", "validation_ms", 40.0),
            ],
            2,
            2,
        );

        assert_eq!(
            summary.slowest_timing_metrics,
            vec![
                point("slow-path", "create_ms_per_item", 980.0),
                point("fast-path", "create_ms_per_item", 125.0),
            ]
        );
        assert_eq!(summary.hottest_metric_families[0].family, "query_families");
        assert_eq!(summary.hottest_metric_families[0].total, 58.0);
        assert_eq!(summary.hottest_metric_families[0].metric_count, 2);
        assert_eq!(summary.hottest_metric_families[1].family, "create");
        assert_eq!(summary.hottest_metric_families[1].total, 36.0);
        assert_eq!(summary.hottest_metric_families[1].metric_count, 2);
    }

    #[test]
    fn uses_deterministic_tie_breakers() {
        let summary = summarize_performance_hotspots(
            &[
                point("beta", "duration_ms", 10.0),
                point("alpha", "z_duration", 10.0),
                point("alpha", "a_duration", 10.0),
                point("zeta", "z_count", 5.0),
                point("alpha", "a_count", 5.0),
            ],
            3,
            2,
        );

        assert_eq!(
            summary.slowest_timing_metrics,
            vec![
                point("alpha", "a_duration", 10.0),
                point("alpha", "z_duration", 10.0),
                point("beta", "duration_ms", 10.0),
            ]
        );
        assert_eq!(summary.hottest_metric_families[0].family, "a");
        assert_eq!(summary.hottest_metric_families[1].family, "z");
    }
}
