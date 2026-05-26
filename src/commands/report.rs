use clap::{Args, Subcommand};
use serde::Serialize;

use super::CmdResult;

mod failure_digest;

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

pub fn is_markdown_mode(args: &ReportArgs) -> bool {
    matches!(
        &args.command,
        ReportCommand::FailureDigest(failure_args) if failure_args.format == "markdown"
    )
}

pub fn run_markdown(args: ReportArgs) -> CmdResult<String> {
    match args.command {
        ReportCommand::FailureDigest(failure_args) => {
            let markdown = render_failure_digest_from_args(&failure_args)?;
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
                },
                0,
            ))
        }
    }
}
