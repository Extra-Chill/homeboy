use clap::Args;

pub use homeboy::core::report_compare::ReportCompareReport;

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
    homeboy::core::report_compare::compare_report_artifacts(&args.old, &args.new)
}
