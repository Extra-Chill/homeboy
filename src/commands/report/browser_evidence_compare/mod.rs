use std::path::PathBuf;

use homeboy::core::extension::trace::trace_browser_evidence_adapters;
use homeboy::core::extension::TraceBrowserEvidenceAdapterConfig;

mod implementation;
mod types;

pub use types::{
    ArtifactComparison, BrowserEvidenceCompareArgs, BrowserEvidenceCompareReport,
    BrowserEvidenceCompareTotals,
};

pub fn render_browser_evidence_compare_from_args(
    args: &BrowserEvidenceCompareArgs,
) -> homeboy::core::Result<String> {
    browser_evidence_compare_from_args(args).map(|report| report.markdown)
}

pub fn browser_evidence_compare_from_args(
    args: &BrowserEvidenceCompareArgs,
) -> homeboy::core::Result<BrowserEvidenceCompareReport> {
    let baseline_dir = PathBuf::from(&args.baseline_dir);
    let candidate_dir = PathBuf::from(&args.candidate_dir);
    browser_evidence_compare_from_dirs_with_visual(
        &[baseline_dir],
        &[candidate_dir],
        &args.baseline_label,
        &args.candidate_label,
        args.include_local_paths,
        visual_compare_options(args)?,
    )
}

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

fn visual_compare_options(
    args: &BrowserEvidenceCompareArgs,
) -> homeboy::core::Result<Option<VisualCompareOptions>> {
    if !args.visual_compare {
        return Ok(None);
    }
    let Some(provider_command) = args.visual_compare_provider.clone() else {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--visual-compare-provider".to_string(),
        ]));
    };
    Ok(Some(VisualCompareOptions {
        artifacts_dir: args
            .visual_artifacts_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".homeboy/browser-visual-compare")),
        provider_command,
        provider_args: args.visual_provider_args.clone(),
        threshold: args.visual_threshold,
    }))
}
