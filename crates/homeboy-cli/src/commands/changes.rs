use clap::Args;
use serde::Serialize;

use homeboy::git::{self, BulkChangesOutput, ChangesOutput};

use super::CmdResult;

#[derive(Args)]
pub struct ChangesArgs {
    /// Component ID (single mode)
    pub component_id: Option<String>,

    /// Show changes for all components in a project
    #[arg(long)]
    pub project: Option<String>,

    /// JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
    #[arg(long)]
    pub json: Option<String>,

    /// Compare against specific tag instead of latest
    #[arg(long)]
    pub since: Option<String>,

    /// Include full diff content in output
    #[arg(long)]
    pub include_diff: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ChangesCommandOutput {
    Single(ChangesOutput),
    Bulk(BulkChangesOutput),
}

pub fn run(args: ChangesArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ChangesCommandOutput> {
    // Priority: --json > --project > component_id
    if let Some(json) = &args.json {
        let output = git::changes_bulk(json, args.include_diff)?;
        let exit_code = if output.summary.with_commits > 0 || output.summary.with_uncommitted > 0 {
            0
        } else {
            0
        };
        return Ok((ChangesCommandOutput::Bulk(output), exit_code));
    }

    if let Some(project_id) = &args.project {
        let output = git::changes_project(project_id, args.include_diff)?;
        let exit_code = 0;
        return Ok((ChangesCommandOutput::Bulk(output), exit_code));
    }

    if let Some(component_id) = &args.component_id {
        let output = git::changes(component_id, args.since.as_deref(), args.include_diff)?;
        let exit_code = 0;
        return Ok((ChangesCommandOutput::Single(output), exit_code));
    }

    Err(homeboy::Error::validation_invalid_argument(
        "input",
        "Provide component ID, --project, or --json spec",
        None,
        None,
    ))
}
