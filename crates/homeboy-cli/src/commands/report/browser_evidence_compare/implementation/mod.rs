use std::collections::{BTreeMap, BTreeSet};

use super::types::{ArtifactRef, AssertionStats};

mod compare;
mod parse;
mod reader;
mod render;
mod visual;

pub(super) use compare::compare_variants;
pub(super) use reader::read_evidence_dirs;
pub(super) use render::render_markdown;
pub(super) use visual::attach_visual_comparisons;

#[derive(Debug, Clone)]
pub(super) struct EvidenceSet {
    pub(super) samples: Vec<BrowserEvidenceSample>,
    pub(super) artifacts: BTreeSet<ArtifactRef>,
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct BrowserEvidenceSample {
    pub(super) scenario: Option<String>,
    pub(super) profile: Option<String>,
    pub(super) matrix: BTreeMap<String, String>,
    pub(super) assertions: AssertionStats,
    pub(super) request_total: Option<f64>,
    pub(super) request_by_host: BTreeMap<String, f64>,
    pub(super) request_by_type: BTreeMap<String, f64>,
    pub(super) browser_metrics: BTreeMap<String, f64>,
    pub(super) lifecycle_metrics: BTreeMap<String, f64>,
    pub(super) console_errors: Option<f64>,
    pub(super) page_errors: Option<f64>,
    pub(super) artifacts: BTreeSet<ArtifactRef>,
    pub(super) source_artifact: Option<ArtifactRef>,
    pub(super) notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SampleContext {
    pub(super) scenario: Option<String>,
    pub(super) profile: Option<String>,
    pub(super) matrix: BTreeMap<String, String>,
}
