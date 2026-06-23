//! Public input/output types for trace workflows.

use serde::Serialize;
use std::path::PathBuf;

use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::invocation::InvocationRequirements;
use crate::core::extension::trace::baseline::TraceBaselineComparison;
use crate::core::rig::TraceDependencySpec;

use super::super::attach::TraceAttachment;
use super::super::canonicality::TraceCanonicalPolicy;
use super::super::overlay::TraceOverlayRequest;
use super::super::parsing::{
    TraceComponentsProvenance, TraceEvidenceMetadata, TraceResults, TraceSpanDefinition,
    TraceToolchainProvenance,
};
use super::super::probes::TraceProbeConfig;

#[derive(Debug, Clone)]
pub struct TraceRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub runner_inputs: TraceRunnerInputs,
    pub scenario_id: String,
    pub json_summary: bool,
    pub rig_id: Option<String>,
    pub overlays: Vec<TraceOverlayRequest>,
    pub keep_overlay: bool,
    pub span_definitions: Vec<TraceSpanDefinition>,
    pub baseline_flags: BaselineFlags,
    pub regression_threshold_percent: f64,
    pub regression_min_delta_ms: u64,
    pub canonical_policy: TraceCanonicalPolicy,
    pub checkout_provenance: Option<TraceCheckoutProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCheckoutProvenance {
    pub source: String,
    pub path: String,
    pub requested_ref: String,
    pub resolved_sha: String,
}

#[derive(Debug, Clone)]
pub struct TraceListWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub runner_inputs: TraceRunnerInputs,
    pub rig_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TraceRunnerInputs {
    pub json_settings: Vec<(String, serde_json::Value)>,
    pub env: Vec<(String, String)>,
    pub workload_paths: Vec<PathBuf>,
    pub probes: Vec<TraceProbeConfig>,
    pub attachments: Vec<TraceAttachment>,
    pub dependencies: Vec<TraceDependencySpec>,
    pub runner_capabilities: Vec<String>,
    pub invocation_requirements: InvocationRequirements,
    pub public_preview: Option<crate::core::rig::TracePublicPreviewSpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub evidence: TraceEvidenceMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<TraceResults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<TraceRunFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TraceOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TraceBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<TraceToolchainProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<TraceComponentsProvenance>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub path: String,
    pub component_path: String,
    pub touched_files: Vec<String>,
    pub kept: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TraceRunFailure {
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_override: Option<String>,
    pub scenario_id: String,
    pub exit_code: i32,
    pub stderr_excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_observed_homeboy_event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_succeeded: Option<bool>,
}
