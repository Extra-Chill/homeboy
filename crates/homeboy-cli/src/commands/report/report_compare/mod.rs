//! `homeboy report compare` — the clap surface plus the artifact-diff engine.
//!
//! The engine and its markdown renderer used to sit in `homeboy-core` as two
//! top-level modules (`report_compare`, `report_compare_render`) even though
//! this command was their only consumer and the renderer was already
//! `pub(crate)`. They live with their caller now (#11143).

mod engine;
mod render;

use clap::Args;

pub use engine::{
    compare_report_artifacts, compare_report_artifacts_with_store, ReportCompareReport,
};

#[derive(Args, Debug, Clone)]
pub struct ReportCompareArgs {
    /// Baseline artifact input: local JSON path, run id, or run:artifact / run/artifact ref
    #[arg(long, value_name = "RUN_OR_ARTIFACT")]
    pub old: String,

    /// Candidate artifact input: local JSON path, run id, or run:artifact / run/artifact ref
    #[arg(long, value_name = "RUN_OR_ARTIFACT")]
    pub new: String,

    /// Output format
    #[arg(long, value_parser = ["markdown", "json"], default_value = "markdown")]
    pub format: String,
}

pub fn render_report_compare_from_args(args: &ReportCompareArgs) -> homeboy::core::Result<String> {
    compare_report_artifacts_from_args(args).map(|report| report.markdown)
}

pub fn compare_report_artifacts_from_args(
    args: &ReportCompareArgs,
) -> homeboy::core::Result<ReportCompareReport> {
    compare_report_artifacts(&args.old, &args.new)
}
