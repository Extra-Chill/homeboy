use clap::Args;
use serde::Serialize;

use homeboy_release::deploy::{
    self, ComponentDeployResult, DeployConfig, DeploySummary, MultiDeploySummary,
    ProjectDeployResult,
};

use super::utils::resolve::{infer_project_for_components, resolve_project_components};
use super::utils::response::{CommandActionableMetadata, CommandNextAction, CommandNextActionKind};
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
    /// Deploy only components whose local version differs from deployed remote
    #[arg(long)]
    pub outdated: bool,
    /// Deploy only components whose local checkout is behind upstream
    #[arg(long, conflicts_with = "outdated")]
    pub behind_upstream: bool,
    /// Preview what would be deployed without executing
    #[arg(long)]
    pub dry_run: bool,
    /// Confirm dangerous deploy modes like --head, --ref, or --force
    #[arg(long)]
    pub apply: bool,
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
    /// Deploy a local build even when its source checkout is behind its upstream
    #[arg(long)]
    pub allow_stale_source: bool,
    /// Deploy a local build even when its semantic version is older than the remote
    #[arg(long)]
    pub allow_downgrade: bool,
    /// Deploy from current branch HEAD instead of the latest tag
    #[arg(long)]
    pub head: bool,
    /// Validate this versioned release-set manifest before any deploy action
    #[arg(long, value_name = "PATH")]
    pub release_set: Option<String>,
    /// Deploy an exact Git ref resolved from the declared component repository
    #[arg(
        long = "ref",
        value_name = "GIT_REF_OR_SHA",
        conflicts_with_all = ["head", "tagged", "version", "outdated", "behind_upstream", "check"]
    )]
    pub requested_ref: Option<String>,
    /// Force local tag-based build/deploy, ignoring reusable release assets
    #[arg(long)]
    pub tagged: bool,
    /// Resume a prior multi-project deploy run after exact identity validation
    #[arg(long, value_name = "RUN_ID")]
    pub resume: Option<String>,
}

#[derive(Serialize)]
pub struct DeployOutput {
    pub command: String,
    pub variant: &'static str,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub behind_upstream: bool,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_set_identity: Option<String>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<CommandActionableMetadata>,
}

#[derive(Serialize)]
pub struct MultiProjectDeployOutput {
    pub command: String,
    pub variant: &'static str,
    pub component_ids: Vec<String>,
    pub projects: Vec<ProjectDeployResult>,
    pub summary: MultiDeploySummary,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_set_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_run_id: Option<String>,
    #[serde(
        rename = "_homeboy_actionable",
        skip_serializing_if = "Option::is_none"
    )]
    pub actionable: Option<CommandActionableMetadata>,
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
    let release_set = args.release_set.as_deref().map(load_release_set).transpose()?;
    if let Some(release_set) = release_set.as_ref() {
        apply_release_set(release_set, &mut args)?;
    }
    validate_apply_boundary(&args)?;

    // Fleet deploy
    if let Some(ref fleet_id) = args.fleet {
        let fl = homeboy::core::fleet::load(fleet_id)?;
        let (component_ids, config) = resolve_multi_args(&args)?;
        return run_multi_output(&fl.project_ids, &component_ids, &config, &args, release_set.as_ref());
    }

    // Shared component deploy (find all projects using the component)
    if args.shared {
        let component_ids = resolve_shared_component_ids(&args)?;
        let project_ids = deploy::resolve_shared_targets(&component_ids)?;
        args.component_ids = component_ids;
        args.target_id = None;
        let (component_ids, config) = resolve_multi_args(&args)?;
        return run_multi_output(&project_ids, &component_ids, &config, &args, release_set.as_ref());
    }

    // Multi-project deploy
    if let Some(ref project_ids) = args.projects {
        let (component_ids, config) = resolve_multi_args(&args)?;
        return run_multi_output(project_ids, &component_ids, &config, &args, release_set.as_ref());
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
            variant: "single",
            project_id: project_id.clone(),
            all: args.all,
            outdated: args.outdated,
            behind_upstream: args.behind_upstream,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
            results: result.results,
            summary: result.summary,
            release_set_identity: release_set.map(|value| value.identity.clone()),
            actionable: Some(deploy_actionable(&project_id)),
        }),
        exit_code,
    ))
}

// === Argument resolution helpers ===

fn validate_apply_boundary(args: &DeployArgs) -> homeboy::core::Result<()> {
    if args.apply
        || args.dry_run
        || args.check
        || (!args.head && args.requested_ref.is_none() && !args.force)
    {
        return Ok(());
    }

    let dangerous_flags = [
        (args.head, "--head"),
        (args.requested_ref.is_some(), "--ref"),
        (args.force, "--force"),
    ]
    .into_iter()
    .filter_map(|(enabled, flag)| enabled.then_some(flag))
    .collect::<Vec<_>>()
    .join(" and ");

    Err(homeboy::core::Error::validation_invalid_argument(
        "apply",
        format!(
            "Real deploys with {dangerous_flags} require explicit --apply. Use --dry-run to preview or re-run with --apply to deploy."
        ),
        None,
        None,
    ))
}

fn load_release_set(path: &str) -> homeboy::core::Result<homeboy_core::release_set::NormalizedReleaseSet> {
    let input = std::fs::read_to_string(path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "release_set",
            format!("Cannot read release-set manifest '{path}': {error}"),
            None,
            None,
        )
    })?;
    homeboy_core::release_set::ReleaseSetManifest::parse_json(&input).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument("release_set", error, None, None)
    })
}

/// Prove every declared component resolves to its caller-supplied exact ref
/// before handing control to deploy orchestration. This is intentionally before
/// lifecycle creation, builds, transfers, or remote actions.
fn apply_release_set(
    release_set: &homeboy_core::release_set::NormalizedReleaseSet,
    args: &mut DeployArgs,
) -> homeboy::core::Result<()> {
    let mut active = Vec::new();
    for entry in &release_set.components {
        match homeboy::core::component::load(&entry.id) {
            Ok(component) => active.push((entry, component)),
            Err(_) if !entry.required => continue,
            Err(error) => {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "release_set",
                    format!("Required component '{}' is unavailable: {}", entry.id, error.message),
                    None,
                    None,
                ));
            }
        }
    }
    if active.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release_set",
            "Release set has no available components to deploy",
            None,
            None,
        ));
    }
    let refs = active
        .iter()
        .map(|(entry, _)| entry.requested_ref.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if refs.len() != 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "release_set",
            "This deploy vertical requires one exact ref shared by every release-set component",
            None,
            None,
        ));
    }
    let requested_ref = refs.into_iter().next().expect("non-empty release set");
    if let Some(ref supplied) = args.requested_ref {
        if supplied != requested_ref {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "ref",
                "--ref must match the release-set component ref",
                None,
                None,
            ));
        }
    }
    for (entry, component) in &active {
        let root = homeboy::core::git::get_git_root(&component.local_path).map_err(|_| {
            homeboy::core::Error::validation_invalid_argument(
                "release_set",
                format!("Component '{}' source is not a Git checkout", entry.id),
                None,
                None,
            )
        })?;
        let status = homeboy::core::git::run_git(
            std::path::Path::new(&root),
            &["status", "--porcelain"],
            "validate release-set source cleanliness",
        )?;
        if !status.trim().is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "release_set",
                format!("Component '{}' source checkout is dirty", entry.id),
                None,
                None,
            ));
        }
        homeboy_release::deploy::preflight_exact_ref(component, &entry.requested_ref)?;
    }
    args.component = Some(active.iter().map(|(entry, _)| entry.id.clone()).collect());
    args.component_ids.clear();
    args.requested_ref = Some(requested_ref.to_string());
    Ok(())
}

fn resolve_shared_component_ids(args: &DeployArgs) -> homeboy::core::Result<Vec<String>> {
    if let Some(ref comps) = args.component {
        Ok(comps.clone())
    } else if let Some(ref target) = args.target_id {
        Ok(vec![target.clone()])
    } else {
        Err(homeboy::core::Error::validation_invalid_argument(
            "component",
            "At least one component ID is required when using --shared",
            None,
            None,
        ))
    }
}

fn resolve_single_deploy_target(args: &DeployArgs) -> homeboy::core::Result<(String, Vec<String>)> {
    match (&args.project, &args.component, &args.target_id) {
        (Some(proj), Some(comps), _) => Ok((proj.clone(), comps.clone())),

        (Some(proj), None, target) => {
            let mut comps = Vec::new();
            if let Some(first) = target {
                comps.push(first.clone());
            }
            comps.extend(args.component_ids.clone());

            let has_selector_flag = args.all
                || args.outdated
                || args.behind_upstream
                || args.check
                || args.json.is_some()
                || args.release_set.is_some();
            if comps.is_empty() && !has_selector_flag {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "input",
                    "Provide component IDs with --project, or add --all/--outdated/--check",
                    None,
                    Some(DEPLOY_RECIPES.iter().map(|r| (*r).to_string()).collect()),
                ));
            }

            Ok((proj.clone(), comps))
        }

        (None, Some(comps), target) => {
            let projects = homeboy::core::project::list_ids().unwrap_or_default();

            if let Some(first) = target {
                if projects.contains(first) {
                    return Ok((first.clone(), comps.clone()));
                }
            }

            match infer_project_for_components(comps) {
                Some(proj) => Ok((proj, comps.clone())),
                None => Err(homeboy::core::Error::validation_invalid_argument(
                    "project_id",
                    "Could not infer project. Use --project flag or provide project as first argument.",
                    None,
                    None,
                )),
            }
        }

        (None, None, Some(target)) => resolve_project_components(target, &args.component_ids),
        (None, None, None) => Err(homeboy::core::Error::validation_invalid_argument(
            "input",
            "Provide component ID, project ID with --all, or use flags",
            None,
            Some(DEPLOY_RECIPES.iter().map(|r| (*r).to_string()).collect()),
        )),
    }
}

fn resolve_multi_args(args: &DeployArgs) -> homeboy::core::Result<(Vec<String>, DeployConfig)> {
    let component_ids = resolve_multi_component_ids(args)?;
    let mut config = build_config(args, false);
    config.component_ids = component_ids.clone();

    Ok((component_ids, config))
}

fn resolve_multi_component_ids(args: &DeployArgs) -> homeboy::core::Result<Vec<String>> {
    if let Some(ref spec) = args.json {
        return Ok(deploy::parse_bulk_component_ids(spec)?
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect());
    }

    if let Some(ref comps) = args.component {
        return Ok(comps.iter().filter(|s| !s.is_empty()).cloned().collect());
    }

    let mut component_ids: Vec<String> = Vec::new();
    if let Some(ref target) = args.target_id {
        component_ids.push(target.clone());
    }
    component_ids.extend(args.component_ids.clone());

    Ok(component_ids
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect())
}

fn build_config(args: &DeployArgs, skip_build: bool) -> DeployConfig {
    DeployConfig {
        component_ids: args.component_ids.clone(),
        all: args.all,
        outdated: args.outdated,
        behind_upstream: args.behind_upstream,
        dry_run: args.dry_run,
        check: args.check,
        force: args.force,
        skip_build,
        keep_deps: args.keep_deps,
        skip_deps_hydration: crate::commands::skip_deps_hydration(),
        expected_version: args.version.clone(),
        no_pull: args.no_pull,
        allow_stale_source: args.allow_stale_source,
        allow_downgrade: args.allow_downgrade,
        head: args.head,
        requested_ref: args.requested_ref.clone(),
        tagged: args.tagged,
        prepared_artifact: None,
        resume_run_id: args.resume.clone(),
    }
}

fn run_multi_output(
    project_ids: &[String],
    component_ids: &[String],
    config: &DeployConfig,
    args: &DeployArgs,
    release_set: Option<&homeboy_core::release_set::NormalizedReleaseSet>,
) -> CmdResult<DeployCommandOutput> {
    let result = deploy::run_multi(project_ids, component_ids, config)?;
    let exit_code = if result.summary.failed > 0 { 1 } else { 0 };

    let actionable = multi_deploy_actionable(&result.projects);
    Ok((
        DeployCommandOutput::Multi(MultiProjectDeployOutput {
            command: "deploy.run_multi".to_string(),
            variant: "multi_project",
            component_ids: result.component_ids,
            projects: result.projects,
            summary: result.summary,
            dry_run: args.dry_run,
            check: args.check,
            force: args.force,
            release_set_identity: release_set.map(|value| value.identity.clone()),
            deploy_run_id: result.deploy_run_id,
            actionable: Some(actionable),
        }),
        exit_code,
    ))
}

fn deploy_actionable(project_id: &str) -> CommandActionableMetadata {
    CommandActionableMetadata::default().with_next_action(
        CommandNextAction::new(
            "check deployment",
            format!("homeboy deploy {project_id} --check"),
        )
        .with_kind(CommandNextActionKind::Show),
    )
}

fn multi_deploy_actionable(projects: &[ProjectDeployResult]) -> CommandActionableMetadata {
    let mut metadata = CommandActionableMetadata::default();
    for project in projects.iter().take(10) {
        metadata.next_actions.push(
            CommandNextAction::new(
                format!("check {}", project.project_id),
                format!("homeboy deploy {} --check", project.project_id),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }
    metadata
}

#[cfg(test)]
#[path = "../../../../tests/commands/deploy_test.rs"]
mod deploy_test;
