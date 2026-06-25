use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;

mod bench_coverage;
mod browser_evidence_compare;
mod failure_digest;
mod performance_digest;
mod report_compare;

pub use bench_coverage::{
    render_markdown as render_bench_coverage_markdown, BenchCoverageArgs, BenchCoverageReport,
};
pub use browser_evidence_compare::{
    browser_evidence_compare_from_args, browser_evidence_compare_from_dirs,
    browser_evidence_compare_from_dirs_with_visual,
    browser_evidence_compare_from_dirs_with_visual_and_adapters,
    render_browser_evidence_compare_from_args, BrowserEvidenceCompareArgs,
    BrowserEvidenceCompareReport, VisualCompareOptions,
};
pub use failure_digest::{render_failure_digest_from_args, FailureDigestArgs};
pub use performance_digest::{
    performance_digest_from_args, render_performance_digest_from_args, PerformanceDigestArgs,
    PerformanceDigestReport,
};
pub use report_compare::{
    compare_report_artifacts_from_args, render_report_compare_from_args, ReportCompareArgs,
    ReportCompareReport,
};

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    #[command(subcommand)]
    pub command: ReportCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ReportCommand {
    /// Render a markdown failure digest from Homeboy command output JSON files
    FailureDigest(FailureDigestArgs),
    /// Render a generic performance digest from Homeboy run artifacts
    PerformanceDigest(PerformanceDigestArgs),
    /// Report list-only benchmark coverage for hot command paths
    BenchCoverage(BenchCoverageArgs),
    /// Compare before/after browser evidence artifact sets
    BrowserEvidenceCompare(BrowserEvidenceCompareArgs),
    /// Compare structured matrix/report artifacts
    Compare(ReportCompareArgs),
}

#[derive(Serialize)]
pub struct ReportOutput {
    pub command: String,
    pub markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_digest: Option<PerformanceDigestReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bench_coverage: Option<BenchCoverageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_evidence_compare: Option<BrowserEvidenceCompareReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_compare: Option<ReportCompareReport>,
}

pub fn is_markdown_mode(args: &ReportArgs) -> bool {
    matches!(
        &args.command,
        ReportCommand::FailureDigest(failure_args) if failure_args.format == "markdown"
    ) || matches!(
        &args.command,
        ReportCommand::PerformanceDigest(performance_args) if performance_args.format == "markdown"
    ) || matches!(
        &args.command,
        ReportCommand::BenchCoverage(coverage_args) if coverage_args.format == "markdown"
    ) || matches!(
        &args.command,
        ReportCommand::BrowserEvidenceCompare(compare_args) if compare_args.format == "markdown"
    ) || matches!(
        &args.command,
        ReportCommand::Compare(compare_args) if compare_args.format == "markdown"
    )
}

pub fn run_markdown(args: ReportArgs) -> CmdResult<String> {
    match args.command {
        ReportCommand::FailureDigest(failure_args) => {
            let markdown = render_failure_digest_from_args(&failure_args)?;
            Ok((markdown, 0))
        }
        ReportCommand::PerformanceDigest(performance_args) => {
            let markdown = render_performance_digest_from_args(&performance_args)?;
            Ok((markdown, 0))
        }
        ReportCommand::BenchCoverage(coverage_args) => {
            let report = bench_coverage::run(&coverage_args)?;
            Ok((bench_coverage::render_markdown(&report), 0))
        }
        ReportCommand::BrowserEvidenceCompare(compare_args) => {
            let markdown = render_browser_evidence_compare_from_args(&compare_args)?;
            Ok((markdown, 0))
        }
        ReportCommand::Compare(compare_args) => {
            let markdown = render_report_compare_from_args(&compare_args)?;
            Ok((markdown, 0))
        }
    }
}

pub fn run(args: ReportArgs, _global: &super::GlobalArgs) -> CmdResult<ReportOutput> {
    match args.command {
        ReportCommand::FailureDigest(failure_args) => {
            let markdown = render_failure_digest_from_args(&failure_args)?;
            Ok((
                ReportOutput {
                    command: "report.failure-digest".to_string(),
                    markdown,
                    performance_digest: None,
                    bench_coverage: None,
                    browser_evidence_compare: None,
                    report_compare: None,
                },
                0,
            ))
        }
        ReportCommand::PerformanceDigest(performance_args) => {
            let report = performance_digest_from_args(&performance_args)?;
            Ok((
                ReportOutput {
                    command: "report.performance-digest".to_string(),
                    markdown: report.markdown.clone(),
                    performance_digest: Some(report),
                    bench_coverage: None,
                    browser_evidence_compare: None,
                    report_compare: None,
                },
                0,
            ))
        }
        ReportCommand::BenchCoverage(coverage_args) => {
            let report = bench_coverage::run(&coverage_args)?;
            let markdown = bench_coverage::render_markdown(&report);
            Ok((
                ReportOutput {
                    command: "report.bench-coverage".to_string(),
                    markdown,
                    performance_digest: None,
                    bench_coverage: Some(report),
                    browser_evidence_compare: None,
                    report_compare: None,
                },
                0,
            ))
        }
        ReportCommand::BrowserEvidenceCompare(compare_args) => {
            let report = browser_evidence_compare_from_args(&compare_args)?;
            Ok((
                ReportOutput {
                    command: "report.browser-evidence-compare".to_string(),
                    markdown: report.markdown.clone(),
                    performance_digest: None,
                    bench_coverage: None,
                    browser_evidence_compare: Some(report),
                    report_compare: None,
                },
                0,
            ))
        }
        ReportCommand::Compare(compare_args) => {
            let report = compare_report_artifacts_from_args(&compare_args)?;
            Ok((
                ReportOutput {
                    command: "report.compare".to_string(),
                    markdown: report.markdown.clone(),
                    performance_digest: None,
                    bench_coverage: None,
                    browser_evidence_compare: None,
                    report_compare: Some(report),
                },
                0,
            ))
        }
    }
}
