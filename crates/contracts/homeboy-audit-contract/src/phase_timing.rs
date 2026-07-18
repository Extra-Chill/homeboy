//! Extension runner phase-timing value type.
//!
//! A pure serde value type describing one phase of an extension runner's work
//! (build/lint/test), with duration, an opaque provider-declared status, a
//! human-readable message, artifacts, and free-form metadata. Core treats every
//! field as opaque data — it never infers tool-specific behavior from `status`
//! or `message`. `extension::runner_contract` re-exports this and owns the
//! runners that produce it; `code_audit` consumes it in its report output.

use std::collections::BTreeMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ExtensionPhaseTiming {
    pub name: String,
    pub duration_ms: u64,
    /// Provider-declared generic state for this phase, for example `running`,
    /// `waiting`, `blocked`, `queued`, `passed`, or `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Human-readable provider summary for the phase. Core treats this as
    /// opaque text and does not infer tool-specific behavior from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}
