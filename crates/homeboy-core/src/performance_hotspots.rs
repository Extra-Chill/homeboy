use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA: &str = "homeboy/performance-hotspots-summary/v1";
pub const PERFORMANCE_HOTSPOTS_SUMMARY_VERSION: u32 = 1;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceHotspotsSummaryContract {
    #[serde(default = "performance_hotspots_summary_schema")]
    pub schema: String,
    #[serde(default = "performance_hotspots_summary_version")]
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<PerformanceHotspotDimension>,
    pub aggregation: PerformanceHotspotAggregation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rankings: Vec<PerformanceHotspotRanking>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl PerformanceHotspotsSummaryContract {
    pub fn from_value(value: Value) -> Result<Self, String> {
        let mut summary: Self = serde_json::from_value(value).map_err(|err| err.to_string())?;
        summary.normalize()?;
        Ok(summary)
    }

    pub fn normalize(&mut self) -> Result<(), String> {
        self.schema = trim_or_default(&self.schema, PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA);
        if self.schema != PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA {
            return Err(format!(
                "performance hotspots summary schema must be {PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA}"
            ));
        }
        if self.version != PERFORMANCE_HOTSPOTS_SUMMARY_VERSION {
            return Err(format!(
                "performance hotspots summary version must be {PERFORMANCE_HOTSPOTS_SUMMARY_VERSION}"
            ));
        }
        self.id = required_trimmed("performance_hotspots_summary.id", &self.id)?;
        self.label = normalize_optional_string(self.label.take());
        for dimension in &mut self.dimensions {
            dimension.normalize("summary")?;
        }
        self.aggregation.normalize()?;
        for ranking in &mut self.rankings {
            ranking.normalize()?;
        }
        self.source_artifact_refs =
            normalize_string_vec(std::mem::take(&mut self.source_artifact_refs));
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceHotspotAggregation {
    pub method: String,
    pub score_metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_unit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl PerformanceHotspotAggregation {
    fn normalize(&mut self) -> Result<(), String> {
        self.method = required_trimmed("performance_hotspot_aggregation.method", &self.method)?;
        self.score_metric = required_trimmed(
            "performance_hotspot_aggregation.score_metric",
            &self.score_metric,
        )?;
        self.score_unit = normalize_optional_string(self.score_unit.take());
        self.dimensions = normalize_string_vec(std::mem::take(&mut self.dimensions));
        self.metrics = normalize_string_vec(std::mem::take(&mut self.metrics));
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceHotspotRanking {
    pub id: String,
    pub rank: u64,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<PerformanceHotspotDimension>,
    pub primary_metric: PerformanceHotspotMetric,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<PerformanceHotspotMetric>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl PerformanceHotspotRanking {
    fn normalize(&mut self) -> Result<(), String> {
        self.id = required_trimmed("performance_hotspot_ranking.id", &self.id)?;
        if self.rank == 0 {
            return Err(format!(
                "performance_hotspot_ranking.rank must be greater than zero for `{}`",
                self.id
            ));
        }
        if !self.score.is_finite() {
            return Err(format!(
                "performance_hotspot_ranking.score must be finite for `{}`",
                self.id
            ));
        }
        if let Some(relative_score) = self.relative_score {
            if !relative_score.is_finite() {
                return Err(format!(
                    "performance_hotspot_ranking.relative_score must be finite for `{}`",
                    self.id
                ));
            }
        }
        self.label = normalize_optional_string(self.label.take());
        for dimension in &mut self.dimensions {
            dimension.normalize(&self.id)?;
        }
        self.primary_metric.normalize(&self.id)?;
        for metric in &mut self.metrics {
            metric.normalize(&self.id)?;
        }
        self.source_artifact_refs =
            normalize_string_vec(std::mem::take(&mut self.source_artifact_refs));
        self.source_refs = normalize_string_vec(std::mem::take(&mut self.source_refs));
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceHotspotDimension {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl PerformanceHotspotDimension {
    fn normalize(&mut self, ranking_id: &str) -> Result<(), String> {
        self.name = required_trimmed("performance_hotspot_dimension.name", &self.name)
            .map_err(|err| format!("{err} for `{ranking_id}`"))?;
        self.value = required_trimmed("performance_hotspot_dimension.value", &self.value)
            .map_err(|err| format!("{err} for `{ranking_id}`"))?;
        self.kind = normalize_optional_string(self.kind.take());
        self.label = normalize_optional_string(self.label.take());
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PerformanceHotspotMetric {
    pub name: String,
    pub value: f64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl PerformanceHotspotMetric {
    fn normalize(&mut self, ranking_id: &str) -> Result<(), String> {
        self.name = required_trimmed("performance_hotspot_metric.name", &self.name)
            .map_err(|err| format!("{err} for `{ranking_id}`"))?;
        if !self.value.is_finite() {
            return Err(format!(
                "performance_hotspot_metric.value must be finite for `{ranking_id}`"
            ));
        }
        self.unit = required_trimmed("performance_hotspot_metric.unit", &self.unit)
            .map_err(|err| format!("{err} for `{ranking_id}`"))?;
        self.aggregation = normalize_optional_string(self.aggregation.take());
        if let Some(rank) = self.rank {
            if rank == 0 {
                return Err(format!(
                    "performance_hotspot_metric.rank must be greater than zero for `{ranking_id}`"
                ));
            }
        }
        if let Some(relative_score) = self.relative_score {
            if !relative_score.is_finite() {
                return Err(format!(
                    "performance_hotspot_metric.relative_score must be finite for `{ranking_id}`"
                ));
            }
        }
        Ok(())
    }
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

fn performance_hotspots_summary_schema() -> String {
    PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA.to_string()
}

fn performance_hotspots_summary_version() -> u32 {
    PERFORMANCE_HOTSPOTS_SUMMARY_VERSION
}

fn trim_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn required_trimmed(field: &str, value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_string_vec(values: Vec<String>) -> Vec<String> {
    let mut normalized = values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
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
    fn parses_generic_performance_hotspot_summary_contract() {
        let summary = PerformanceHotspotsSummaryContract::from_value(serde_json::json!({
            "schema": PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA,
            "version": PERFORMANCE_HOTSPOTS_SUMMARY_VERSION,
            "id": "component-hotspots",
            "label": "Component hotspots",
            "dimensions": [
                { "name": "component", "value": "example-component", "kind": "component" }
            ],
            "aggregation": {
                "method": "sum_by_dimension",
                "score_metric": "duration_ms",
                "score_unit": "ms",
                "dimensions": ["scenario", "operation"],
                "metrics": ["duration_ms", "query_count"],
                "limit": 20,
                "source_count": 2,
                "metadata": { "window": "candidate" }
            },
            "rankings": [
                {
                    "id": "scenario:checkout",
                    "rank": 1,
                    "score": 481.5,
                    "relative_score": 1.0,
                    "label": "Checkout",
                    "dimensions": [
                        { "name": "scenario", "value": "checkout" },
                        { "name": "operation", "value": "submit" },
                        { "name": "product_dimension", "value": "runtime-owned-label" }
                    ],
                    "primary_metric": {
                        "name": "duration_ms",
                        "value": 481.5,
                        "unit": "ms",
                        "aggregation": "p95",
                        "sample_count": 144,
                        "rank": 1,
                        "relative_score": 1.0
                    },
                    "metrics": [
                        { "name": "query_count", "value": 27, "unit": "count", "aggregation": "sum" }
                    ],
                    "source_artifact_refs": ["bench-summary.json", "bench-summary.json"],
                    "source_refs": ["scenario:checkout", ""]
                }
            ],
            "source_artifact_refs": ["bench-summary.json"],
            "metadata": { "producer": "rig" }
        }))
        .expect("valid generic hotspot summary");

        assert_eq!(summary.schema, PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA);
        assert_eq!(summary.rankings[0].dimensions[2].name, "product_dimension");
        assert_eq!(
            summary.rankings[0].primary_metric.aggregation.as_deref(),
            Some("p95")
        );
        assert_eq!(
            summary.rankings[0].source_artifact_refs,
            vec!["bench-summary.json"]
        );
        assert_eq!(summary.rankings[0].source_refs, vec!["scenario:checkout"]);
    }

    #[test]
    fn rejects_invalid_performance_hotspot_summary_values() {
        let invalid_score = serde_json::json!({
            "schema": PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA,
            "id": "component-hotspots",
            "aggregation": {
                "method": "sum_by_dimension",
                "score_metric": "duration_ms"
            },
            "rankings": [
                {
                    "id": "scenario:checkout",
                    "rank": 1,
                    "score": "slow",
                    "primary_metric": { "name": "duration_ms", "value": 481.5, "unit": "ms" }
                }
            ]
        });
        assert!(PerformanceHotspotsSummaryContract::from_value(invalid_score).is_err());

        let invalid_rank = serde_json::json!({
            "schema": PERFORMANCE_HOTSPOTS_SUMMARY_SCHEMA,
            "id": "component-hotspots",
            "aggregation": {
                "method": "sum_by_dimension",
                "score_metric": "duration_ms"
            },
            "rankings": [
                {
                    "id": "scenario:checkout",
                    "rank": 0,
                    "score": 481.5,
                    "primary_metric": { "name": "duration_ms", "value": 481.5, "unit": "ms" }
                }
            ]
        });
        assert!(PerformanceHotspotsSummaryContract::from_value(invalid_rank).is_err());
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
