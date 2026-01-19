use clap::Args;
use serde::Serialize;

use homeboy::deploy::{self, ComponentDeployResult, DeployConfig, DeploySummary};

use super::CmdResult;

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID
    pub project_id: String,

    /// Component IDs to deploy
    pub component_ids: Vec<String>,

    /// JSON input spec for bulk operations
    #[arg(long)]
    pub json: Option<String>,

    /// Deploy all configured components
    #[arg(long)]
    pub all: bool,

    /// Deploy only outdated components
    #[arg(long)]
    pub outdated: bool,

    /// Preview what would be deployed without executing
    #[arg(long)]
    pub dry_run: bool,

    /// Check component status without building or deploying
    #[arg(long)]
    pub check: bool,
}

#[derive(Serialize)]

pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub check: bool,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

pub fn run(mut args: DeployArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DeployOutput> {
    let project_id = &args.project_id;

    // Validate project exists
    let available_projects = homeboy::project::list_ids().unwrap_or_default();
    if !available_projects.contains(project_id) {
        return Err(homeboy::Error::validation_invalid_argument(
            "project_id",
            format!("Project '{}' not found", project_id),
            None,
            Some(vec![format!(
                "Available projects: {}",
                available_projects.join(", ")
            )]),
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
        dry_run: args.dry_run,
        check: args.check,
    };

    let result = deploy::run(project_id, &config).map_err(|e| {
        if e.message.contains("No components configured for project") {
            e.with_hint(format!(
                "Run 'homeboy project components add {} <component-id>' to add components",
                project_id
            ))
            .with_hint("Run 'homeboy init' to see project context and available components")
        } else {
            e
        }
    })?;

    let exit_code = if result.summary.failed > 0 { 1 } else { 0 };

    Ok((
        DeployOutput {
            command: "deploy.run".to_string(),
            project_id: project_id.clone(),
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            check: args.check,
            results: result.results,
            summary: result.summary,
        },
        exit_code,
    ))
}
