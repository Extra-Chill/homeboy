//! Top-level trace result contract type.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::trace_parsing::{
    TraceArtifact, TraceAssertion, TraceComponentsProvenance, TraceDependencyProvenance,
    TraceEvent, TraceEvidenceMetadata, TraceScenario, TraceSpanDefinition, TraceSpanResult,
    TraceStatus, TraceTemporalAssertionDefinition, TraceToolchainProvenance,
};
use crate::trace_preview::TracePreviewMetadata;
use homeboy_lifecycle_contract::timeline::ObservationEvent;
use homeboy_lifecycle_contract::RigStateSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceResults {
    pub component_id: String,
    pub scenario_id: String,
    pub status: TraceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<RigStateSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TraceEvidenceMetadata>,
    #[serde(default)]
    pub timeline: Vec<TraceEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_definitions: Vec<TraceSpanDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_results: Vec<TraceSpanResult>,
    #[serde(default)]
    pub assertions: Vec<TraceAssertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temporal_assertions: Vec<TraceTemporalAssertionDefinition>,
    #[serde(default)]
    pub artifacts: Vec<TraceArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<TraceDependencyProvenance>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<TraceToolchainProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<TraceComponentsProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<TracePreviewMetadata>,
}
