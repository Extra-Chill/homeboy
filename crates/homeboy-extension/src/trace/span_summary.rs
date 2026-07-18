use serde::Serialize;
use std::collections::BTreeMap;

use super::parsing::{TraceResults, TraceSpanStatus};
use super::report::TraceCommandOutput;
use super::TraceSpanMetadata;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TraceSpanSummaryOutput {
    pub id: String,
    pub from: String,
    pub to: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_t_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<TraceSpanMetadata>,
}

pub fn trace_span_summaries(
    results: &TraceResults,
    metadata_by_span: &BTreeMap<String, TraceSpanMetadata>,
) -> Vec<TraceSpanSummaryOutput> {
    results
        .span_results
        .iter()
        .map(|span| TraceSpanSummaryOutput {
            id: span.id.clone(),
            from: span.from.clone(),
            to: span.to.clone(),
            status: match span.status {
                TraceSpanStatus::Ok => "ok".to_string(),
                TraceSpanStatus::Skipped => "skipped".to_string(),
            },
            duration_ms: span.duration_ms,
            from_t_ms: span.from_t_ms,
            to_t_ms: span.to_t_ms,
            missing: span.missing.clone(),
            message: span.message.clone(),
            metadata: metadata_by_span.get(&span.id).cloned(),
        })
        .collect()
}

pub fn attach_span_summary_metadata(
    output: &mut TraceCommandOutput,
    metadata_by_span: &BTreeMap<String, TraceSpanMetadata>,
) {
    if metadata_by_span.is_empty() {
        return;
    }
    let TraceCommandOutput::Run(run) = output else {
        return;
    };
    if let Some(results) = run.results.as_ref() {
        run.span_summaries = trace_span_summaries(results, metadata_by_span);
    }
}

pub fn format_span_summary_status(span: &TraceSpanSummaryOutput) -> String {
    let mut parts = vec![span.status.clone()];
    if !span.missing.is_empty() {
        parts.push(format!("missing `{}`", span.missing.join("`, `")));
    }
    if let Some(message) = span.message.as_deref() {
        parts.push(message.to_string());
    }
    parts.join(": ")
}

pub fn format_span_summary_metadata(metadata: Option<&TraceSpanMetadata>) -> String {
    let Some(metadata) = metadata else {
        return "-".to_string();
    };
    let mut parts = Vec::new();
    if let Some(category) = metadata.category.as_deref() {
        parts.push(format!("category={category}"));
    }
    if let Some(blocks) = metadata.blocks.as_deref() {
        parts.push(format!("blocks={blocks}"));
    }
    if metadata.critical {
        parts.push("critical".to_string());
    }
    if metadata.blocking {
        parts.push("blocking".to_string());
    }
    if metadata.cacheable {
        parts.push("cacheable".to_string());
    }
    if metadata.prewarmable {
        parts.push("prewarmable".to_string());
    }
    if metadata.deferrable {
        parts.push("deferrable".to_string());
    }
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(", ")
    }
}
