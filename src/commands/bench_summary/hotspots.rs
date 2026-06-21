use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::commands::summary_json::{string_value, value_at};

use super::format_metric;

/// Render generic bench hotspots from either a `BenchCommandOutput` payload
/// (`results.scenarios`) or a persisted run metadata object
/// (`scenario_metrics`). The extractor is intentionally schema-blind: it ranks
/// numeric timing/query/count metrics by name patterns instead of knowing any
/// product-specific scenario names.
pub(crate) fn bench_hotspot_lines(output: &Value) -> Vec<String> {
    let metrics = collect_bench_metric_points(output);
    if metrics.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let slowest = top_slowest_metrics(&metrics, 5);
    let families = top_metric_families(&metrics, 5);

    if slowest.is_empty() && families.is_empty() {
        return Vec::new();
    }

    lines.push("Hotspots:".to_string());
    if !slowest.is_empty() {
        lines.push("  Slowest timing metrics:".to_string());
        for point in slowest {
            lines.push(format!(
                "    {} {}={}{}",
                point.scenario_id,
                point.metric,
                format_metric(point.value),
                point.failure_context.annotation()
            ));
        }
    }
    if !families.is_empty() {
        lines.push("  Hottest metric families:".to_string());
        for family in families {
            lines.push(format!(
                "    {} total={} metrics={}",
                family.0,
                format_metric(family.1),
                family.2
            ));
        }
    }
    lines.extend(bench_failure_context_lines(&metrics));
    lines
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ScenarioFailureDetails {
    success_rate_zero: bool,
    http_error_count: u64,
    request_error_count: u64,
    status_counts: BTreeMap<String, u64>,
    fatal_signatures: Vec<String>,
}

impl ScenarioFailureDetails {
    fn is_failure(&self) -> bool {
        self.success_rate_zero
            || self.http_error_count > 0
            || self.request_error_count > 0
            || !self.status_counts.is_empty()
            || !self.fatal_signatures.is_empty()
    }

    fn annotation(&self) -> String {
        if self.is_failure() {
            format!(" [failed: {}]", self.summary())
        } else {
            String::new()
        }
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.success_rate_zero {
            parts.push("success_rate=0".to_string());
        }
        if self.http_error_count > 0 {
            parts.push(format!("http_errors={}", self.http_error_count));
        }
        if self.request_error_count > 0 {
            parts.push(format!("request_errors={}", self.request_error_count));
        }
        if !self.status_counts.is_empty() {
            let statuses = self
                .status_counts
                .iter()
                .map(|(status, count)| format!("{status}:{count}"))
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("statuses={statuses}"));
        }
        if let Some(signature) = self.fatal_signatures.first() {
            parts.push(format!("fatal={signature}"));
        }
        parts.join(" ")
    }
}

#[derive(Clone, Debug)]
struct BenchMetricPoint {
    scenario_id: String,
    metric: String,
    value: f64,
    failure_context: ScenarioFailureDetails,
}

fn collect_bench_metric_points(output: &Value) -> Vec<BenchMetricPoint> {
    let scenarios = value_at(output, &["results", "scenarios"])
        .and_then(Value::as_array)
        .or_else(|| value_at(output, &["scenario_metrics"]).and_then(Value::as_array))
        .or_else(|| value_at(output, &["metadata", "scenario_metrics"]).and_then(Value::as_array));
    let Some(scenarios) = scenarios else {
        return Vec::new();
    };

    let mut points = Vec::new();
    for scenario in scenarios {
        let Some(scenario_id) =
            string_value(scenario, &["scenario_id"]).or_else(|| string_value(scenario, &["id"]))
        else {
            continue;
        };
        let failure_context = scenario_failure_context(output, scenario_id, scenario);
        collect_numeric_metric_points(
            scenario_id,
            None,
            &scenario["metrics"],
            &failure_context,
            &mut points,
        );
        if let Some(groups) = scenario["metric_groups"].as_object() {
            for (group, values) in groups {
                collect_numeric_metric_points(
                    scenario_id,
                    Some(group),
                    values,
                    &failure_context,
                    &mut points,
                );
            }
        }
    }
    points
}

fn collect_numeric_metric_points(
    scenario_id: &str,
    group: Option<&str>,
    value: &Value,
    failure_context: &ScenarioFailureDetails,
    points: &mut Vec<BenchMetricPoint>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (name, value) in object {
        let Some(number) = value.as_f64() else {
            continue;
        };
        let metric = match group {
            Some(group) => format!("{group}.{name}"),
            None => name.clone(),
        };
        points.push(BenchMetricPoint {
            scenario_id: scenario_id.to_string(),
            metric,
            value: number,
            failure_context: failure_context.clone(),
        });
    }
}

fn top_slowest_metrics(points: &[BenchMetricPoint], limit: usize) -> Vec<BenchMetricPoint> {
    let mut timing = points
        .iter()
        .filter(|point| is_timing_metric(&point.metric))
        .cloned()
        .collect::<Vec<_>>();
    timing.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.scenario_id.cmp(&b.scenario_id))
            .then_with(|| a.metric.cmp(&b.metric))
    });
    timing.truncate(limit);
    timing
}

fn top_metric_families(points: &[BenchMetricPoint], limit: usize) -> Vec<(String, f64, usize)> {
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
        .map(|(family, total)| {
            (
                family.clone(),
                total,
                metric_counts.get(&family).copied().unwrap_or(0),
            )
        })
        .collect::<Vec<_>>();
    families.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    families.truncate(limit);
    families
}

fn bench_failure_context_lines(points: &[BenchMetricPoint]) -> Vec<String> {
    let mut scenarios = BTreeMap::<String, ScenarioFailureDetails>::new();
    for point in points {
        if point.failure_context.is_failure() {
            scenarios
                .entry(point.scenario_id.clone())
                .or_insert_with(|| point.failure_context.clone());
        }
    }
    if scenarios.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["  Failure context:".to_string()];
    for (scenario_id, context) in scenarios {
        lines.push(format!("    {scenario_id}: {}", context.summary()));
    }
    lines
}

fn scenario_failure_context(
    output: &Value,
    scenario_id: &str,
    scenario: &Value,
) -> ScenarioFailureDetails {
    let mut context = ScenarioFailureDetails::default();
    collect_failure_context_from_value(scenario, &mut context);
    collect_artifact_fatal_signatures(output, scenario_id, &mut context);
    context.fatal_signatures.sort();
    context.fatal_signatures.dedup();
    context
}

fn collect_failure_context_from_value(value: &Value, context: &mut ScenarioFailureDetails) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let normalized = key.to_ascii_lowercase();
                if normalized == "success_rate" && child.as_f64() == Some(0.0) {
                    context.success_rate_zero = true;
                } else if normalized == "http_error_count" {
                    context.http_error_count += numeric_count(child);
                } else if normalized == "request_error_count" {
                    context.request_error_count += numeric_count(child);
                } else if is_status_count_object_key(&normalized) {
                    collect_status_count_object(child, context);
                } else if let Some(status) = status_count_key(&normalized) {
                    add_status_count(context, status, numeric_count(child));
                } else if normalized == "fatal_signature" || normalized == "fatal_signatures" {
                    collect_signature_value(child, context);
                }
                collect_failure_context_from_value(child, context);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_failure_context_from_value(child, context);
            }
        }
        _ => {}
    }
}

fn collect_artifact_fatal_signatures(
    output: &Value,
    scenario_id: &str,
    context: &mut ScenarioFailureDetails,
) {
    for artifacts_path in [&["artifacts"][..], &["metadata", "artifacts"][..]] {
        let Some(artifacts) = value_at(output, artifacts_path).and_then(Value::as_array) else {
            continue;
        };
        for artifact in artifacts {
            let artifact_scenario = string_value(artifact, &["scenario_id"]);
            if artifact_scenario.is_some_and(|value| value != scenario_id) {
                continue;
            }
            if artifact_scenario.is_none() {
                continue;
            }
            collect_artifact_signature_fields(artifact, context);
        }
    }
}

fn collect_artifact_signature_fields(value: &Value, context: &mut ScenarioFailureDetails) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (key, child) in object {
        let normalized = key.to_ascii_lowercase();
        if normalized == "fatal_signature" || normalized == "fatal_signatures" {
            collect_signature_value(child, context);
        }
    }
}

fn numeric_count(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_f64().map(|number| number as u64))
        .unwrap_or(0)
}

fn is_status_count_object_key(key: &str) -> bool {
    matches!(
        key,
        "status_counts" | "status_count" | "status_codes" | "http_status_counts"
    )
}

fn collect_status_count_object(value: &Value, context: &mut ScenarioFailureDetails) {
    let Some(object) = value.as_object() else {
        return;
    };
    for (status, count) in object {
        if status_is_error(status) {
            add_status_count(context, status.clone(), numeric_count(count));
        }
    }
}

fn status_count_key(key: &str) -> Option<String> {
    for prefix in ["status_", "http_status_"] {
        let Some(rest) = key.strip_prefix(prefix) else {
            continue;
        };
        let status = rest.strip_suffix("_count").unwrap_or(rest);
        if status_is_error(status) {
            return Some(status.to_string());
        }
    }
    None
}

fn status_is_error(status: &str) -> bool {
    status.starts_with('4') || status.starts_with('5')
}

fn add_status_count(context: &mut ScenarioFailureDetails, status: String, count: u64) {
    if count > 0 {
        *context.status_counts.entry(status).or_default() += count;
    }
}

fn collect_signature_value(value: &Value, context: &mut ScenarioFailureDetails) {
    match value {
        Value::String(signature) if !signature.is_empty() => {
            context.fatal_signatures.push(signature.clone());
        }
        Value::Array(values) => {
            for value in values {
                collect_signature_value(value, context);
            }
        }
        _ => {}
    }
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
