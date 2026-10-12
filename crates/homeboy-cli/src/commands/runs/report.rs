use clap::{Args, Subcommand};
use serde::Serialize;

use super::super::CmdResult;

mod browser_evidence_compare;
mod failure_digest;

pub use browser_evidence_compare::{
    browser_evidence_compare_from_dirs, browser_evidence_compare_from_dirs_with_visual,
    browser_evidence_compare_from_dirs_with_visual_and_adapters, BrowserEvidenceCompareReport,
    VisualCompareOptions,
};
pub use failure_digest::{render_failure_digest_from_args, FailureDigestArgs};

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    #[command(subcommand)]
    pub command: ReportCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ReportCommand {
    /// Render a markdown failure digest from Homeboy command output JSON files
    FailureDigest(FailureDigestArgs),
}

#[derive(Serialize)]
pub struct ReportOutput {
    pub command: String,
    pub markdown: String,
}

/// The markdown format value every `report` subcommand compares against.
pub const MARKDOWN_FORMAT: &str = "markdown";

impl ReportCommand {
    /// The `--format` value this subcommand was invoked with.
    pub fn format(&self) -> &str {
        match self {
            ReportCommand::FailureDigest(args) => &args.format,
        }
    }

    /// True when this invocation renders markdown directly.
    pub(crate) fn is_markdown(&self) -> bool {
        self.format() == MARKDOWN_FORMAT
    }
}

pub(crate) fn is_markdown_mode(args: &ReportArgs) -> bool {
    args.command.is_markdown()
}

pub(crate) fn run_markdown(args: ReportArgs) -> CmdResult<String> {
    match args.command {
        ReportCommand::FailureDigest(failure_args) => {
            let markdown = render_failure_digest_from_args(&failure_args)?;
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
                },
                0,
            ))
        }
    }
}
