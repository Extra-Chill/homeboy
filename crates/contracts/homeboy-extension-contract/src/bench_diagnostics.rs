//! Pure bench diagnostic + phase-event contract types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchDiagnostic {
    /// Workload-defined diagnostic class used for grouping related failures.
    #[serde(alias = "kind", alias = "code")]
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BenchDiagnosticSource>,
    #[serde(default, alias = "details", skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BenchDiagnosticSource {
    Run,
    Scenario {
        scenario_id: String,
    },
    ScenarioRun {
        scenario_id: String,
        run_index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchPhaseEvent {
    /// Generic phase identifier. Core treats this as a label, not a closed
    /// enum, so extensions can model their own lifecycle.
    pub phase: String,
    /// Event status, for example `started`, `heartbeat`, `completed`,
    /// `failed`, or `timeout`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub diagnostics: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payload: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchPhaseSummary {
    pub phase: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub heartbeat_count: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub diagnostic_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchPhaseFailureClassification {
    /// `timeout` or `failure` for classified terminal events.
    pub kind: String,
    pub phase: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
