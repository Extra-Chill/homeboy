use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;

mod bench_coverage;
mod browser_evidence_compare;
mod failure_digest;
mod matrix_artifacts;
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
pub use matrix_artifacts::{
    matrix_artifacts_from_args, render_matrix_artifacts_from_args, MatrixArtifactsArgs,
    MatrixArtifactsReport,
};
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
    /// Summarize matrix-style run artifacts and finding packets
    MatrixArtifacts(MatrixArtifactsArgs),
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
    pub matrix_artifacts: Option<MatrixArtifactsReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_compare: Option<ReportCompareReport>,
}

/// The markdown format value every `report` subcommand compares against.
pub const MARKDOWN_FORMAT: &str = "markdown";

impl ReportCommand {
    /// The `--format` value this subcommand was invoked with.
    ///
    /// Every `report` subcommand declares its own `--format`, so the
    /// markdown check used to be written out six times — once per variant —
    /// in `is_markdown_mode` (#11138). Reading the value through one
    /// accessor means a seventh subcommand only has to be added here.
    ///
    /// The six declarations still carry three *different* `value_parser`
    /// sets, which this deliberately does not reconcile:
    ///
    /// - `failure-digest`, `performance-digest`: `["markdown"]`
    /// - `bench-coverage`, `browser-evidence-compare`, `compare`:
    ///   `["markdown", "json"]`
    /// - `matrix-artifacts`: **no `value_parser` at all**, so it accepts
    ///   any string and silently falls through to the JSON envelope for
    ///   anything that is not exactly `markdown`.
    ///
    /// Narrowing `matrix-artifacts` would reject values it accepts today,
    /// and widening the two markdown-only reports would accept values they
    /// reject today. Both are behavior changes, so they are left for a
    /// separate issue.
    pub fn format(&self) -> &str {
        match self {
            ReportCommand::FailureDigest(args) => &args.format,
            ReportCommand::PerformanceDigest(args) => &args.format,
            ReportCommand::BenchCoverage(args) => &args.format,
            ReportCommand::BrowserEvidenceCompare(args) => &args.format,
            ReportCommand::MatrixArtifacts(args) => &args.format,
            ReportCommand::Compare(args) => &args.format,
        }
    }

    /// True when this invocation renders markdown directly.
    pub fn is_markdown(&self) -> bool {
        self.format() == MARKDOWN_FORMAT
    }
}

pub fn is_markdown_mode(args: &ReportArgs) -> bool {
    args.command.is_markdown()
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
        ReportCommand::MatrixArtifacts(matrix_args) => {
            let markdown = render_matrix_artifacts_from_args(&matrix_args)?;
            Ok((markdown, 0))
        }
        ReportCommand::Compare(compare_args) => {
            let markdown = render_report_compare_from_args(&compare_args)?;
            Ok((markdown, 0))
        }
    }
}

pub fn run(args: ReportArgs) -> CmdResult<ReportOutput> {
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
                    matrix_artifacts: None,
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
                    matrix_artifacts: None,
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
                    matrix_artifacts: None,
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
                    matrix_artifacts: None,
                    report_compare: None,
                },
                0,
            ))
        }
        ReportCommand::MatrixArtifacts(matrix_args) => {
            let report = matrix_artifacts_from_args(&matrix_args)?;
            Ok((
                ReportOutput {
                    command: "report.matrix-artifacts".to_string(),
                    markdown: report.markdown.clone(),
                    performance_digest: None,
                    bench_coverage: None,
                    browser_evidence_compare: None,
                    matrix_artifacts: Some(report),
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
                    matrix_artifacts: None,
                    report_compare: Some(report),
                },
                0,
            ))
        }
    }
}
