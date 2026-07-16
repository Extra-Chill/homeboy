use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Map, Value};

use homeboy::core::extension::trace::{
    trace_browser_artifact_map_fields, trace_browser_summary_extract,
};
use homeboy::core::extension::TraceBrowserEvidenceAdapterConfig;

use super::super::types::{ArtifactRef, AssertionFailure, AssertionStats};
use super::BrowserEvidenceSample;

pub(super) fn assertion_stats(value: Option<&Value>) -> AssertionStats {
    let Some(value) = value else {
        return AssertionStats::default();
    };
    if let Some(object) = value.as_object() {
        return AssertionStats {
            total: u64_value(object, "total").unwrap_or_default(),
            passed: u64_value(object, "passed").unwrap_or_default(),
            failed: u64_value(object, "failed").unwrap_or_default(),
            skipped: u64_value(object, "skipped").unwrap_or_default(),
            ..AssertionStats::default()
        };
    }
    let mut stats = AssertionStats::default();
    for assertion in value.as_array().into_iter().flatten() {
        let status = assertion
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        stats.total += 1;
        match status {
            "pass" | "passed" | "ok" | "success" => stats.passed += 1,
            "fail" | "failed" | "error" => {
                stats.failed += 1;
                if is_advisory_assertion(assertion) {
                    stats.advisory_failed += 1;
                    stats
                        .failed_advisory_assertions
                        .push(assertion_failure(assertion));
                }
            }
            "skip" | "skipped" => stats.skipped += 1,
            _ => {}
        }
    }
    stats
}

fn is_advisory_assertion(assertion: &Value) -> bool {
    ["severity", "level", "kind", "type"]
        .iter()
        .filter_map(|key| assertion.get(*key).and_then(Value::as_str))
        .any(|value| value.eq_ignore_ascii_case("advisory"))
        || first_value_string(assertion, &["id"])
            .is_some_and(|id| id.to_ascii_lowercase().starts_with("advisory:"))
        || assertion
            .get("details")
            .and_then(Value::as_object)
            .and_then(|details| first_string(details, &["severity", "level", "kind", "type"]))
            .is_some_and(|value| value.eq_ignore_ascii_case("advisory"))
}

fn assertion_failure(assertion: &Value) -> AssertionFailure {
    let id = first_value_string(assertion, &["id", "name"])
        .unwrap_or_else(|| "advisory assertion".to_string());
    let details = assertion.get("details").and_then(Value::as_object);
    AssertionFailure {
        id,
        selector: first_value_string(assertion, &["selector"])
            .or_else(|| details.and_then(|details| first_string(details, &["selector"]))),
        message: first_value_string(assertion, &["message", "failure"])
            .or_else(|| details.and_then(|details| first_string(details, &["message", "failure"]))),
    }
}

pub(super) fn collect_requests(object: &Map<String, Value>, sample: &mut BrowserEvidenceSample) {
    sample.request_total = first_number(object, &["request_count", "requests_total"]);
    if let Some(requests) = object
        .get("requests")
        .or_else(|| object.get("network_requests"))
        .and_then(Value::as_array)
    {
        sample.request_total = Some(requests.len() as f64);
        for request in requests {
            if let Some(host) = request_host(request) {
                *sample.request_by_host.entry(host).or_default() += 1.0;
            }
            if let Some(resource_type) =
                first_value_string(request, &["resource_type", "resourceType", "type"])
            {
                *sample.request_by_type.entry(resource_type).or_default() += 1.0;
            }
        }
    }
    if let Some(summary) = object.get("request_summary").and_then(Value::as_object) {
        if sample.request_total.is_none() {
            sample.request_total = first_number(summary, &["total", "count"]);
        }
        collect_count_map(summary.get("by_host"), &mut sample.request_by_host);
        collect_count_map(summary.get("by_type"), &mut sample.request_by_type);
        collect_count_map(summary.get("by_resource_type"), &mut sample.request_by_type);
    }
}

pub(super) fn collect_declared_browser_summary_adapters(
    summary: &Map<String, Value>,
    sample: &mut BrowserEvidenceSample,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    let extraction = trace_browser_summary_extract(summary, adapters);
    if sample.request_total.is_none() {
        sample.request_total = extraction.request_total;
    }
    sample.page_errors = sample.page_errors.or(extraction.page_errors);
    sample.browser_metrics.extend(extraction.browser_metrics);
}

pub(super) fn collect_declared_artifact_map_adapters(
    object: &Map<String, Value>,
    artifacts: &mut BTreeSet<ArtifactRef>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    for field in trace_browser_artifact_map_fields(adapters) {
        collect_artifact_map(object.get(&field), artifacts);
    }
}

pub(super) fn collect_artifacts(
    object: &Map<String, Value>,
    artifacts: &mut BTreeSet<ArtifactRef>,
) {
    let Some(values) = object.get("artifacts").and_then(Value::as_array) else {
        return;
    };
    for artifact in values {
        let label = first_value_string(artifact, &["label", "kind", "type", "name"])
            .unwrap_or_else(|| "artifact".to_string());
        let target = first_value_string(artifact, &["url", "href", "path", "target"]);
        if let Some(target) = target {
            artifacts.insert(ArtifactRef { label, target });
        }
    }
}

fn collect_artifact_map(value: Option<&Value>, artifacts: &mut BTreeSet<ArtifactRef>) {
    let Some(files) = value.and_then(Value::as_object) else {
        return;
    };
    for (label, value) in files {
        match value {
            Value::String(target) if !target.is_empty() => {
                artifacts.insert(ArtifactRef {
                    label: label.clone(),
                    target: target.clone(),
                });
            }
            Value::Array(values) => {
                for target in values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|target| !target.is_empty())
                {
                    artifacts.insert(ArtifactRef {
                        label: label.clone(),
                        target: target.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
}

pub(super) fn collect_metric_object(
    value: Option<&Value>,
    out: &mut BTreeMap<String, f64>,
    names: &[&str],
) {
    let Some(object) = value.and_then(Value::as_object) else {
        return;
    };
    collect_top_level_numbers(object, out, names);
}

pub(super) fn collect_top_level_numbers(
    object: &Map<String, Value>,
    out: &mut BTreeMap<String, f64>,
    names: &[&str],
) {
    for name in names {
        if let Some(value) = number_value(object, name) {
            out.insert((*name).to_string(), value);
        }
    }
    for (name, value) in object {
        if name.starts_with("browser_") {
            if let Some(value) = value.as_f64() {
                out.insert(name.clone(), value);
            }
        }
    }
}

fn collect_count_map(value: Option<&Value>, out: &mut BTreeMap<String, f64>) {
    let Some(object) = value.and_then(Value::as_object) else {
        return;
    };
    for (key, value) in object {
        if let Some(value) = value.as_f64() {
            out.insert(key.clone(), value);
        }
    }
}

pub(super) fn collect_matrix(value: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if let Some(value) = scalar_string(value) {
                    out.insert(key.clone(), value);
                }
            }
        }
        Value::String(value) => {
            out.insert(prefix.to_string(), value.clone());
        }
        _ => {}
    }
}

pub(super) fn artifact_ref(
    root: &Path,
    path: &Path,
    include_local_paths: bool,
    label: Option<String>,
) -> ArtifactRef {
    let target = if include_local_paths {
        path.display().to_string()
    } else {
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    ArtifactRef {
        label: label.unwrap_or_else(|| "source".to_string()),
        target,
    }
}

fn request_host(value: &Value) -> Option<String> {
    first_value_string(value, &["host", "hostname"]).or_else(|| {
        let url = first_value_string(value, &["url", "href"])?;
        host_from_url(&url)
    })
}

fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|host| !host.is_empty())
        .map(|host| host.to_string())
}

pub(super) fn error_count(object: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = object.get(*key) {
            if let Some(count) = value.as_f64() {
                return Some(count);
            }
            if let Some(array) = value.as_array() {
                return Some(array.len() as f64);
            }
        }
    }
    None
}

pub(super) fn first_string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(scalar_string))
}

fn first_value_string(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    first_string(object, keys)
}

pub(super) fn first_number(object: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| number_value(object, key))
}

fn number_value(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(Value::as_f64)
}

fn u64_value(object: &Map<String, Value>, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn browser_metric_names() -> Vec<&'static str> {
    vec![
        "fcp_ms",
        "lcp_ms",
        "cls",
        "ttfb_ms",
        "total_blocking_time_ms",
        "load_ms",
        "duration_ms",
        "ready_ms",
        "browser_peak_used_js_heap_bytes",
        "browser_final_used_js_heap_bytes",
        "browser_checkpoint_count",
        "browser_dom_node_count",
        "browser_iframe_count",
        "browser_resource_count",
        "browser_transfer_size_bytes",
        "browser_nav_duration_ms",
        "browser_dom_content_loaded_ms",
        "browser_load_event_ms",
        "browser_response_start_ms",
        "browser_response_end_ms",
        "browser_request_start_ms",
        "browser_ttfb_ms",
        "browser_redirect_ms",
        "browser_first_paint_ms",
        "browser_fcp_ms",
        "browser_lcp_ms",
        "browser_lcp_size",
        "browser_long_task_count",
        "browser_long_task_total_ms",
        "browser_cls",
        "browser_layout_shift_count",
        "browser_layout_shift_max",
        "browser_evidence_summary_present",
        "browser_console_message_count",
        "browser_page_error_count",
        "browser_network_event_count",
    ]
}

pub(super) fn lifecycle_metric_names() -> Vec<&'static str> {
    vec![
        "dom_content_loaded_ms",
        "domContentLoaded_ms",
        "load_event_ms",
        "network_idle_ms",
        "first_paint_ms",
        "interactive_ms",
    ]
}
