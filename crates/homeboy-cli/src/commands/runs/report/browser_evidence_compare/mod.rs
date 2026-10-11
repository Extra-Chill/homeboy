use std::path::PathBuf;

use homeboy_core::extension::trace::trace_browser_evidence_adapters;
use homeboy_extension_contract::trace_config::TraceBrowserEvidenceAdapterConfig;

mod implementation;
mod types;

pub use types::{ArtifactComparison, BrowserEvidenceCompareReport, BrowserEvidenceCompareTotals};

pub fn browser_evidence_compare_from_dirs(
    baseline_dirs: &[PathBuf],
    candidate_dirs: &[PathBuf],
    baseline_label: &str,
    candidate_label: &str,
    include_local_paths: bool,
) -> homeboy::core::Result<BrowserEvidenceCompareReport> {
    browser_evidence_compare_from_dirs_with_visual(
        baseline_dirs,
        candidate_dirs,
        baseline_label,
        candidate_label,
        include_local_paths,
        None,
    )
}

pub fn browser_evidence_compare_from_dirs_with_visual(
    baseline_dirs: &[PathBuf],
    candidate_dirs: &[PathBuf],
    baseline_label: &str,
    candidate_label: &str,
    include_local_paths: bool,
    visual_options: Option<VisualCompareOptions>,
) -> homeboy::core::Result<BrowserEvidenceCompareReport> {
    let adapters = trace_browser_evidence_adapters();
    browser_evidence_compare_from_dirs_with_visual_and_adapters(
        baseline_dirs,
        candidate_dirs,
        baseline_label,
        candidate_label,
        include_local_paths,
        visual_options,
        &adapters,
    )
}

pub fn browser_evidence_compare_from_dirs_with_visual_and_adapters(
    baseline_dirs: &[PathBuf],
    candidate_dirs: &[PathBuf],
    baseline_label: &str,
    candidate_label: &str,
    include_local_paths: bool,
    visual_options: Option<VisualCompareOptions>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> homeboy::core::Result<BrowserEvidenceCompareReport> {
    let baseline =
        implementation::read_evidence_dirs(baseline_dirs, include_local_paths, adapters)?;
    let candidate =
        implementation::read_evidence_dirs(candidate_dirs, include_local_paths, adapters)?;
    let mut notes = Vec::new();
    notes.extend(
        baseline
            .notes
            .iter()
            .map(|note| format!("{}: {}", baseline_label, note)),
    );
    notes.extend(
        candidate
            .notes
            .iter()
            .map(|note| format!("{}: {}", candidate_label, note)),
    );

    let artifacts = ArtifactComparison {
        baseline: baseline.artifacts.iter().cloned().collect(),
        candidate: candidate.artifacts.iter().cloned().collect(),
    };
    let mut variants = implementation::compare_variants(&baseline.samples, &candidate.samples);
    if let Some(visual_options) = visual_options {
        let baseline_local = implementation::read_evidence_dirs(baseline_dirs, true, adapters)?;
        let candidate_local = implementation::read_evidence_dirs(candidate_dirs, true, adapters)?;
        let local_variants =
            implementation::compare_variants(&baseline_local.samples, &candidate_local.samples);
        implementation::attach_visual_comparisons(
            &mut variants,
            &local_variants,
            &visual_options,
            baseline_label,
            candidate_label,
            include_local_paths,
        )?;
    }
    let totals = BrowserEvidenceCompareTotals {
        baseline_samples: baseline.samples.len(),
        candidate_samples: candidate.samples.len(),
        variant_count: variants.len(),
        variants_with_baseline: variants
            .iter()
            .filter(|variant| variant.baseline_repeats > 0)
            .count(),
        variants_with_candidate: variants
            .iter()
            .filter(|variant| variant.candidate_repeats > 0)
            .count(),
    };
    let markdown = implementation::render_markdown(
        baseline_label,
        candidate_label,
        &totals,
        &artifacts,
        &variants,
        &notes,
    );

    Ok(BrowserEvidenceCompareReport {
        command: "report.browser-evidence-compare".to_string(),
        markdown,
        baseline_label: baseline_label.to_string(),
        candidate_label: candidate_label.to_string(),
        totals,
        artifacts,
        variants,
        notes,
    })
}

#[derive(Debug, Clone)]
pub struct VisualCompareOptions {
    pub artifacts_dir: PathBuf,
    pub provider_command: String,
    pub provider_args: Vec<String>,
    pub threshold: Option<f64>,
}
