use std::collections::BTreeMap;

use clap::Args;
use serde::Serialize;

#[derive(Args, Debug, Clone)]
pub struct BrowserEvidenceCompareArgs {
    /// Directory containing baseline browser evidence JSON artifacts
    #[arg(long, value_name = "DIR")]
    pub baseline_dir: String,

    /// Directory containing candidate browser evidence JSON artifacts
    #[arg(long, value_name = "DIR")]
    pub candidate_dir: String,

    /// Label for the baseline artifact set
    #[arg(long, default_value = "baseline")]
    pub baseline_label: String,

    /// Label for the candidate artifact set
    #[arg(long, default_value = "candidate")]
    pub candidate_label: String,

    /// Include local filesystem paths in Markdown output. By default Markdown only uses relative artifact names and URLs.
    #[arg(long)]
    pub include_local_paths: bool,

    /// Output format. Markdown is direct-rendered; JSON uses the normal command envelope.
    #[arg(long, value_parser = ["markdown", "json"], default_value = "markdown")]
    pub format: String,

    /// Run visual screenshot comparisons through a declared visual compare provider.
    #[arg(long)]
    pub visual_compare: bool,

    /// Directory where visual compare artifacts should be written.
    #[arg(long, value_name = "DIR")]
    pub visual_artifacts_dir: Option<String>,

    /// Executable implementing the generic Homeboy visual compare provider contract.
    #[arg(long, value_name = "COMMAND")]
    pub visual_compare_provider: Option<String>,

    /// Extra argument forwarded to the visual compare provider before the input JSON path.
    #[arg(long = "visual-provider-arg", value_name = "ARG")]
    pub visual_provider_args: Vec<String>,

    /// Visual mismatch threshold forwarded to the visual compare provider.
    #[arg(long, value_name = "RATIO")]
    pub visual_threshold: Option<f64>,
}

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
    pub baseline: Vec<ArtifactRef>,
    pub candidate: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArtifactRef {
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
    pub artifacts: Vec<ArtifactRef>,
}

fn is_default_u64(value: &u64) -> bool {
    value.eq(&u64::default())
}
