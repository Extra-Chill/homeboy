//! Extension trace capability — black-box evidence capture for lifecycle bugs.
//!
//! Trace is a sibling of `test` and `bench`: Homeboy resolves a component-owned
//! extension script, creates a run directory, passes an env-var contract, and
//! parses a JSON envelope written by the runner. Unlike bench, trace has no
//! baselines, ratchets, or metric gates; its job is to preserve causality and
//! evidence artifacts. Optional span baselines compare generic
//! `source.event` intervals without teaching core about product-specific
//! milestones.

mod aggregate_report;
pub mod assertions;
mod attach;
pub mod baseline;
mod canonicality;
mod generic_runner;
mod overlay;
mod overlay_lock;
pub mod parsing;
mod preflight;
mod preview;
pub mod probes;
pub mod report;
pub mod run;
mod span_summary;
pub mod spans;

use crate::core::component::Component;
use crate::core::extension::{ExtensionCapability, ExtensionExecutionContext};

pub use aggregate_report::TraceAggregateSpanSampleOutput;
pub use attach::TraceAttachment;
pub use canonicality::TraceCanonicalPolicy;
pub use overlay::TraceOverlayRequest;
pub use overlay_lock::{cleanup_stale_trace_overlay_locks, list_trace_overlay_locks};
pub use overlay_lock::{
    TraceOverlayLockCleanupResult, TraceOverlayLockRecord, TraceOverlayLockStatus,
};
pub use parsing::{parse_trace_list_str, parse_trace_results_file};
pub use parsing::{
    TraceArtifact, TraceAssertion, TraceEvent, TraceEvidenceMetadata, TraceList, TraceScenario,
    TraceStatus,
};
pub use parsing::{TraceAssertionStatus, TraceResults, TraceSpanDefinition, TraceSpanResult};
pub use preview::{TracePreviewMetadata, TracePublicPreviewSession};
pub use probes::{ActiveTraceProbes, TraceProbeConfig};
pub use report::{
    from_list_workflow, from_main_workflow, from_main_workflow_outputs, TraceAggregateMetricOutput,
    TraceAggregateMetricSampleOutput, TraceAggregateOutput, TraceAggregateRunOutput,
    TraceAggregateSpanOutput, TraceBrowserProofOutput, TraceClassificationSummaryOutput,
    TraceCommandOutput, TraceCompareClassificationSummaryOutput, TraceCompareMetricOutput,
    TraceCompareOutput, TraceCompareRunOrderOutput, TraceCompareSpanOutput, TraceGuardrailOutput,
    TraceListOutput, TraceMetricGuardrailOutput, TraceOverlayLocksOutput, TraceProfileListItem,
    TraceResolvedProfileOutput, TraceRunOrderEntryOutput, TraceScenarioMatrixAxisOutput,
    TraceScenarioMatrixCellOutput, TraceScenarioMatrixOutput, TraceSpanMetadata,
    TraceVariantMatrixOutput, TraceVariantMatrixRunOutput,
};
pub use report::{push_overlay_markdown, render_markdown};
pub use run::TraceOverlay;
pub use run::{
    resolve_declared_trace_artifact_path, run_trace_list_workflow, run_trace_workflow,
    trace_is_unclaimed, TraceListWorkflowArgs,
};
pub use run::{TraceRunWorkflowArgs, TraceRunWorkflowResult, TraceRunnerInputs};
pub use span_summary::{
    attach_span_summary_metadata, format_span_summary_metadata, format_span_summary_status,
    TraceSpanSummaryOutput,
};

pub fn resolve_trace_command(
    component: &Component,
) -> crate::core::error::Result<ExtensionExecutionContext> {
    crate::core::extension::resolve_execution_context(component, ExtensionCapability::Trace)
}
