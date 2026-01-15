use clap::Args;
use serde::Serialize;

use homeboy::deploy::{self, ComponentDeployResult, DeployConfig, DeploySummary};

use super::CmdResult;

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID
    pub project_id: String,

    /// JSON input spec for bulk operations
    #[arg(long)]
    pub json: Option<String>,

    /// Component IDs to deploy
    pub component_ids: Vec<String>,

    /// Deploy all configured components
    #[arg(long)]
    pub all: bool,

    /// Deploy only outdated components
    #[arg(long)]
    pub outdated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

pub fn run(mut args: DeployArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DeployOutput> {
    // Check for common subcommand mistakes
    let subcommand_hints = ["status", "list", "show", "help"];
    if subcommand_hints.contains(&args.project_id.as_str()) {
        return Err(homeboy::Error::validation_invalid_argument(
            "project_id",
            format!(
                "'{}' looks like a subcommand, but 'deploy' doesn't have subcommands. \
                  Usage: homeboy deploy <projectId> [componentIds...] [--all]",
                args.project_id
            ),
            None,
            None,
        ));
    }

    // Parse JSON input if provided
    if let Some(ref spec) = args.json {
        args.component_ids = deploy::parse_bulk_component_ids(spec)?;
    }

    // Build config and call core orchestration
    let config = DeployConfig {
        component_ids: args.component_ids.clone(),
        all: args.all,
        outdated: args.outdated,
    };

    let result = deploy::run(&args.project_id, &config)?;
    let exit_code = if result.summary.failed > 0 { 1 } else { 0 };

    Ok((
        DeployOutput {
            command: "deploy.run".to_string(),
            project_id: args.project_id,
            all: args.all,
            outdated: args.outdated,
            results: result.results,
            summary: result.summary,
        },
        exit_code,
    ))
}
