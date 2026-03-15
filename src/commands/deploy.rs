use clap::Args;
use serde::Serialize;

use homeboy::deploy::{
    self, ComponentDeployResult, DeployConfig, DeploySummary, MultiDeploySummary,
    ProjectDeployResult,
};

use super::utils::resolve::{infer_project_for_components, resolve_project_components};
use super::CmdResult;

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
    pub summary: MultiDeploySummary,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
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
    // Fleet deploy
    if let Some(ref fleet_id) = args.fleet {
        let fl = homeboy::fleet::load(fleet_id)?;
        let (component_ids, config) = resolve_multi_args(&args)?;
        return run_multi_output(&fl.project_ids, &component_ids, &config, &args);
    }

    // Shared component deploy (find all projects using the component)
    if args.shared {
        let component_ids = resolve_shared_component_ids(&args)?;
        let project_ids = deploy::resolve_shared_targets(&component_ids)?;
        args.component_ids = component_ids;
        args.target_id = None;
        let (component_ids, config) = resolve_multi_args(&args)?;
        return run_multi_output(&project_ids, &component_ids, &config, &args);
    }

    // Multi-project deploy
    if let Some(ref project_ids) = args.projects {
        let (component_ids, config) = resolve_multi_args(&args)?;
        return run_multi_output(project_ids, &component_ids, &config, &args);
    }

    // Single-project deploy: resolve project and component IDs
    let (project_id, component_ids) = resolve_single_deploy_target(&args)?;
    args.target_id = Some(project_id.clone());
    args.component_ids = component_ids;

    // Parse JSON input if provided
    if let Some(ref spec) = args.json {
        args.component_ids = deploy::parse_bulk_component_ids(spec)?;
    }

    let config = build_config(&args, false);

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
            project_id,
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

// === Argument resolution helpers ===

fn resolve_shared_component_ids(args: &DeployArgs) -> homeboy::Result<Vec<String>> {
    if let Some(ref comps) = args.component {
        Ok(comps.clone())
    } else if let Some(ref target) = args.target_id {
        Ok(vec![target.clone()])
    } else {
        Err(homeboy::Error::validation_invalid_argument(
            "component",
            "At least one component ID is required when using --shared",
            None,
            None,
        ))
    }
}

fn resolve_single_deploy_target(args: &DeployArgs) -> homeboy::Result<(String, Vec<String>)> {
    match (&args.project, &args.component, &args.target_id) {
        (Some(proj), Some(comps), _) => Ok((proj.clone(), comps.clone())),

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

            Ok((proj.clone(), comps))
        }

        (None, Some(comps), target) => {
            let projects = homeboy::project::list_ids().unwrap_or_default();

            if let Some(first) = target {
                if projects.contains(first) {
                    return Ok((first.clone(), comps.clone()));
                }
            }

            match infer_project_for_components(comps) {
                Some(proj) => Ok((proj, comps.clone())),
                None => Err(homeboy::Error::validation_invalid_argument(
                    "project_id",
                    "Could not infer project. Use --project flag or provide project as first argument.",
                    None,
                    None,
                )),
            }
        }

        (None, None, Some(target)) => resolve_project_components(target, &args.component_ids),
        (None, None, None) => Err(homeboy::Error::validation_invalid_argument(
            "input",
            "Provide component ID, project ID with --all, or use flags",
            None,
            Some(DEPLOY_RECIPES.iter().map(|r| (*r).to_string()).collect()),
        )),
    }
}

fn resolve_multi_args(args: &DeployArgs) -> homeboy::Result<(Vec<String>, DeployConfig)> {
    let mut component_ids: Vec<String> = Vec::new();
    if let Some(ref target) = args.target_id {
        component_ids.push(target.clone());
    }
    component_ids.extend(args.component_ids.clone());
    let component_ids: Vec<String> = component_ids
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();

    Ok((component_ids, build_config(args, false)))
}

fn build_config(args: &DeployArgs, skip_build: bool) -> DeployConfig {
    DeployConfig {
        component_ids: args.component_ids.clone(),
        all: args.all,
        outdated: args.outdated,
        dry_run: args.dry_run,
        check: args.check,
        force: args.force,
        skip_build,
        keep_deps: args.keep_deps,
        expected_version: args.version.clone(),
        no_pull: args.no_pull,
        head: args.head,
    }
}

fn run_multi_output(
    project_ids: &[String],
    component_ids: &[String],
    config: &DeployConfig,
    args: &DeployArgs,
) -> CmdResult<DeployCommandOutput> {
    let result = deploy::run_multi(project_ids, component_ids, config)?;
    let exit_code = if result.summary.failed > 0 { 1 } else { 0 };

    Ok((
        DeployCommandOutput::Multi(MultiProjectDeployOutput {
            command: "deploy.run_multi".to_string(),
            component_ids: result.component_ids,
            projects: result.projects,
            summary: result.summary,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
        }),
        exit_code,
    ))
}
