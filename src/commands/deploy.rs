use clap::Args;
use homeboy::log_status;
use serde::Serialize;

use homeboy::deploy::{self, ComponentDeployResult, DeployConfig, DeploySummary};

use super::utils::resolve::{infer_project_for_components, resolve_project_components};
use super::{CmdResult, ProjectsSummary};

const DEPLOY_RECIPES: &[&str] = &[
    "Deploy single component: homeboy deploy <component-id>",
    "Deploy all in project: homeboy deploy <project-id> --all",
    "Flag style: homeboy deploy --project <project> --component <component>",
    "Bulk JSON array: homeboy deploy --project <project> --json '[\"component-a\",\"component-b\"]'",
    "Bulk JSON object: homeboy deploy --project <project> --json '{\"component_ids\":[\"component-a\",\"component-b\"]}'",
];

#[derive(Args)]
pub struct DeployArgs {
    /// Target ID: project ID or component ID (order is auto-detected)
    pub target_id: Option<String>,
    /// Additional component IDs (enables project/component order detection)
    pub component_ids: Vec<String>,
    /// Explicit project ID (takes precedence over positional detection)
    #[arg(long, short = 'p')]
    pub project: Option<String>,
    /// Explicit component IDs (takes precedence over positional)
    #[arg(long, short = 'c')]
    pub component: Option<Vec<String>>,
    /// JSON input spec for bulk operations (array or {"component_ids": [...]})
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
    /// Deploy to all projects using the specified component(s)
    #[arg(long, short = 's')]
    pub shared: bool,
    /// Keep build dependencies (skip post-deploy cleanup)
    #[arg(long)]
    pub keep_deps: bool,
    /// Assert expected version before deploying (abort if local version doesn't match)
    #[arg(long)]
    pub version: Option<String>,
    /// Skip auto-pulling latest changes before deploy
    #[arg(long)]
    pub no_pull: bool,
    /// Deploy from current branch HEAD instead of the latest tag
    #[arg(long)]
    pub head: bool,
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
    pub summary: ProjectsSummary,
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
#[serde(untagged)]
pub enum DeployCommandOutput {
    Single(DeployOutput),
    Multi(MultiProjectDeployOutput),
}

pub fn run(
    mut args: DeployArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<DeployCommandOutput> {
    if let Some(ref fleet_id) = args.fleet {
        let fl = homeboy::fleet::load(fleet_id)?;
        return run_multi_project(&args, &fl.project_ids);
    }

    // Resolve --shared: find all projects using the specified component(s)
    if args.shared {
        let component_ids: Vec<String> = if let Some(ref comps) = args.component {
            comps.clone()
        } else if let Some(ref target) = args.target_id {
            vec![target.clone()]
        } else {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                "At least one component ID is required when using --shared",
                None,
                None,
            ));
        };

        if component_ids.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                "At least one component ID is required when using --shared",
                None,
                None,
            ));
        }

        let mut project_ids: Vec<String> = Vec::new();
        for component_id in &component_ids {
            let using = homeboy::component::projects_using(component_id).unwrap_or_default();
            for pid in using {
                if !project_ids.contains(&pid) {
                    project_ids.push(pid);
                }
            }
        }

        if project_ids.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                format!("No projects found using component(s): {:?}", component_ids),
                None,
                Some(vec![
                    "Run 'homeboy component shared' to see component usage".to_string(),
                ]),
            ));
        }

        args.component_ids = component_ids;
        args.target_id = None;

        return run_multi_project(&args, &project_ids);
    }

    // Handle multi-project case first
    if let Some(ref project_ids) = args.projects {
        return run_multi_project(&args, project_ids);
    }

    // Resolve project and component IDs based on flag/positional combinations
    let (project_id, component_ids) = match (&args.project, &args.component, &args.target_id) {
        // Both flags provided - use them directly (no positional required)
        (Some(proj), Some(comps), _) => (proj.clone(), comps.clone()),

        // Only --project flag - optional positional components
        (Some(proj), None, target) => {
            let mut comps = Vec::new();
            if let Some(first) = target {
                comps.push(first.clone());
            }
            comps.extend(args.component_ids.clone());

            let has_selector_flag = args.all || args.outdated || args.check || args.json.is_some();
            if comps.is_empty() && !has_selector_flag {
                return Err(homeboy::Error::validation_invalid_argument(
                    "input",
                    "Provide component IDs with --project, or add --all/--outdated/--check",
                    None,
                    Some(DEPLOY_RECIPES.iter().map(|r| (*r).to_string()).collect()),
                ));
            }

            (proj.clone(), comps)
        }

        // Only --component flag - optional positional project, else infer
        (None, Some(comps), target) => {
            let projects = homeboy::project::list_ids().unwrap_or_default();

            if let Some(first) = target {
                if projects.contains(first) {
                    (first.clone(), comps.clone())
                } else {
                    match infer_project_for_components(comps) {
                        Some(proj) => (proj, comps.clone()),
                        None => {
                            return Err(homeboy::Error::validation_invalid_argument(
                                "project_id",
                                "Could not infer project. Use --project flag or provide project as first argument.",
                                None,
                                None,
                            ));
                        }
                    }
                }
            } else {
                match infer_project_for_components(comps) {
                    Some(proj) => (proj, comps.clone()),
                    None => {
                        return Err(homeboy::Error::validation_invalid_argument(
                            "project_id",
                            "Could not infer project. Use --project flag or provide project as first argument.",
                            None,
                            None,
                        ));
                    }
                }
            }
        }

        // No flags - positional args required
        (None, None, Some(target)) => resolve_project_components(target, &args.component_ids)?,
        (None, None, None) => {
            return Err(homeboy::Error::validation_invalid_argument(
                "input",
                "Provide component ID, project ID with --all, or use flags",
                None,
                Some(DEPLOY_RECIPES.iter().map(|r| (*r).to_string()).collect()),
            ));
        }
    };

    // Update args with resolved values
    args.target_id = Some(project_id.clone());
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
        keep_deps: args.keep_deps,
        expected_version: args.version.clone(),
        no_pull: args.no_pull,
        head: args.head,
    };

    let result = deploy::run(&project_id, &config).map_err(|e| {
        if e.message.contains("No components configured for project")
            || e.message.contains("No deployable components found")
        {
            e.with_hint(format!(
                "Run 'homeboy project components add {} <component-id>' to add components",
                project_id
            ))
            .with_hint(
                "Run 'homeboy status --full' to see project context and available components",
            )
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

fn run_multi_project(args: &DeployArgs, project_ids: &[String]) -> CmdResult<DeployCommandOutput> {
    // Collect component IDs from positional arguments
    let mut component_ids: Vec<String> = Vec::new();
    if let Some(ref target) = args.target_id {
        component_ids.push(target.clone());
    }
    component_ids.extend(args.component_ids.clone());

    // Filter out empty strings
    let component_ids: Vec<String> = component_ids
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();

    if component_ids.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "component_ids",
            "At least one component ID is required for multi-project deployment",
            None,
            None,
        ));
    }

    // Validate specified projects exist, skip unknown ones instead of aborting.
    // Fleet configs can accumulate stale project references — one missing project
    // should not block deploying to the rest.
    let known_projects = homeboy::project::list_ids().unwrap_or_default();
    let mut unknown_projects = Vec::new();
    let valid_project_ids: Vec<&String> = project_ids
        .iter()
        .filter(|pid| {
            if known_projects.contains(pid) {
                true
            } else {
                unknown_projects.push(pid.to_string());
                false
            }
        })
        .collect();

    for pid in &unknown_projects {
        log_status!(
            "deploy",
            "Skipping unknown project '{}' — remove from fleet with: homeboy fleet remove-project <fleet> {}",
            pid,
            pid
        );
    }

    if valid_project_ids.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "projects",
            format!(
                "No valid projects found — all specified projects are unknown: {}",
                unknown_projects.join(", ")
            ),
            None,
            None,
        ));
    }

    log_status!(
        "deploy",
        "Deploying {:?} to {} project(s){}...",
        component_ids,
        valid_project_ids.len(),
        if unknown_projects.is_empty() {
            String::new()
        } else {
            format!(" ({} skipped)", unknown_projects.len())
        }
    );

    let mut project_results = Vec::new();
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;
    let skipped: u32 = unknown_projects.len() as u32;
    let mut planned: u32 = 0;
    let mut first_project = true;

    // Add skipped results for unknown projects
    for pid in &unknown_projects {
        project_results.push(ProjectDeployResult {
            project_id: pid.clone(),
            status: "skipped".to_string(),
            error: Some(format!("Project '{}' not found — skipped", pid)),
            results: vec![],
            summary: DeploySummary {
                total: 0,
                succeeded: 0,
                skipped: 0,
                failed: 0,
            },
        });
    }

    for project_id in &valid_project_ids {
        log_status!("deploy", "Deploying to project '{}'...", project_id);

        let config = DeployConfig {
            component_ids: component_ids.clone(),
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
            skip_build: !first_project, // Build only on first project
            keep_deps: args.keep_deps,
            expected_version: args.version.clone(),
            no_pull: args.no_pull || !first_project, // Only pull on first project
            head: args.head,
        };

        match deploy::run(project_id, &config) {
            Ok(result) => {
                let deploy_failed = result.summary.failed > 0;

                // Determine the correct project-level status:
                // - dry-run/check modes never actually deploy, so report "planned"
                // - real deploys report "deployed" or "failed" based on results
                let is_planned = args.dry_run || args.check;

                if deploy_failed {
                    let error_msg = result
                        .results
                        .iter()
                        .find_map(|r| r.error.clone())
                        .unwrap_or_else(|| "Deployment failed".to_string());

                    project_results.push(ProjectDeployResult {
                        project_id: project_id.to_string(),
                        status: "failed".to_string(),
                        error: Some(error_msg),
                        results: result.results,
                        summary: result.summary,
                    });
                    failed += 1;
                } else if is_planned {
                    project_results.push(ProjectDeployResult {
                        project_id: project_id.to_string(),
                        status: "planned".to_string(),
                        error: None,
                        results: result.results,
                        summary: result.summary,
                    });
                    planned += 1;
                } else {
                    project_results.push(ProjectDeployResult {
                        project_id: project_id.to_string(),
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
                    project_id: project_id.to_string(),
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
            summary: ProjectsSummary {
                total_projects: total,
                succeeded,
                failed,
                skipped,
                planned,
            },
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
        }),
        exit_code,
    ))
}
