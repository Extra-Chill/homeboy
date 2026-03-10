use clap::Args;
use serde::Serialize;

use homeboy::context;
use homeboy::git::{self, ChangesOutput};
use homeboy::project;
use homeboy::resolve::resolve_project_components;
use homeboy::BulkResult;

use super::CmdResult;

#[derive(Args)]
pub struct ChangesArgs {
    /// Target ID: component ID (single mode) or project ID (if followed by component IDs)
    pub target_id: Option<String>,

    /// Component IDs to filter (when target_id is a project)
    pub component_ids: Vec<String>,

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
    // Priority: --json > --project flag > positional args
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
        // Multiple args: use shared resolver to detect order
        if !args.component_ids.is_empty() {
            let (project_id, component_ids) =
                resolve_project_components(target_id, &args.component_ids)?;
            let output =
                git::changes_project_filtered(&project_id, &component_ids, args.git_diffs)?;
            let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
            return Ok((ChangesCommandOutput::Bulk(output), exit_code));
        }

        // Single target_id: try as component first, fall back to project
        match git::changes(Some(target_id), args.since.as_deref(), args.git_diffs) {
            Ok(output) => return Ok((ChangesCommandOutput::Single(Box::new(output)), 0)),
            Err(e) => {
                if project::exists(target_id) {
                    let output = git::changes_project(target_id, args.git_diffs)?;
                    let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                    return Ok((ChangesCommandOutput::Bulk(output), exit_code));
                }
                return Err(e);
            }
        }
    }

    let (ctx, _) = context::run(None)?;

    // Auto-use when exactly one component is matched
    if ctx.managed && ctx.matched_components.len() == 1 {
        let component_id = &ctx.matched_components[0];
        let output = git::changes(Some(component_id), args.since.as_deref(), args.git_diffs)?;
        return Ok((ChangesCommandOutput::Single(Box::new(output)), 0));
    }

    // Multiple components or unmanaged: return error with helpful hints
    let mut err = homeboy::Error::validation_invalid_argument(
        "input",
        "No component ID provided",
        None,
        None,
    );

    if ctx.managed && ctx.matched_components.len() > 1 {
        err = err.with_hint(format!(
            "Multiple components detected: {}",
            ctx.matched_components.join(", ")
        ));
        err = err.with_hint("Specify one explicitly:");
    } else {
        err = err.with_hint(
            "Run 'homeboy init' to see available components, or specify one explicitly:",
        );
    }
    err = err.with_hint("  homeboy changes <component-id>");

    Err(err)
}
