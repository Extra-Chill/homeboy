//! Pure trace parsing/result contract types (assertions, provenance,
//! artifacts, canonical checks) + span/event aliases.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use homeboy_lifecycle_contract::timeline::{
    ObservationEvent, ObservationSpanDefinition, ObservationSpanResult, ObservationSpanStatus,
};

pub type TraceEvent = ObservationEvent;

pub type TraceSpanDefinition = ObservationSpanDefinition;

pub type TraceSpanResult = ObservationSpanResult;

pub type TraceSpanStatus = ObservationSpanStatus;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TraceStatus {
    Pass,
    Fail,
    Error,
}

impl TraceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TraceStatus::Pass => "pass",
            TraceStatus::Fail => "fail",
            TraceStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TraceAssertionStatus {
    Pass,
    Fail,
    Error,
}

impl TraceAssertionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TraceAssertionStatus::Pass => "pass",
            TraceAssertionStatus::Fail => "fail",
            TraceAssertionStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceDependencyProvenance {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_marker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceToolchainProvenance {
    pub canonical: bool,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    pub homeboy: TraceGitProvenance,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub toolchains: BTreeMap<String, TraceGitProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_assets: BTreeMap<String, TraceRuntimeAssetProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceComponentsProvenance {
    pub target: TraceGitProvenance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<TraceGitProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceGitProvenance {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceRuntimeAssetProvenance {
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceEvidenceMetadata {
    pub canonical: bool,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<TraceCanonicalCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceCanonicalCheck {
    pub target: String,
    pub path: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_ahead: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits_behind: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceAssertion {
    pub id: String,
    pub status: TraceAssertionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TraceTemporalAssertionDefinition {
    Count {
        id: String,
        events: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    ForbiddenEvent {
        id: String,
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    MaxConcurrent {
        id: String,
        track: Vec<String>,
        max: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    NoOverlap {
        id: String,
        events: Vec<String>,
        by: String,
        window_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Ordering {
        id: String,
        before: String,
        after: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        within_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    LatencyBound {
        id: String,
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        p50_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        p95_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        p99_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    RequiredSequence {
        id: String,
        sequence: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceArtifact {
    pub label: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceList {
    pub component_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TraceStatus>,
    #[serde(default)]
    pub scenarios: Vec<TraceScenario>,
    #[serde(default)]
    pub timeline: Vec<TraceEvent>,
    #[serde(default)]
    pub assertions: Vec<TraceAssertion>,
    #[serde(default)]
    pub artifacts: Vec<TraceArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceScenario {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}
