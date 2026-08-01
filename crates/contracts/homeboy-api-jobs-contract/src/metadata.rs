//! Pure job artifact / lifecycle metadata structs shared by the job model and
//! the runner-facing execution surface.

use serde::{Deserialize, Serialize};

/// The canonical artifact pointer. Defined in `homeboy-lab-contract` because
/// this crate already depends on it and the Lab workload types need the same
/// shape -- defining it here and importing it there would be a cycle.
/// Re-exported so existing `api_jobs`-side call sites are unchanged.
pub use homeboy_lab_contract::lab::workload::JobArtifactMetadata;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobLifecycleMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_cell_count: Option<u64>,
}
