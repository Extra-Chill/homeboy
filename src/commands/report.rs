use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;

mod failure_digest;
mod performance_digest;

pub use failure_digest::{render_failure_digest_from_args, FailureDigestArgs};
pub use performance_digest::{
    performance_digest_from_args, render_performance_digest_from_args, PerformanceDigestArgs,
    PerformanceDigestReport,
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
}

#[derive(Serialize)]
pub struct ReportOutput {
    pub command: String,
    pub markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_digest: Option<PerformanceDigestReport>,
}

pub fn is_markdown_mode(args: &ReportArgs) -> bool {
    matches!(
        &args.command,
        ReportCommand::FailureDigest(failure_args) if failure_args.format == "markdown"
    ) || matches!(
        &args.command,
        ReportCommand::PerformanceDigest(performance_args) if performance_args.format == "markdown"
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
                },
                0,
            ))
        }
    }
}
