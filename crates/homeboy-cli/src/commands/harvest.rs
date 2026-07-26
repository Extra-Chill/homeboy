use clap::Args;
use serde::Serialize;

use super::utils::resolve::resolve_project_components;
use super::CmdResult;

#[derive(Args)]
pub struct HarvestArgs {
    /// Project ID or component ID (order is auto-detected)
    pub target_id: String,
    /// Additional component IDs or the project ID
    pub component_ids: Vec<String>,
    /// Report remote content drift without writing local files
    #[arg(long)]
    pub check: bool,
    /// Print the remote content delta without writing or committing
    #[arg(long)]
    pub dry_run: bool,
    /// Materialize the reviewed remote delta and commit it
    #[arg(long)]
    pub apply: bool,
    /// Relative glob to exclude. Repeat for multiple patterns.
    #[arg(long)]
    pub exclude: Vec<String>,
    /// Git author for the recovery commit, for example 'Remote agent <agent@example.invalid>'
    #[arg(long)]
    pub author: Option<String>,
}

#[derive(Serialize)]
pub struct HarvestOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub result: homeboy::core::harvest::HarvestResult,
}

pub fn run(args: HarvestArgs, _global: &super::GlobalArgs) -> CmdResult<HarvestOutput> {
    let (project_id, component_ids) =
        resolve_project_components(&args.target_id, &args.component_ids)?;
    let result = homeboy::core::harvest::run(
        &project_id,
        &homeboy::core::harvest::HarvestOptions {
            component_ids,
            check: args.check,
            dry_run: args.dry_run,
            apply: args.apply,
            excludes: args.exclude,
            author: args.author,
        },
    )?;
    let exit = if result.results.iter().any(|result| result.status == "drift") && args.check {
        2
    } else {
        0
    };
    Ok((
        HarvestOutput {
            command: "harvest",
            result,
        },
        exit,
    ))
}
