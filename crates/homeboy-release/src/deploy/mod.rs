pub(crate) mod binding;
mod effect;
mod execution;
mod generated_artifacts;
mod lifecycle;
mod orchestration;
mod orchestration_ref_checkout;
mod orchestration_tag_checkout;
mod path_roots;
pub(crate) mod permissions;
mod planning;
mod policy;
#[allow(dead_code)] // Internal contract consumed by the follow-up payload integration.
pub(crate) mod preparation;
pub(crate) mod provenance;
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

/// Resolve an exact component source reference for a caller-owned preflight.
/// The resolver is shared with deploy materialization so acceptance criteria do
/// not diverge between a release-set proof and the eventual deploy action.
pub fn preflight_exact_ref(
    component: &component::Component,
    requested_ref: &str,
) -> Result<String> {
    Ok(orchestration_ref_checkout::resolve_exact_ref(component, requested_ref)?.resolved_sha)
}

use homeboy_core::component;
use homeboy_core::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use homeboy_core::error::{Error, Result};
use homeboy_core::phase_timing::PhaseTimer;
use homeboy_core::project;
use uuid::Uuid;

/// High-level deploy entry point. Resolves SSH context internally.
///
/// This is the preferred entry point for callers - it handles project loading
/// and SSH context resolution, keeping those details encapsulated.
pub fn run(project_id: &str, config: &DeployConfig) -> Result<DeployOrchestrationResult> {
    let mut release_artifacts =
        homeboy_core::git::release_download::ReleaseArtifactStore::default();
    run_with_release_artifacts(project_id, config, &mut release_artifacts)
}

fn run_with_release_artifacts(
    project_id: &str,
    config: &DeployConfig,
    release_artifacts: &mut homeboy_core::git::release_download::ReleaseArtifactStore,
) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    // A version-pinned release asset is resolved remotely before orchestration;
    // requiring its configured checkout to exist would reintroduce a mutable
    // source gate. Other modes retain the existing early local-path validation.
    if config.expected_version.is_none() {
        project::validate_deploy_component_local_paths(&project, &config.component_ids)?;
    }
    preflight_prepared_payload_binding(&project, project_id, config)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    orchestration::deploy_components(config, &project, &ctx, &base_path, release_artifacts)
}

/// Bind caller-supplied payloads before SSH context or lifecycle work begins.
/// Locally prepared payloads follow the same binding primitive after preparation
/// retains their process-local ownership guards.
fn preflight_prepared_payload_binding(
    project: &project::Project,
    project_id: &str,
    config: &DeployConfig,
) -> Result<()> {
    let Some(artifact) = config.prepared_artifact.as_ref() else {
        return Ok(());
    };
    let base_path = require_project_base_path(project_id, project)?;
    let components = config
        .component_ids
        .iter()
        .map(|component_id| project::resolve_project_component(project, component_id))
        .collect::<Result<Vec<_>>>()?;
    binding::bind_project_payloads(
        project,
        &base_path,
        &components,
        &std::collections::HashMap::from([(artifact.component_id.clone(), artifact.clone())]),
    )?;
    Ok(())
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
/// Reuses a validated prepared artifact or verified release asset across targets
/// while keeping per-target lifecycle state isolated for resumable deployments.
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
        homeboy_core::log_status!(
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

    // Every supplied payload must bind safely before this multi-target run
    // creates lifecycle state or resolves an SSH context for any project.
    for project_id in &valid_project_ids {
        let project = project::load(project_id)?;
        preflight_prepared_payload_binding(&project, project_id, config)?;
    }

    homeboy_core::log_status!(
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

    let identity = lifecycle_identity(project_ids, component_ids, config);
    let mut lifecycle_run = if config.dry_run || config.check {
        None
    } else if let Some(id) = config.resume_run_id.as_deref() {
        let mut run = lifecycle::load(id)?;
        run.resume(&identity)?;
        lifecycle::save(&run)?;
        Some(run)
    } else {
        let run = lifecycle::DeployLifecycleRun::new(Uuid::new_v4().to_string(), identity);
        lifecycle::save(&run)?;
        Some(run)
    };
    let deploy_run_id = lifecycle_run.as_ref().map(|run| run.id.clone());

    let mut project_results = Vec::new();
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;
    let mut skipped: u32 = unknown_projects.len() as u32;
    let mut planned: u32 = 0;
    let mut release_artifacts =
        homeboy_core::git::release_download::ReleaseArtifactStore::default();
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
            phase_timings: None,
        });
    }

    for project_id in &valid_project_ids {
        homeboy_core::log_status!("deploy", "Deploying to project '{}'...", project_id);

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
            requested_refs: config.requested_refs.clone(),
            tagged: config.tagged,
            prepared_artifact: config.prepared_artifact.clone(),
            resume_run_id: None,
        };

        if lifecycle_run
            .as_ref()
            .is_some_and(|run| run.target_is_succeeded(project_id))
        {
            let mut timer = PhaseTimer::new();
            timer.record_skipped("transfer");
            timer.record_skipped("install");
            timer.record_skipped("verify");
            project_results.push(ProjectDeployResult {
                project_id: project_id.to_string(),
                status: "skipped".to_string(),
                error: Some("Already succeeded in the resumed deploy run".to_string()),
                results: vec![],
                summary: DeploySummary {
                    total: 0,
                    succeeded: 0,
                    failed: 0,
                    skipped: 1,
                },
                phase_timings: Some(timer.into_report()),
            });
            skipped += 1;
            continue;
        }

        if let Some(run) = lifecycle_run.as_mut() {
            run.update_target(
                project_id,
                lifecycle::DeployTargetStatus::Running,
                None,
                None,
            );
            lifecycle::save(run)?;
        }
        let mut timer = PhaseTimer::new();
        let result = timer.time("resolve_source", || {
            run_with_release_artifacts(project_id, &project_config, &mut release_artifacts)
        });
        let timings = timer.into_report();

        match result {
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
                        phase_timings: Some(timings.clone()),
                    });
                    if let Some(run) = lifecycle_run.as_mut() {
                        run.update_target(
                            project_id,
                            lifecycle::DeployTargetStatus::Failed,
                            project_results.last().and_then(|entry| entry.error.clone()),
                            Some(timings.clone()),
                        );
                        lifecycle::save(run)?;
                    }
                    failed += 1;
                } else if is_planned {
                    project_results.push(ProjectDeployResult {
                        project_id: project_id.to_string(),
                        status: "planned".to_string(),
                        error: None,
                        results: result.results,
                        summary: result.summary,
                        phase_timings: Some(timings.clone()),
                    });
                    planned += 1;
                } else {
                    project_results.push(ProjectDeployResult {
                        project_id: project_id.to_string(),
                        status: "deployed".to_string(),
                        error: None,
                        results: result.results,
                        summary: result.summary,
                        phase_timings: Some(timings.clone()),
                    });
                    if let Some(run) = lifecycle_run.as_mut() {
                        run.update_target(
                            project_id,
                            lifecycle::DeployTargetStatus::Succeeded,
                            None,
                            Some(timings.clone()),
                        );
                        lifecycle::save(run)?;
                    }
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
                    phase_timings: Some(timings.clone()),
                });
                if let Some(run) = lifecycle_run.as_mut() {
                    run.update_target(
                        project_id,
                        lifecycle::DeployTargetStatus::Failed,
                        Some(e.to_string()),
                        Some(timings),
                    );
                    lifecycle::save(run)?;
                }
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
        deploy_run_id,
    })
}

fn lifecycle_identity(
    project_ids: &[String],
    component_ids: &[String],
    config: &DeployConfig,
) -> lifecycle::DeployRunIdentity {
    let mut components = component_ids.to_vec();
    components.sort();
    let mut targets = project_ids.to_vec();
    targets.sort();
    let source = if !config.requested_refs.is_empty() {
        config
            .requested_refs
            .iter()
            .map(|(component, reference)| format!("{component}@{reference}"))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        config.requested_ref.clone().unwrap_or_else(|| {
            if config.head {
                "HEAD".to_string()
            } else if let Some(artifact) = config.prepared_artifact.as_ref() {
                format!("{}@{}", artifact.tag, artifact.source_commit)
            } else {
                "release-tag".to_string()
            }
        })
    };
    let artifact = config.prepared_artifact.as_ref().map_or_else(
        || {
            config
                .expected_version
                .clone()
                .unwrap_or_else(|| "resolved-at-preflight".to_string())
        },
        |prepared| format!("sha256:{};size={}", prepared.sha256, prepared.size_bytes),
    );
    lifecycle::DeployRunIdentity {
        source,
        // Artifact selection is an input policy before preparation. Per-component
        // provenance remains in the result after preparation has resolved it.
        artifact,
        components,
        targets,
        policy: format!(
            "all={};outdated={};behind_upstream={};force={};skip_build={};keep_deps={};no_pull={};allow_stale_source={};allow_downgrade={};head={};tagged={}",
            config.all, config.outdated, config.behind_upstream, config.force, config.skip_build,
            config.keep_deps, config.no_pull, config.allow_stale_source, config.allow_downgrade,
            config.head, config.tagged
        ),
    }
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
    use homeboy_core::project::{Project, ProjectComponentAttachment};
    use homeboy_core::test_support::with_isolated_home;
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
            requested_refs: Default::default(),
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
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
