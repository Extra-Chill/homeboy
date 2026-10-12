use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEvidenceCompareReport {
    pub command: String,
    pub markdown: String,
    pub baseline_label: String,
    pub candidate_label: String,
    pub totals: BrowserEvidenceCompareTotals,
    pub artifacts: ArtifactComparison,
    pub variants: Vec<BrowserEvidenceVariantComparison>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEvidenceCompareTotals {
    pub baseline_samples: usize,
    pub candidate_samples: usize,
    pub variant_count: usize,
    pub variants_with_baseline: usize,
    pub variants_with_candidate: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEvidenceVariantComparison {
    pub variant: BrowserEvidenceVariant,
    pub baseline_repeats: usize,
    pub candidate_repeats: usize,
    pub assertions: AssertionComparison,
    pub request_totals: MetricComparison,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub request_by_host: BTreeMap<String, MetricComparison>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub request_by_type: BTreeMap<String, MetricComparison>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub browser_metrics: BTreeMap<String, MetricComparison>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lifecycle_metrics: BTreeMap<String, MetricComparison>,
    pub console_errors: MetricComparison,
    pub page_errors: MetricComparison,
    pub artifacts: ArtifactComparison,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_compare: Option<VisualCompareResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrowserEvidenceVariant {
    pub scenario: String,
    pub profile: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub matrix: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AssertionComparison {
    pub baseline: AssertionStats,
    pub candidate: AssertionStats,
    pub pass_delta: i64,
    pub fail_delta: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct AssertionStats {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    #[serde(default, skip_serializing_if = "is_default_u64")]
    pub advisory_failed: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_advisory_assertions: Vec<AssertionFailure>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssertionFailure {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricComparison {
    pub baseline: Option<MetricStats>,
    pub candidate: Option<MetricStats>,
    pub median_delta: Option<f64>,
    pub median_delta_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricStats {
    pub n: usize,
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ArtifactComparison {
    pub baseline: Vec<BrowserEvidenceArtifactLink>,
    pub candidate: Vec<BrowserEvidenceArtifactLink>,
}

/// A labelled link scraped out of a browser-evidence blob.
///
/// Deliberately *not* the canonical `homeboy_core::artifact_ref::ArtifactRef`:
/// this carries no artifact identity at all. It is recovered from arbitrary
/// third-party evidence JSON where the only two things reliably present are a
/// display label and a target path/URL. It is `Ord` because the reader dedupes
/// links in a `BTreeSet`, and `Serialize`-only because it is report output that
/// is never read back.
///
/// It was named `ArtifactRef` until #10310, which made it one of three
/// unrelated types sharing that name across the workspace. Renamed rather than
/// merged: a display link and an addressable artifact reference are different
/// things. The serialized `{label, target}` shape is unchanged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrowserEvidenceArtifactLink {
    pub label: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VisualCompareResult {
    pub status: Option<String>,
    pub mismatch_ratio: Option<f64>,
    pub mismatch_pixels: Option<u64>,
    pub total_pixels: Option<u64>,
    pub dimension_mismatch: Option<bool>,
    pub artifacts_directory: String,
    pub artifacts: Vec<BrowserEvidenceArtifactLink>,
}

fn is_default_u64(value: &u64) -> bool {
    value.eq(&u64::default())
}
