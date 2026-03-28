mod execution;
mod orchestration;
pub(crate) mod permissions;
mod planning;
pub(crate) mod provenance;
pub mod release_download;
mod safety_and_artifact;
mod transfer;
mod types;
mod version_overrides;

// Public API — re-export types and entry points used outside the deploy module
pub use planning::{bucket_release_states, calculate_release_state, classify_release_state};
pub use types::{
    parse_bulk_component_ids, ComponentDeployResult, ComponentStatus, DeployConfig,
    DeployOrchestrationResult, DeployReason, DeploySummary, MultiDeployResult, MultiDeploySummary,
    ProjectDeployResult, ReleaseState, ReleaseStateBuckets, ReleaseStateStatus,
};
pub use version_overrides::fetch_remote_versions;

use crate::component;
use crate::context::resolve_project_ssh_with_base_path;
use crate::error::{Error, Result};
use crate::project;

/// High-level deploy entry point. Resolves SSH context internally.
///
/// This is the preferred entry point for callers - it handles project loading
/// and SSH context resolution, keeping those details encapsulated.
pub fn run(project_id: &str, config: &DeployConfig) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    orchestration::deploy_components(config, &project, &ctx, &base_path)
}

/// Deploy components across multiple projects.
///
/// Handles the build-skip optimization: only the first project builds
/// from source; subsequent projects reuse the already-built artifact.
/// Similarly, only the first project pulls latest changes.
///
/// Unknown project IDs are skipped (not fatal) — fleet configs can
/// accumulate stale references that shouldn't block the rest.
pub fn run_multi(
    project_ids: &[String],
    component_ids: &[String],
    config: &DeployConfig,
) -> Result<MultiDeployResult> {
    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component_ids",
            "At least one component ID is required for multi-project deployment",
            None,
            None,
        ));
    }

    // Validate project IDs, skip unknown ones
    let known_projects = project::list_ids().unwrap_or_default();
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
        return Err(Error::validation_invalid_argument(
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

    // Record skipped results for unknown projects
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

        let project_config = DeployConfig {
            component_ids: component_ids.to_vec(),
            all: config.all,
            outdated: config.outdated,
            dry_run: config.dry_run,
            check: config.check,
            force: config.force,
            // Build-skip optimization: only build on first project
            skip_build: config.skip_build || !first_project,
            keep_deps: config.keep_deps,
            expected_version: config.expected_version.clone(),
            // Only pull on first project
            no_pull: config.no_pull || !first_project,
            head: config.head,
            tagged: config.tagged,
        };

        match run(project_id, &project_config) {
            Ok(result) => {
                let deploy_failed = result.summary.failed > 0;
                let is_planned = config.dry_run || config.check;

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

    let total_projects = project_results.len() as u32;

    Ok(MultiDeployResult {
        component_ids: component_ids.to_vec(),
        projects: project_results,
        summary: MultiDeploySummary {
            total_projects,
            succeeded,
            failed,
            skipped,
            planned,
        },
    })
}

/// Find all projects that use any of the specified components.
///
/// Used by `--shared` flag to deploy a component to every project that has it.
pub fn resolve_shared_targets(component_ids: &[String]) -> Result<Vec<String>> {
    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            "At least one component ID is required when using --shared",
            None,
            None,
        ));
    }

    let mut project_ids: Vec<String> = Vec::new();
    for component_id in component_ids {
        let using = component::projects_using(component_id).unwrap_or_default();
        for pid in using {
            if !project_ids.contains(&pid) {
                project_ids.push(pid);
            }
        }
    }

    if project_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component",
            format!("No projects found using component(s): {:?}", component_ids),
            None,
            Some(vec![
                "Run 'homeboy component shared' to see component usage".to_string(),
            ]),
        ));
    }

    Ok(project_ids)
}
