//! Pure lint + self-check + stream-capture result contract types.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use homeboy_finding::{FindingProducerSummary, HomeboyFinding};

/// Truncation metadata describing how much of a captured stream was retained.
///
/// `seen_bytes` is the total observed length of the source; `retained_bytes`
/// is how many bytes survived the `limit_bytes` cap; `truncated` records
/// whether the source exceeded the cap (so the overflow is observable rather
/// than silently dropped).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamCaptureMetadata {
    pub limit_bytes: usize,
    pub seen_bytes: usize,
    pub retained_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SelfCheckCaptureMetadata {
    pub stdout: StreamCaptureMetadata,
    pub stderr: StreamCaptureMetadata,
}

/// Compact lint summary for automation consumers.
#[derive(Debug, Clone, Serialize)]
pub struct LintSummaryOutput {
    pub total_findings: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub categories: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_findings: Vec<HomeboyFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FormattingFindings {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub suggested_command: String,
}
