use clap::Args;
use serde::Serialize;

use homeboy::git::{self, ChangesOutput};
use homeboy::BulkResult;

use super::CmdResult;

#[derive(Args)]
pub struct ChangesArgs {
    /// Target ID: component ID (single mode) or project ID (if followed by component IDs)
    pub target_id: Option<String>,

    /// Component IDs to filter (when target_id is a project)
    pub component_ids: Vec<String>,

    /// Use current working directory (ad-hoc mode, no component registration required)
    #[arg(long)]
    pub cwd: bool,

    /// Show changes for all components in a project (alternative to positional project mode)
    #[arg(long)]
    pub project: Option<String>,

    /// JSON input spec for bulk operations: {"componentIds": ["id1", "id2"]}
    #[arg(long)]
    pub json: Option<String>,

    /// Compare against specific tag instead of latest
    #[arg(long)]
    pub since: Option<String>,

    /// Include commit range diff in output (uncommitted diff is always included)
    #[arg(long)]
    pub git_diffs: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ChangesCommandOutput {
    Single(Box<ChangesOutput>),
    Bulk(BulkResult<ChangesOutput>),
}

pub fn run(
    args: ChangesArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<ChangesCommandOutput> {
    // Priority: --cwd > --json > --project flag > positional args
    if args.cwd {
        let output = git::changes(None, None, args.git_diffs)?;
        return Ok((ChangesCommandOutput::Single(Box::new(output)), 0));
    }

    if let Some(json) = &args.json {
        let output = git::changes_bulk(json, args.git_diffs)?;
        let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
        return Ok((ChangesCommandOutput::Bulk(output), exit_code));
    }

    // --project flag mode (with optional component filter from positional args)
    if let Some(project_id) = &args.project {
        if args.component_ids.is_empty() {
            let output = git::changes_project(project_id, args.git_diffs)?;
            let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
            return Ok((ChangesCommandOutput::Bulk(output), exit_code));
        } else {
            let output =
                git::changes_project_filtered(project_id, &args.component_ids, args.git_diffs)?;
            let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
            return Ok((ChangesCommandOutput::Bulk(output), exit_code));
        }
    }

    // Positional args mode
    if let Some(target_id) = &args.target_id {
        // If additional component_ids provided, treat target_id as project_id
        if !args.component_ids.is_empty() {
            let output =
                git::changes_project_filtered(target_id, &args.component_ids, args.git_diffs)?;
            let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
            return Ok((ChangesCommandOutput::Bulk(output), exit_code));
        }

        // Single target_id: try as component first
        let output = git::changes(Some(target_id), args.since.as_deref(), args.git_diffs)?;
        return Ok((ChangesCommandOutput::Single(Box::new(output)), 0));
    }

    Err(homeboy::Error::validation_invalid_argument(
        "input",
        "Provide component ID, <project> <components...>, --project, or --json spec",
        None,
        None,
    ))
}
