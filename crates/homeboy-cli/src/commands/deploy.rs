use clap::Args;
use serde::Serialize;

use homeboy::context::resolve_project_ssh_with_base_path;
use homeboy::deploy::{self, DeployConfig};
use homeboy::project;

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

    /// Show what would be deployed without executing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployComponentResult {
    pub id: String,
    pub name: String,
    pub status: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub remote_path: Option<String>,
    pub build_command: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploySummary {
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub components: Vec<DeployComponentResult>,
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
                 Usage: homeboy deploy <projectId> [componentIds...] [--all] [--dry-run]",
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

    // Load project and SSH context
    let project = project::load_record(&args.project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(&args.project_id)?;

    // Build config and call core orchestration
    let config = DeployConfig {
        component_ids: args.component_ids.clone(),
        all: args.all,
        outdated: args.outdated,
        dry_run: args.dry_run,
    };

    let result = deploy::deploy_components(&config, &project, &ctx, &base_path)?;

    // Format output
    let components: Vec<DeployComponentResult> = result
        .components
        .into_iter()
        .map(|r| DeployComponentResult {
            id: r.id,
            name: r.name,
            status: r.status,
            local_version: r.local_version,
            remote_version: r.remote_version,
            error: r.error,
            artifact_path: r.artifact_path,
            remote_path: r.remote_path,
            build_command: r.build_command,
            build_exit_code: r.build_exit_code,
            deploy_exit_code: r.deploy_exit_code,
        })
        .collect();

    let exit_code = if result.summary.failed > 0 { 1 } else { 0 };

    Ok((
        DeployOutput {
            command: "deploy.run".to_string(),
            project_id: args.project_id,
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            components,
            summary: DeploySummary {
                succeeded: result.summary.succeeded,
                failed: result.summary.failed,
                skipped: result.summary.skipped,
            },
        },
        exit_code,
    ))
}
