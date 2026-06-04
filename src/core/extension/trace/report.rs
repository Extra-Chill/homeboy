//! Trace command output envelopes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::aggregate_report::TraceAggregateSpanSampleOutput;
use super::baseline::TraceBaselineComparison;
use super::overlay_lock::TraceOverlayLockRecord;
use super::parsing::{TraceArtifact, TraceAssertionStatus, TraceList, TraceResults};
use super::run::{TraceOverlay, TraceRunWorkflowResult};
use super::span_summary::{
    format_span_summary_metadata, format_span_summary_status, trace_span_summaries,
    TraceSpanSummaryOutput,
};
use crate::core::engine::detail_output::{bounded_items, DEFAULT_DETAIL_ITEM_LIMIT};
use crate::core::rig::RigStateSnapshot;
use crate::core::runner::is_reportable_artifact_evidence_path;

#[derive(Serialize)]
#[serde(untagged)]
pub enum TraceCommandOutput {
    Run(Box<TraceRunOutput>),
    Summary(TraceRunSummaryOutput),
    Aggregate(TraceAggregateOutput),
    Compare(TraceCompareOutput),
    Matrix(TraceVariantMatrixOutput),
    List(TraceListOutput),
    OverlayLocks(TraceOverlayLocksOutput),
}

#[derive(Serialize)]
pub struct TraceRunOutput {
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<TraceArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<TraceResults>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_summaries: Vec<TraceSpanSummaryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_state: Option<RigStateSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<super::run::TraceRunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TraceBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<TraceResolvedProfileOutput>,
}

#[derive(Serialize)]
pub struct TraceRunSummaryOutput {
    pub summary_only: bool,
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub assertion_count: usize,
    pub artifact_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceOverlay>,
    pub span_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<TraceResolvedProfileOutput>,
}

#[derive(Serialize)]
pub struct TraceListOutput {
    pub command: &'static str,
    pub component: String,
    pub component_id: String,
    pub count: usize,
    pub scenarios: Vec<super::parsing::TraceScenario>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<TraceProfileListItem>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TraceResolvedProfileOutput {
    pub id: String,
    pub rig_id: Option<String>,
    pub component: String,
    pub scenario: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct TraceProfileListItem {
    pub id: String,
    pub rig_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
}

#[derive(Serialize)]
pub struct TraceOverlayLocksOutput {
    pub command: &'static str,
    pub count: usize,
    pub active_count: usize,
    pub stale_count: usize,
    pub unknown_count: usize,
    pub locks: Vec<TraceOverlayLockRecord>,
}

#[derive(Serialize, Clone)]
pub struct TraceAggregateOutput {
    pub command: &'static str,
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub scenario_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_preset: Option<String>,
    pub repeat: usize,
    pub run_count: usize,
    pub failure_count: usize,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_order: Vec<TraceRunOrderEntryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rig_state: Option<RigStateSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceOverlay>,
    pub runs: Vec<TraceAggregateRunOutput>,
    pub spans: Vec<TraceAggregateSpanOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardrails: Vec<TraceGuardrailOutput>,
    #[serde(default, skip_serializing_if = "is_default_usize")]
    pub guardrail_failure_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_span_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_spans: Vec<TraceAggregateSpanOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classification_summaries: Vec<TraceClassificationSummaryOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmatched_span_metadata_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<TraceResolvedProfileOutput>,
}

#[derive(Serialize, Clone)]
pub struct TraceRunOrderEntryOutput {
    pub index: usize,
    pub group: String,
    pub iteration: usize,
}

#[derive(Serialize, Clone)]
pub struct TraceAggregateRunOutput {
    pub index: usize,
    pub passed: bool,
    pub status: String,
    pub exit_code: i32,
    pub artifact_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct TraceAggregateSpanOutput {
    pub id: String,
    pub n: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stddev_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p75_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p90_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_run_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_artifact_path: Option<String>,
    pub failures: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<TraceAggregateSpanSampleOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<TraceSpanMetadata>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct TraceSpanMetadata {
    #[serde(default, skip_serializing_if = "is_default_bool")]
    pub critical: bool,
    #[serde(default, skip_serializing_if = "is_default_bool")]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "is_default_bool")]
    pub cacheable: bool,
    #[serde(default, skip_serializing_if = "is_default_bool")]
    pub prewarmable: bool,
    #[serde(default, skip_serializing_if = "is_default_bool")]
    pub deferrable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TraceClassificationSummaryOutput {
    pub classification: String,
    pub span_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_avg_ms: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TraceGuardrailOutput {
    pub label: String,
    pub source: String,
    pub passed: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct TraceCompareOutput {
    pub command: &'static str,
    pub before_path: String,
    pub after_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_git_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_git_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_scenario_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_scenario_id: Option<String>,
    pub span_count: usize,
    pub spans: Vec<TraceCompareSpanOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_span_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_spans: Vec<TraceCompareSpanOutput>,
    #[serde(default, skip_serializing_if = "is_default_usize")]
    pub focus_regression_count: usize,
    #[serde(default, skip_serializing_if = "is_default_usize")]
    pub focus_failure_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before_guardrails: Vec<TraceGuardrailOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after_guardrails: Vec<TraceGuardrailOutput>,
    #[serde(default, skip_serializing_if = "is_default_usize")]
    pub guardrail_failure_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guardrail_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classification_summaries: Vec<TraceCompareClassificationSummaryOutput>,
}

#[derive(Serialize, Clone)]
pub struct TraceCompareClassificationSummaryOutput {
    pub classification: String,
    pub span_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_total_median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_total_median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_delta_ms: Option<i64>,
}

#[derive(Serialize, Clone)]
pub struct TraceVariantMatrixOutput {
    pub command: &'static str,
    pub passed: bool,
    pub status: String,
    pub component: String,
    pub scenario_id: String,
    pub matrix: String,
    pub output_dir: String,
    pub baseline_path: String,
    pub summary_path: String,
    pub run_count: usize,
    pub failure_count: usize,
    pub exit_code: i32,
    pub runs: Vec<TraceVariantMatrixRunOutput>,
}

#[derive(Serialize, Clone)]
pub struct TraceVariantMatrixRunOutput {
    pub label: String,
    pub variants: Vec<String>,
    pub overlays: Vec<String>,
    pub aggregate_path: String,
    pub compare_path: String,
    pub passed: bool,
    pub status: String,
    pub exit_code: i32,
    pub span_count: usize,
}

#[derive(Serialize, Clone)]
pub struct TraceCompareSpanOutput {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_delta_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_delta_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_avg_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_avg_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_delta_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_delta_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_failures: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_failures: Option<usize>,
}

fn is_default_usize(value: &usize) -> bool {
    value.eq(&usize::default())
}

fn is_default_bool(value: &bool) -> bool {
    !*value
}

pub fn from_main_workflow(
    result: TraceRunWorkflowResult,
    rig_state: Option<RigStateSnapshot>,
    summary_only: bool,
) -> (TraceCommandOutput, i32) {
    let (output, _, exit_code) = from_main_workflow_outputs(result, rig_state, summary_only);
    (output, exit_code)
}

pub fn from_main_workflow_outputs(
    result: TraceRunWorkflowResult,
    rig_state: Option<RigStateSnapshot>,
    summary_only: bool,
) -> (TraceCommandOutput, Option<TraceCommandOutput>, i32) {
    let exit_code = result.exit_code;
    if summary_only {
        let full_output = from_run_workflow_result(result.clone(), rig_state.clone());
        let output = TraceRunSummaryOutput {
            summary_only: true,
            passed: exit_code == 0 && result.status == "pass",
            status: result.status,
            component: result.component,
            exit_code,
            scenario_id: result.results.as_ref().map(|r| r.scenario_id.clone()),
            summary: result.results.as_ref().and_then(|r| r.summary.clone()),
            assertion_count: result
                .results
                .as_ref()
                .map(|r| r.assertions.len())
                .unwrap_or(0),
            artifact_count: result
                .results
                .as_ref()
                .map(|r| r.artifacts.len())
                .unwrap_or(0),
            rig_id: rig_state.as_ref().map(|r| r.rig_id.clone()),
            overlays: result.overlays,
            span_count: result
                .results
                .as_ref()
                .map(|r| r.span_results.len())
                .unwrap_or(0),
            hints: result.hints,
            profile: None,
        };
        return (
            TraceCommandOutput::Summary(output),
            Some(full_output),
            exit_code,
        );
    }

    (from_run_workflow_result(result, rig_state), None, exit_code)
}

fn from_run_workflow_result(
    result: TraceRunWorkflowResult,
    rig_state: Option<RigStateSnapshot>,
) -> TraceCommandOutput {
    let artifacts = result
        .results
        .as_ref()
        .map(|r| reportable_trace_artifacts(&r.artifacts))
        .unwrap_or_default();
    let span_summaries = result
        .results
        .as_ref()
        .map(|results| trace_span_summaries(results, &BTreeMap::new()))
        .unwrap_or_default();
    TraceCommandOutput::Run(Box::new(TraceRunOutput {
        passed: result.exit_code == 0 && result.status == "pass",
        status: result.status,
        component: result.component,
        exit_code: result.exit_code,
        artifacts,
        results: result.results,
        span_summaries,
        rig_state,
        failure: result.failure,
        overlays: result.overlays,
        baseline_comparison: result.baseline_comparison,
        hints: result.hints,
        profile: None,
    }))
}

pub fn render_markdown(results: &TraceResults, overlays: &[TraceOverlay]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Trace: `{}`\n\n", results.scenario_id));
    out.push_str(&format!("- **Component:** `{}`\n", results.component_id));
    out.push_str(&format!("- **Status:** `{}`\n", results.status.as_str()));
    if let Some(summary) = &results.summary {
        out.push_str(&format!("- **Summary:** {}\n", summary));
    }
    if let Some(failure) = &results.failure {
        out.push_str(&format!("- **Failure:** {}\n", failure));
    }

    push_overlay_markdown(&mut out, overlays);

    if !results.span_results.is_empty() {
        out.push_str("\n## Spans\n\n");
        out.push_str("| Span | From | To | Duration | Status | Metadata |\n");
        out.push_str("|---|---|---|---:|---|---|\n");
        let summaries = trace_span_summaries(results, &BTreeMap::new());
        let (spans, metadata) = bounded_items(&summaries, DEFAULT_DETAIL_ITEM_LIMIT);
        for span in spans {
            let duration = span
                .duration_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            let status = format_span_summary_status(span);
            let metadata = format_span_summary_metadata(span.metadata.as_ref());
            out.push_str(&format!(
                "| `{}` | `{}` | `{}` | {} | {} | {} |\n",
                span.id, span.from, span.to, duration, status, metadata
            ));
        }
        push_omitted_detail_line(&mut out, "span(s)", &metadata);
    }

    if !results.assertions.is_empty() {
        out.push_str("\n## Assertions\n\n");
        let (assertions, metadata) = bounded_items(&results.assertions, DEFAULT_DETAIL_ITEM_LIMIT);
        for assertion in assertions {
            let status = match assertion.status {
                TraceAssertionStatus::Pass => "pass",
                TraceAssertionStatus::Fail => "fail",
                TraceAssertionStatus::Error => "error",
            };
            match &assertion.message {
                Some(message) => out.push_str(&format!(
                    "- `{}`: **{}** - {}\n",
                    assertion.id, status, message
                )),
                None => out.push_str(&format!("- `{}`: **{}**\n", assertion.id, status)),
            }
        }
        push_omitted_detail_line(&mut out, "assertion(s)", &metadata);
    }

    let artifacts = reportable_trace_artifacts(&results.artifacts);
    if !artifacts.is_empty() {
        out.push_str("\n## Artifacts\n\n");
        let (artifacts, metadata) = bounded_items(&artifacts, DEFAULT_DETAIL_ITEM_LIMIT);
        for artifact in artifacts {
            out.push_str(&format!("- **{}:** `{}`\n", artifact.label, artifact.path));
        }
        push_omitted_detail_line(&mut out, "artifact(s)", &metadata);
    }

    if !results.timeline.is_empty() {
        out.push_str("\n## Timeline\n\n");
        let (events, metadata) = bounded_items(&results.timeline, DEFAULT_DETAIL_ITEM_LIMIT);
        for event in events {
            out.push_str(&format!(
                "- `{}ms` `{}.{}`\n",
                event.t_ms, event.source, event.event
            ));
        }
        push_omitted_detail_line(&mut out, "timeline event(s)", &metadata);
    }

    out
}

fn reportable_trace_artifacts(artifacts: &[TraceArtifact]) -> Vec<TraceArtifact> {
    artifacts
        .iter()
        .filter(|artifact| is_reportable_artifact_evidence_path(&artifact.path))
        .cloned()
        .collect()
}

pub fn push_overlay_markdown(out: &mut String, overlays: &[TraceOverlay]) {
    if overlays.is_empty() {
        return;
    }

    out.push_str("\n## Trace Overlays\n\n");
    for overlay in overlays {
        let status = if overlay.kept { "kept" } else { "reverted" };
        let variant = overlay
            .variant
            .as_ref()
            .map(|name| format!(" variant `{name}`,"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- **Patch:**{} `{}` (`{}`)\n",
            variant, overlay.path, status
        ));
        out.push_str(&format!(
            "  - Applied relative to: `{}`\n",
            overlay.component_path
        ));
        if overlay.touched_files.is_empty() {
            out.push_str("  - Touched files: none reported by `git apply --numstat`\n");
        } else {
            out.push_str("  - Touched files:\n");
            let (files, metadata) =
                bounded_items(&overlay.touched_files, DEFAULT_DETAIL_ITEM_LIMIT);
            for file in files {
                out.push_str(&format!("    - `{}`\n", file));
            }
            push_indented_omitted_detail_line(out, "touched file(s)", &metadata);
        }
    }
}

fn push_omitted_detail_line(
    out: &mut String,
    label: &str,
    metadata: &crate::core::engine::detail_output::DetailOutputMetadata,
) {
    if metadata.truncated {
        out.push_str(&format!(
            "- _... {} more {} omitted (shown: {}, total: {}, limit: {})_\n",
            metadata.omitted_item_count,
            label,
            metadata.items_rendered,
            metadata.items_seen,
            metadata.item_limit
        ));
    }
}

fn push_indented_omitted_detail_line(
    out: &mut String,
    label: &str,
    metadata: &crate::core::engine::detail_output::DetailOutputMetadata,
) {
    if metadata.truncated {
        out.push_str(&format!(
            "    - _... {} more {} omitted (shown: {}, total: {}, limit: {})_\n",
            metadata.omitted_item_count,
            label,
            metadata.items_rendered,
            metadata.items_seen,
            metadata.item_limit
        ));
    }
}

pub fn from_list_workflow(component: String, list: TraceList) -> (TraceCommandOutput, i32) {
    let count = list.scenarios.len();
    (
        TraceCommandOutput::List(TraceListOutput {
            command: "trace.list",
            component,
            component_id: list.component_id,
            count,
            scenarios: list.scenarios,
            profiles: Vec::new(),
        }),
        0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::trace::parsing::{TraceScenario, TraceStatus};

    #[test]
    fn test_from_list_workflow() {
        let list = TraceList {
            component_id: "studio".to_string(),
            scenario_id: None,
            status: None,
            scenarios: vec![TraceScenario {
                id: "close-window-running-site".to_string(),
                source: Some("fixtures/close-window.trace.js".to_string()),
                summary: Some("Close window while a site is running".to_string()),
            }],
            timeline: Vec::new(),
            assertions: Vec::new(),
            artifacts: Vec::new(),
        };

        let (output, exit_code) = from_list_workflow("Studio".to_string(), list);
        let value = serde_json::to_value(output).expect("list output should serialize");

        assert_eq!(exit_code, 0);
        assert_eq!(value["command"], "trace.list");
        assert_eq!(value["component"], "Studio");
        assert_eq!(value["component_id"], "studio");
        assert_eq!(value["count"], 1);
        assert_eq!(value["scenarios"][0]["id"], "close-window-running-site");
    }

    #[test]
    fn test_from_main_workflow() {
        let result = TraceRunWorkflowResult {
            status: "pass".to_string(),
            component: "Studio".to_string(),
            exit_code: 0,
            results: Some(TraceResults {
                component_id: "studio".to_string(),
                scenario_id: "close-window-running-site".to_string(),
                status: TraceStatus::Pass,
                summary: Some("No window reopened".to_string()),
                failure: None,
                rig: None,
                timeline: Vec::new(),
                span_definitions: Vec::new(),
                span_results: Vec::new(),
                assertions: Vec::new(),
                temporal_assertions: Vec::new(),
                artifacts: vec![TraceArtifact {
                    label: "main log".to_string(),
                    path: "artifacts/main.log".to_string(),
                }],
            }),
            failure: None,
            overlays: vec![TraceOverlay {
                variant: Some("disable-install-mail".to_string()),
                component_id: Some("studio".to_string()),
                path: "/tmp/overlay.patch".to_string(),
                component_path: "/tmp/studio".to_string(),
                touched_files: vec!["scenario.txt".to_string()],
                kept: false,
            }],
            baseline_comparison: None,
            hints: None,
        };

        let (output, exit_code) = from_main_workflow(result, None, true);
        let value = serde_json::to_value(output).expect("summary output should serialize");

        assert_eq!(exit_code, 0);
        assert_eq!(value["summary_only"], true);
        assert_eq!(value["passed"], true);
        assert_eq!(value["scenario_id"], "close-window-running-site");
        assert_eq!(value["artifact_count"], 1);
        assert_eq!(value["span_count"], 0);
        assert_eq!(value["overlays"][0]["path"], "/tmp/overlay.patch");
        assert_eq!(value["overlays"][0]["variant"], "disable-install-mail");
        assert_eq!(value["overlays"][0]["component_id"], "studio");
        assert_eq!(value["overlays"][0]["component_path"], "/tmp/studio");
        assert_eq!(value["overlays"][0]["touched_files"][0], "scenario.txt");
        assert_eq!(value["overlays"][0]["kept"], false);
    }

    #[test]
    fn test_from_main_workflow_outputs_keeps_full_output_for_summary_artifact() {
        let result = TraceRunWorkflowResult {
            status: "pass".to_string(),
            component: "Studio".to_string(),
            exit_code: 0,
            results: Some(TraceResults {
                component_id: "studio".to_string(),
                scenario_id: "create-site".to_string(),
                status: TraceStatus::Pass,
                summary: Some("Created a site".to_string()),
                failure: None,
                rig: None,
                timeline: vec![crate::core::extension::trace::parsing::TraceEvent {
                    t_ms: 10,
                    source: "ui".to_string(),
                    event: "submit".to_string(),
                    data: std::collections::BTreeMap::new(),
                }],
                span_definitions: Vec::new(),
                span_results: vec![crate::core::extension::trace::parsing::TraceSpanResult {
                    id: "submit_to_cli".to_string(),
                    from: "ui.submit".to_string(),
                    to: "cli.start".to_string(),
                    status: crate::core::extension::trace::parsing::TraceSpanStatus::Ok,
                    duration_ms: Some(42),
                    from_t_ms: Some(10),
                    to_t_ms: Some(52),
                    missing: Vec::new(),
                    message: None,
                }],
                assertions: Vec::new(),
                temporal_assertions: Vec::new(),
                artifacts: Vec::new(),
            }),
            failure: None,
            overlays: Vec::new(),
            baseline_comparison: None,
            hints: None,
        };

        let (stdout_output, artifact_output, exit_code) =
            from_main_workflow_outputs(result, None, true);
        let stdout_value = serde_json::to_value(stdout_output).expect("summary should serialize");
        let artifact_value = serde_json::to_value(artifact_output.expect("full artifact output"))
            .expect("artifact should serialize");

        assert_eq!(exit_code, 0);
        assert_eq!(stdout_value["summary_only"], true);
        assert_eq!(stdout_value["span_count"], 1);
        assert!(stdout_value.get("results").is_none());
        assert_eq!(
            artifact_value["results"]["span_results"][0]["id"],
            "submit_to_cli"
        );
        assert_eq!(artifact_value["results"]["timeline"][0]["event"], "submit");
    }

    #[test]
    fn test_push_overlay_markdown_lists_paths() {
        let mut markdown = String::new();
        let overlays = vec![
            TraceOverlay {
                variant: None,
                component_id: Some("studio".to_string()),
                path: "/tmp/overlay.patch".to_string(),
                component_path: "/tmp/studio".to_string(),
                touched_files: vec!["apps/studio/out/app.js".to_string()],
                kept: false,
            },
            TraceOverlay {
                variant: None,
                component_id: Some("studio".to_string()),
                path: "/tmp/kept.patch".to_string(),
                component_path: "/tmp/studio".to_string(),
                touched_files: Vec::new(),
                kept: true,
            },
        ];

        push_overlay_markdown(&mut markdown, &overlays);

        assert!(markdown.contains("## Trace Overlays"));
        assert!(markdown.contains("- **Patch:** `/tmp/overlay.patch` (`reverted`)"));
        assert!(markdown.contains("- Applied relative to: `/tmp/studio`"));
        assert!(markdown.contains("- `apps/studio/out/app.js`"));
        assert!(markdown.contains("- **Patch:** `/tmp/kept.patch` (`kept`)"));
        assert!(markdown.contains("Touched files: none reported by `git apply --numstat`"));
    }

    #[test]
    fn test_render_markdown() {
        let results = TraceResults {
            component_id: "studio".to_string(),
            scenario_id: "create-site".to_string(),
            status: TraceStatus::Pass,
            summary: Some("Created a site".to_string()),
            failure: None,
            rig: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: vec![crate::core::extension::trace::parsing::TraceSpanResult {
                id: "submit_to_cli".to_string(),
                from: "ui.submit".to_string(),
                to: "cli.start".to_string(),
                status: crate::core::extension::trace::parsing::TraceSpanStatus::Ok,
                duration_ms: Some(42),
                from_t_ms: Some(10),
                to_t_ms: Some(52),
                missing: Vec::new(),
                message: None,
            }],
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
        };

        let overlays = vec![TraceOverlay {
            variant: None,
            component_id: Some("studio".to_string()),
            path: "/tmp/overlay.patch".to_string(),
            component_path: "/tmp/studio".to_string(),
            touched_files: vec!["apps/studio/out/app.js".to_string()],
            kept: false,
        }];
        let markdown = render_markdown(&results, &overlays);

        assert!(markdown.contains("# Trace: `create-site`"));
        assert!(markdown.contains("## Trace Overlays"));
        assert!(markdown.contains("- **Patch:** `/tmp/overlay.patch` (`reverted`)"));
        assert!(markdown.contains("- Applied relative to: `/tmp/studio`"));
        assert!(markdown.contains("- `apps/studio/out/app.js`"));
        assert!(markdown.contains("| `submit_to_cli` | `ui.submit` | `cli.start` | 42ms | ok |"));
    }

    #[test]
    fn test_render_markdown_omits_unproven_remote_artifact_paths() {
        let results = TraceResults {
            component_id: "studio".to_string(),
            scenario_id: "create-site".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: vec![
                TraceArtifact {
                    label: "remote trace".to_string(),
                    path: "/srv/remote-only/trace.zip".to_string(),
                },
                TraceArtifact {
                    label: "mirrored trace".to_string(),
                    path: "runner-artifact://lab/run-1/trace.zip".to_string(),
                },
            ],
        };

        let markdown = render_markdown(&results, &[]);

        assert!(!markdown.contains("/srv/remote-only/trace.zip"));
        assert!(markdown.contains("runner-artifact://lab/run-1/trace.zip"));
    }
}
