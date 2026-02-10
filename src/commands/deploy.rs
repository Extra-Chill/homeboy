use clap::Args;
use serde::Serialize;

use homeboy::deploy::{self, ComponentDeployResult, DeployConfig, DeploySummary};
use homeboy::resolve::{infer_project_for_components, resolve_project_components};

use super::CmdResult;

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID (or component ID - order is auto-detected)
    pub project_id: String,

    /// Component IDs to deploy (or project ID if first arg is a component)
    pub component_ids: Vec<String>,

    /// Explicit project ID (takes precedence over positional detection)
    #[arg(long, short = 'p')]
    pub project: Option<String>,

    /// Explicit component IDs (takes precedence over positional)
    #[arg(long, short = 'c')]
    pub component: Option<Vec<String>>,

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
    #[arg(long, visible_alias = "status")]
    pub check: bool,

    /// Deploy even with uncommitted changes
    #[arg(long)]
    pub force: bool,

    /// Deploy to multiple projects (comma-separated or repeated)
    #[arg(long, value_delimiter = ',')]
    pub projects: Option<Vec<String>>,

    /// Deploy to all projects in a fleet
    #[arg(long, short = 'f')]
    pub fleet: Option<String>,
}

#[derive(Serialize)]

pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

#[derive(Serialize)]
pub struct MultiProjectDeployOutput {
    pub command: String,
    pub component_ids: Vec<String>,
    pub projects: Vec<ProjectDeployResult>,
    pub summary: MultiProjectDeploySummary,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
}

#[derive(Serialize)]
pub struct ProjectDeployResult {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

#[derive(Serialize)]
pub struct MultiProjectDeploySummary {
    pub total_projects: u32,
    pub succeeded: u32,
    pub failed: u32,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum DeployCommandOutput {
    Single(DeployOutput),
    Multi(MultiProjectDeployOutput),
}

pub fn run(
    mut args: DeployArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<DeployCommandOutput> {
    // Resolve fleet to project IDs if specified
    if let Some(ref fleet_id) = args.fleet {
        let fl = homeboy::fleet::load(fleet_id)?;
        return run_multi_project(&args, &fl.project_ids);
    }

    // Handle multi-project case first
    if let Some(ref project_ids) = args.projects {
        return run_multi_project(&args, project_ids);
    }

    // Resolve project and component IDs based on flag/positional combinations
    let (project_id, component_ids) = match (&args.project, &args.component) {
        // Both flags provided - use them directly
        (Some(ref proj), Some(ref comps)) => (proj.clone(), comps.clone()),

        // Only --project flag - positionals are components
        (Some(ref proj), None) => {
            let mut comps = vec![args.project_id.clone()];
            comps.extend(args.component_ids.clone());
            (proj.clone(), comps)
        }

        // Only --component flag - resolve project from positional or inference
        (None, Some(ref comps)) => {
            let projects = homeboy::project::list_ids().unwrap_or_default();
            if projects.contains(&args.project_id) {
                (args.project_id.clone(), comps.clone())
            } else {
                // Try to infer project from components
                match infer_project_for_components(comps) {
                    Some(proj) => (proj, comps.clone()),
                    None => {
                        return Err(homeboy::Error::validation_invalid_argument(
                            "project_id",
                            "Could not infer project. Use --project flag or provide project as first argument.",
                            None,
                            None,
                        )
                        .into())
                    }
                }
            }
        }

        // No flags - use shared positional detection
        (None, None) => resolve_project_components(&args.project_id, &args.component_ids)?,
    };

    // Update args with resolved values
    args.project_id = project_id.clone();
    args.component_ids = component_ids;

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
        force: args.force,
        skip_build: false,
    };

    let result = deploy::run(&project_id, &config).map_err(|e| {
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
        DeployCommandOutput::Single(DeployOutput {
            command: "deploy.run".to_string(),
            project_id: project_id.clone(),
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
            results: result.results,
            summary: result.summary,
        }),
        exit_code,
    ))
}

fn run_multi_project(
    args: &DeployArgs,
    project_ids: &[String],
) -> CmdResult<DeployCommandOutput> {
    // Collect component IDs from positional arguments
    let mut component_ids = vec![args.project_id.clone()];
    component_ids.extend(args.component_ids.clone());

    // Filter out empty strings
    let component_ids: Vec<String> = component_ids.into_iter().filter(|s| !s.is_empty()).collect();

    if component_ids.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "component_ids",
            "At least one component ID is required when using --projects",
            None,
            None,
        )
        .into());
    }

    // Validate all specified projects exist
    let known_projects = homeboy::project::list_ids().unwrap_or_default();
    for project_id in project_ids {
        if !known_projects.contains(project_id) {
            return Err(homeboy::Error::validation_invalid_argument(
                "projects",
                &format!("Unknown project: '{}'", project_id),
                None,
                None,
            )
            .into());
        }
    }

    eprintln!(
        "[deploy] Deploying {:?} to {} project(s)...",
        component_ids,
        project_ids.len()
    );

    let mut project_results = Vec::new();
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;
    let mut first_project = true;

    for project_id in project_ids {
        eprintln!("[deploy] Deploying to project '{}'...", project_id);

        let config = DeployConfig {
            component_ids: component_ids.clone(),
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
            skip_build: !first_project, // Build only on first project
        };

        match deploy::run(project_id, &config) {
            Ok(result) => {
                let deploy_failed = result.summary.failed > 0;

                if deploy_failed {
                    let error_msg = result
                        .results
                        .iter()
                        .find_map(|r| r.error.clone())
                        .unwrap_or_else(|| "Deployment failed".to_string());

                    project_results.push(ProjectDeployResult {
                        project_id: project_id.clone(),
                        status: "failed".to_string(),
                        error: Some(error_msg),
                        results: result.results,
                        summary: result.summary,
                    });
                    failed += 1;
                } else {
                    project_results.push(ProjectDeployResult {
                        project_id: project_id.clone(),
                        status: "deployed".to_string(),
                        error: None,
                        results: result.results,
                        summary: result.summary,
                    });
                    succeeded += 1;
                }
            }
            Err(e) => {
                project_results.push(ProjectDeployResult {
                    project_id: project_id.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    results: vec![],
                    summary: DeploySummary {
                        total: 0,
                        succeeded: 0,
                        skipped: 0,
                        failed: 1,
                    },
                });
                failed += 1;
            }
        }

        first_project = false;
    }

    let total = project_results.len() as u32;
    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        DeployCommandOutput::Multi(MultiProjectDeployOutput {
            command: "deploy.run_multi".to_string(),
            component_ids,
            projects: project_results,
            summary: MultiProjectDeploySummary {
                total_projects: total,
                succeeded,
                failed,
            },
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
        }),
        exit_code,
    ))
}
