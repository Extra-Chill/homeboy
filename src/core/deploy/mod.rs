mod effect;
mod execution;
mod generated_artifacts;
mod orchestration;
mod orchestration_ref_checkout;
mod orchestration_tag_checkout;
mod path_roots;
pub(crate) mod permissions;
mod planning;
mod policy;
pub(crate) mod provenance;
pub mod release_download;
mod safety_and_artifact;
mod smoke;
mod transfer;
mod types;
mod version_overrides;

// Public API — re-export types and entry points used outside the deploy module
pub use planning::{
    bucket_release_states, calculate_release_state, calculate_release_state_from_baseline,
    classify_release_state,
};
pub(crate) use types::sha256_file;
pub use types::{
    compare_deployed_versions, parse_bulk_component_ids, ComponentDeployResult, ComponentStatus,
    DeployConfig, DeployOrchestrationResult, DeployReason, DeploySummary, MultiDeployResult,
    MultiDeploySummary, PreparedDeployArtifact, ProjectDeployResult, ReleaseState,
    ReleaseStateBuckets, ReleaseStateStatus,
};
pub use version_overrides::fetch_remote_versions;
pub use version_overrides::{RemoteVersionProbeFailure, RemoteVersionProbeResult};

use crate::core::component;
use crate::core::context::resolve_project_ssh_with_base_path;
use crate::core::error::{Error, Result};
use crate::core::project;

/// High-level deploy entry point. Resolves SSH context internally.
///
/// This is the preferred entry point for callers - it handles project loading
/// and SSH context resolution, keeping those details encapsulated.
pub fn run(project_id: &str, config: &DeployConfig) -> Result<DeployOrchestrationResult> {
    let mut release_artifacts = release_download::ReleaseArtifactStore::default();
    run_with_release_artifacts(project_id, config, &mut release_artifacts)
}

fn run_with_release_artifacts(
    project_id: &str,
    config: &DeployConfig,
    release_artifacts: &mut release_download::ReleaseArtifactStore,
) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    // A version-pinned release asset is resolved remotely before orchestration;
    // requiring its configured checkout to exist would reintroduce a mutable
    // source gate. Other modes retain the existing early local-path validation.
    if config.expected_version.is_none() {
        project::validate_deploy_component_local_paths(&project, &config.component_ids)?;
    }
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    orchestration::deploy_components(config, &project, &ctx, &base_path, release_artifacts)
}

/// Read deployed component versions without running deploy planning or git
/// checks. Status uses this narrow probe to keep timeout diagnostics attached to
/// the affected dashboard component.
pub fn fetch_project_remote_versions(
    project_id: &str,
    components: &[component::Component],
) -> Result<RemoteVersionProbeResult> {
    let project = project::load(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    Ok(version_overrides::fetch_remote_versions_for_project(
        components,
        Some(&project),
        &base_path,
        &ctx.client,
    ))
}

/// Deploy components across multiple projects.
///
/// Builds each project independently unless an upstream workflow supplies a
/// prepared artifact, which is validated once and reused unchanged for every target.
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

    if let Some(prepared_artifact) = config.prepared_artifact.as_ref() {
        for component_id in component_ids {
            prepared_artifact.validate(component_id, config.expected_version.as_deref())?;
        }
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
    let mut release_artifacts = release_download::ReleaseArtifactStore::default();
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
            behind_upstream: config.behind_upstream,
            dry_run: config.dry_run,
            check: config.check,
            force: config.force,
            skip_build: config.skip_build,
            keep_deps: config.keep_deps,
            skip_deps_hydration: config.skip_deps_hydration,
            expected_version: config.expected_version.clone(),
            no_pull: config.no_pull,
            allow_stale_source: config.allow_stale_source,
            allow_downgrade: config.allow_downgrade,
            head: config.head,
            requested_ref: config.requested_ref.clone(),
            tagged: config.tagged,
            prepared_artifact: config.prepared_artifact.clone(),
        };

        match run_with_release_artifacts(project_id, &project_config, &mut release_artifacts) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project::{Project, ProjectComponentAttachment};
    use crate::test_support::with_isolated_home;
    use std::path::Path;

    fn deploy_config() -> DeployConfig {
        DeployConfig {
            component_ids: vec!["plugin".to_string()],
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: true,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            skip_deps_hydration: false,
            expected_version: None,
            no_pull: false,
            allow_stale_source: false,
            allow_downgrade: false,
            head: false,
            requested_ref: None,
            tagged: false,
            prepared_artifact: None,
        }
    }

    #[test]
    fn deploy_planning_fails_closed_when_project_component_local_path_is_missing() {
        with_isolated_home(|_| {
            project::save(&Project {
                id: "site".to_string(),
                server_id: None,
                base_path: Some("/srv/site".to_string()),
                components: vec![ProjectComponentAttachment {
                    id: "plugin".to_string(),
                    local_path: "/tmp/homeboy-missing-component-path".to_string(),
                    remote_path: Some("wp-content/plugins/plugin".to_string()),
                }],
                ..Project::default()
            })
            .expect("save project");

            let err = run("site", &deploy_config()).expect_err("missing local_path should block");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("missing local_path"));
            assert!(err.hints.iter().any(|hint| {
                hint.message.contains(
                    "Component 'plugin' local_path '/tmp/homeboy-missing-component-path' does not exist",
                )
            }));
        });
    }

    #[test]
    fn prepared_artifact_mismatch_fails_before_project_ssh_resolution() {
        with_isolated_home(|_| {
            project::save(&Project {
                id: "site".to_string(),
                ..Project::default()
            })
            .expect("save project");
            let missing_path = Path::new("/definitely/missing/prepared-artifact.zip");
            let config = DeployConfig {
                prepared_artifact: Some(PreparedDeployArtifact {
                    component_id: "plugin".to_string(),
                    path: missing_path.display().to_string(),
                    durable_path: missing_path.display().to_string(),
                    size_bytes: 0,
                    sha256: "not-a-real-sha".to_string(),
                    version: "1.2.3".to_string(),
                    tag: "v1.2.3".to_string(),
                    source_commit: "0123456789abcdef".to_string(),
                }),
                ..deploy_config()
            };

            let error = run_multi(&["site".to_string()], &["plugin".to_string()], &config)
                .expect_err("missing prepared artifact must stop before any target mutation");

            assert!(
                error.details.to_string().contains("prepared-artifact.zip"),
                "prepared artifact validation must fail before SSH resolution: {error:?}"
            );
        });
    }
}
