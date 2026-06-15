use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::core::extension::{
    load_all_extensions, TraceBrowserArtifactMapConfig, TraceBrowserEvidenceAdapterConfig,
    TraceBrowserSummaryAliasConfig,
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraceBrowserSummaryExtraction {
    pub request_total: Option<f64>,
    pub page_errors: Option<f64>,
    pub browser_metrics: BTreeMap<String, f64>,
}

pub fn trace_browser_evidence_adapters() -> Vec<TraceBrowserEvidenceAdapterConfig> {
    load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|extension| extension.trace_browser_evidence().to_vec())
        .collect()
}

pub fn trace_browser_summary_has_signal(
    summary: &Map<String, Value>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> bool {
    summary_aliases(adapters)
        .iter()
        .any(|alias| summary_alias_has_signal(summary, alias))
}

pub fn trace_browser_summary_extract(
    summary: &Map<String, Value>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> TraceBrowserSummaryExtraction {
    let mut extraction = TraceBrowserSummaryExtraction::default();
    for alias in summary_aliases(adapters) {
        if extraction.request_total.is_none() {
            extraction.request_total =
                first_number_from_strings(summary, &alias.request_total_keys);
        }
        extraction.page_errors = extraction
            .page_errors
            .or_else(|| first_number_from_strings(summary, &alias.page_error_keys));
        for metric in alias.metrics {
            if let Some(value) = first_number_from_strings(summary, &metric.keys) {
                extraction.browser_metrics.insert(metric.metric, value);
            }
        }
    }
    extraction
}

pub fn trace_browser_artifact_map_fields(
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> Vec<String> {
    artifact_maps(adapters)
        .into_iter()
        .map(|map| map.field)
        .collect()
}

fn summary_aliases(
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> Vec<TraceBrowserSummaryAliasConfig> {
    adapters
        .iter()
        .cloned()
        .flat_map(|adapter| adapter.summary_aliases)
        .collect()
}

fn artifact_maps(
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> Vec<TraceBrowserArtifactMapConfig> {
    adapters
        .iter()
        .cloned()
        .flat_map(|adapter| adapter.artifact_maps)
        .collect()
}

fn summary_alias_has_signal(
    summary: &Map<String, Value>,
    alias: &TraceBrowserSummaryAliasConfig,
) -> bool {
    first_number_from_strings(summary, &alias.request_total_keys).is_some()
        || first_number_from_strings(summary, &alias.page_error_keys).is_some()
        || alias
            .metrics
            .iter()
            .any(|metric| first_number_from_strings(summary, &metric.keys).is_some())
}

fn first_number_from_strings(object: &Map<String, Value>, keys: &[String]) -> Option<f64> {
    keys.iter().find_map(|key| {
        object
            .get(key)
            .and_then(Value::as_f64)
            .or_else(|| {
                object
                    .get(key)
                    .and_then(Value::as_u64)
                    .map(|value| value as f64)
            })
            .or_else(|| {
                object
                    .get(key)
                    .and_then(Value::as_i64)
                    .map(|value| value as f64)
            })
    })
}
