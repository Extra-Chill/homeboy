#[allow(
    dead_code,
    reason = "Deployment binding evidence supports optional planning paths."
)]
pub(crate) mod binding;
mod content_manifest;
mod effect;
mod execution;
mod generated_artifacts;
mod lifecycle;
mod orchestration;
mod orchestration_ref_checkout;
mod orchestration_tag_checkout;
mod path_roots;
pub(crate) mod permissions;
#[allow(
    dead_code,
    reason = "Component loading supports optional deployment planning."
)]
mod planning;
mod policy;
#[allow(
    dead_code,
    reason = "Payload preparation evidence supports optional deployment execution."
)]
pub(crate) mod preparation;
pub(crate) mod provenance;
mod provider;
pub mod provider_impl;
mod receipt;
mod route;
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
// The CLI selects a deployment target explicitly, so this crosses the boundary.
pub use route::DeployTarget;
// `homeboy-release` reads artifact digests through this when projecting a
// release deployment, so it crosses the crate boundary now.
pub use types::sha256_file;
pub use types::{
    compare_deployed_versions, parse_bulk_component_ids, ComponentDeployResult, ComponentStatus,
    DeployConfig, DeployOrchestrationResult, DeployReason, DeploySummary, MultiDeployResult,
    MultiDeploySummary, PreparedDeployArtifact, PreparedDeployProjection, ProjectDeployResult,
    ReleaseState, ReleaseStateBuckets, ReleaseStateStatus, VersionSource, VersionSources,
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

/// Resolve every exact source identity before a caller crosses a mutation boundary.
/// Remote inspection uses `ls-remote`, so this cannot update configured checkouts.
pub fn preflight_exact_refs(
    refs: &[(&component::Component, &str)],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut refs = refs.to_vec();
    refs.sort_by(|(left_component, left_ref), (right_component, right_ref)| {
        left_component
            .id
            .cmp(&right_component.id)
            .then_with(|| left_ref.cmp(right_ref))
    });

    let mut resolved = std::collections::BTreeMap::new();
    let mut failures = Vec::new();
    let mut component_counts = std::collections::BTreeMap::new();
    for (component, _) in &refs {
        *component_counts
            .entry(component.id.as_str())
            .or_insert(0usize) += 1;
    }
    for (component_id, count) in component_counts {
        if count > 1 {
            failures.push(format!(
                "component '{component_id}' appears {count} times in the ref preflight"
            ));
        }
    }
    for (component, requested_ref) in refs {
        match orchestration_ref_checkout::resolve_exact_ref(component, requested_ref) {
            Ok(identity) => {
                resolved
                    .entry(component.id.clone())
                    .or_insert(identity.resolved_sha);
            }
            Err(error) => failures.push(format!(
                "component '{}' ref '{}': {}",
                component.id, requested_ref, error.message
            )),
        }
    }

    if failures.is_empty() {
        Ok(resolved)
    } else {
        Err(Error::validation_invalid_argument(
            "ref",
            format!(
                "Release-set ref preflight failed for every unresolved component: {}",
                failures.join("; ")
            ),
            None,
            None,
        ))
    }
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
    // Single-project deploy is its own unit of work: resolve once here so the
    // receipt read, write, and invalidation below all address one home (#7505).
    let roots = homeboy_core::paths::PathRoots::from_environment()?;
    run_with_release_artifacts(roots.data(), project_id, config, &mut release_artifacts)
}

fn run_with_release_artifacts(
    data_root: &std::path::Path,
    project_id: &str,
    config: &DeployConfig,
    release_artifacts: &mut homeboy_core::git::release_download::ReleaseArtifactStore,
) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    let source =
        lifecycle_identity(&[project_id.to_string()], &config.component_ids, config).source;
    let mut observation = should_observe_deploy(config)
        .then(|| lifecycle::DeployObservation::start(project_id, &source))
        .transpose()?;
    let admitted_run_id = observation.as_ref().map(|run| run.run_id().to_string());
    if let Some(mut result) =
        provider::run_if_configured(project_id, &project, config, observation.as_mut())
            .map_err(|error| attach_admitted_run_id(error, admitted_run_id.as_deref()))?
    {
        if let Some(observation) = observation.as_mut() {
            observation.finish(
                if result.summary.failed == 0 {
                    homeboy_core::observation::RunStatus::Pass
                } else {
                    homeboy_core::observation::RunStatus::Fail
                },
                (result.summary.failed > 0)
                    .then_some("deployment provider reported failure".to_string()),
            );
            result.deploy_run_id = Some(observation.run_id().to_string());
        }
        return Ok(result);
    }
    // A version-pinned release asset is resolved remotely before orchestration;
    // requiring its configured checkout to exist would reintroduce a mutable
    // source gate. Other modes retain the existing early local-path validation.
    //
    // `--check` is read-only and returns before any build or remote write, so
    // this fail-closed gate protects nothing there — it only aborts the status
    // pass and hides every other component (#12214). Check mode reports each
    // absent checkout as a scoped skipped result instead.
    if !config.check
        && config.expected_version.is_none()
        && config.prepared_projection.is_none()
        && config.prepared_artifact.is_none()
    {
        project::validate_deploy_component_local_paths(&project, &config.component_ids)
            .map_err(|error| attach_admitted_run_id(error, admitted_run_id.as_deref()))?;
    }
    preflight_prepared_payload_binding(&project, project_id, config)
        .map_err(|error| attach_admitted_run_id(error, admitted_run_id.as_deref()))?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)
        .map_err(|error| attach_admitted_run_id(error, admitted_run_id.as_deref()))?;
    let mut result = orchestration::deploy_components(
        data_root,
        config,
        &project,
        &ctx,
        &base_path,
        release_artifacts,
        observation.as_mut(),
    )
    .map_err(|error| attach_admitted_run_id(error, admitted_run_id.as_deref()))?;
    disclose_server_routes(&project, config, &mut result);
    if let Some(observation) = observation.as_mut() {
        observation.finish(
            if result.summary.failed == 0 {
                homeboy_core::observation::RunStatus::Pass
            } else {
                homeboy_core::observation::RunStatus::Fail
            },
            (result.summary.failed > 0)
                .then_some("deployment reported one or more failures".to_string()),
        );
        result.deploy_run_id = Some(observation.run_id().to_string());
    }
    Ok(result)
}

/// Record which deliverable each dual-deliverable component actually deployed.
///
/// Everything reaching here took the server route; the provider route returns
/// from `provider::run_if_configured` above and labels itself. A component with
/// one deliverable had no choice to report, so only a component that also
/// declares a deployment provider is annotated (#12853). Resolution failures
/// are skipped: this is disclosure on a completed deploy, never a new gate.
fn disclose_server_routes(
    project: &project::Project,
    config: &DeployConfig,
    result: &mut DeployOrchestrationResult,
) {
    for row in &mut result.results {
        if row
            .warnings
            .iter()
            .any(|warning| warning.starts_with("deployment route:"))
        {
            continue;
        }
        let Ok(component) = planning::resolve_project_component(
            project,
            &row.id,
            None,
            config.prepared_projection.as_ref(),
        ) else {
            continue;
        };
        if let Some(disclosure) = route::server_route_disclosure(&component, project, config) {
            row.warnings.push(disclosure);
        }
    }
}

fn attach_admitted_run_id(mut error: Error, run_id: Option<&str>) -> Error {
    if let (Some(run_id), Some(details)) = (run_id, error.details.as_object_mut()) {
        details.insert(
            "deploy_run_id".to_string(),
            serde_json::Value::String(run_id.to_string()),
        );
    }
    error
}

/// Read-only planning modes do not create a deployment lifecycle. Every mode
/// that can build or mutate a target is admitted before any expensive source work.
fn should_observe_deploy(config: &DeployConfig) -> bool {
    !config.dry_run && !config.check
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
        .map(|component_id| {
            planning::resolve_project_component(
                project,
                component_id,
                None,
                config.prepared_projection.as_ref(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let payloads =
        std::collections::HashMap::from([(artifact.component_id.clone(), artifact.clone())]);
    match binding::bind_project_payloads(project, &base_path, &components, &payloads) {
        Ok(_) => return Ok(()),
        Err(error)
            if error
                .details
                .get("field")
                .and_then(serde_json::Value::as_str)
                == Some("remotePath")
                && error
                    .details
                    .get("problem")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|problem| problem.contains("requires path_root")) =>
        {
            // Detector-backed roots require read-only remote inspection before binding.
        }
        Err(error) => return Err(error),
    }

    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    let project = path_roots::project_with_detected_path_roots(
        project,
        &components,
        &base_path,
        &ctx.client,
        "deploy",
    );
    binding::bind_project_payloads(&project, &base_path, &components, &payloads)?;
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

/// Deadline-aware remote-version probe for interactive project status.
pub fn fetch_project_remote_versions_with_deadline(
    project_id: &str,
    components: &[component::Component],
    deadline: std::time::Instant,
) -> Result<RemoteVersionProbeResult> {
    let project = project::load(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    Ok(
        version_overrides::fetch_remote_versions_for_project_with_deadline(
            components,
            Some(&project),
            &base_path,
            &ctx.client,
            deadline,
        ),
    )
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

    // One resolution for the whole run. Every checkpoint read and write below
    // addresses this data root, so a resumed run cannot read its checkpoint
    // from one home and then record target outcomes into another (#7505). The
    // observation store admitted alongside it still resolves its own roots
    // inside `homeboy-core`, which is not reachable from here.
    let roots = homeboy_core::paths::PathRoots::from_environment()?;
    let identity = lifecycle_identity(project_ids, component_ids, config);
    let mut lifecycle_run = if config.dry_run || config.check {
        None
    } else if let Some(id) = config.resume_run_id.as_deref() {
        let mut run = lifecycle::load_in_roots(roots.data(), id)?;
        run.resume(&identity)?;
        lifecycle::save_in_roots(roots.data(), &run)?;
        Some(run)
    } else {
        let run = lifecycle::DeployLifecycleRun::new(Uuid::new_v4().to_string(), identity.clone());
        lifecycle::save_in_roots(roots.data(), &run)?;
        Some(run)
    };
    let checkpoint_run_id = lifecycle_run.as_ref().map(|run| run.id.clone());
    let mut aggregate_observation = checkpoint_run_id
        .as_deref()
        .map(|id| {
            if config.resume_run_id.is_some() {
                lifecycle::DeployObservation::start("multi", &identity.source)
            } else {
                lifecycle::DeployObservation::start_with_id(Some(id), "multi", &identity.source)
            }
        })
        .transpose()?;
    if let (Some(aggregate), Some(prior_checkpoint)) = (
        aggregate_observation.as_mut(),
        config.resume_run_id.as_deref(),
    ) {
        aggregate.link_resume(prior_checkpoint)?;
    }
    let deploy_run_id = aggregate_observation
        .as_ref()
        .map(|run| run.run_id().to_string());

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
            observation_run_id: None,
        });
    }

    for project_id in &valid_project_ids {
        homeboy_core::log_status!("deploy", "Deploying to project '{}'...", project_id);

        // Per-target config is the caller's config with exactly two deltas:
        // the explicitly requested component set, and a cleared resume id so
        // each project target starts its own lifecycle run. Everything else is
        // carried by `..config.clone()` so a new `DeployConfig` field can never
        // be silently dropped here.
        let project_config = DeployConfig {
            component_ids: component_ids.to_vec(),
            resume_run_id: None,
            ..config.clone()
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
                observation_run_id: None,
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
            lifecycle::save_in_roots(roots.data(), run)?;
        }
        let mut timer = PhaseTimer::new();
        let result = timer.time("resolve_source", || {
            run_with_release_artifacts(
                roots.data(),
                project_id,
                &project_config,
                &mut release_artifacts,
            )
        });
        let timings = timer.into_report();

        match result {
            Ok(result) => {
                let observation_run_id = result.deploy_run_id.clone();
                if let (Some(aggregate), Some(target_run_id)) = (
                    aggregate_observation.as_mut(),
                    observation_run_id.as_deref(),
                ) {
                    aggregate.link_target(project_id, target_run_id)?;
                }
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
                        observation_run_id,
                    });
                    if let Some(run) = lifecycle_run.as_mut() {
                        run.update_target(
                            project_id,
                            lifecycle::DeployTargetStatus::Failed,
                            project_results.last().and_then(|entry| entry.error.clone()),
                            Some(timings.clone()),
                        );
                        lifecycle::save_in_roots(roots.data(), run)?;
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
                        observation_run_id,
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
                        observation_run_id,
                    });
                    if let Some(run) = lifecycle_run.as_mut() {
                        run.update_target(
                            project_id,
                            lifecycle::DeployTargetStatus::Succeeded,
                            None,
                            Some(timings.clone()),
                        );
                        lifecycle::save_in_roots(roots.data(), run)?;
                    }
                    succeeded += 1;
                }
            }
            Err(e) => {
                let observation_run_id = e
                    .details
                    .get("deploy_run_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
                if let (Some(aggregate), Some(target_run_id)) = (
                    aggregate_observation.as_mut(),
                    observation_run_id.as_deref(),
                ) {
                    aggregate.link_target(project_id, target_run_id)?;
                }
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
                    observation_run_id,
                });
                if let Some(run) = lifecycle_run.as_mut() {
                    run.update_target(
                        project_id,
                        lifecycle::DeployTargetStatus::Failed,
                        Some(e.to_string()),
                        Some(timings),
                    );
                    lifecycle::save_in_roots(roots.data(), run)?;
                }
                failed += 1;
            }
        }
    }

    let total_projects = project_results.len() as u32;

    if let Some(aggregate) = aggregate_observation.as_mut() {
        aggregate.finish(
            if failed == 0 {
                homeboy_core::observation::RunStatus::Pass
            } else {
                homeboy_core::observation::RunStatus::Fail
            },
            (failed > 0).then_some("deployment reported one or more target failures".to_string()),
        );
    }

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
        resume_run_id: checkpoint_run_id,
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
    use homeboy_core::component::{Component, ScopedExtensionConfig};
    use homeboy_core::project::{Project, ProjectComponentAttachment};
    use homeboy_core::server::{self, Server};
    use homeboy_core::test_support::with_isolated_home;
    use homeboy_extension::{DeployCapability, ExtensionManifest, RemotePathRootRule};
    use std::collections::{BTreeMap, HashMap};
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
            resolved_refs: Default::default(),
            preflighted_source_paths: Default::default(),
            preflighted_component_identities: Default::default(),
            prepared_projection: None,
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
            target: None,
        }
    }

    #[test]
    fn multi_ref_preflight_reports_all_failures_in_component_order_before_materialization() {
        let alpha = Component {
            id: "alpha".to_string(),
            local_path: "/definitely/missing/alpha".to_string(),
            ..Component::default()
        };
        let zebra = Component {
            id: "zebra".to_string(),
            local_path: "/definitely/missing/zebra".to_string(),
            ..Component::default()
        };

        let error = preflight_exact_refs(&[(&zebra, "zebra-ref"), (&alpha, "alpha-ref")])
            .expect_err("every required ref must be checked before preflight returns red");
        let alpha = error
            .message
            .find("component 'alpha' ref 'alpha-ref'")
            .expect("alpha failure");
        let zebra = error
            .message
            .find("component 'zebra' ref 'zebra-ref'")
            .expect("zebra failure");

        assert!(
            alpha < zebra,
            "multi-ref errors must be deterministic regardless of caller ordering: {}",
            error.message
        );
    }

    #[test]
    fn multi_ref_preflight_rejects_duplicate_component_ids_before_any_checkout() {
        let component = Component {
            id: "duplicate".to_string(),
            local_path: "/definitely/missing/duplicate".to_string(),
            ..Component::default()
        };

        let error = preflight_exact_refs(&[(&component, "first"), (&component, "second")])
            .expect_err("ambiguous component refs must fail before materialization");

        assert!(error
            .message
            .contains("component 'duplicate' appears 2 times"));
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
                    ..Default::default()
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
    fn every_mutating_deploy_mode_is_admitted_to_the_shared_lifecycle() {
        let ordinary = deploy_config();
        let mut head = ordinary.clone();
        head.dry_run = false;
        head.head = true;
        let mut exact_ref = head.clone();
        exact_ref.head = false;
        exact_ref.requested_ref = Some("d8abbeb".to_string());
        let mut ordinary_apply = head.clone();
        ordinary_apply.head = false;

        assert!(should_observe_deploy(&head));
        assert!(should_observe_deploy(&exact_ref));
        assert!(should_observe_deploy(&ordinary_apply));
        assert!(!should_observe_deploy(&ordinary));
        let mut check = ordinary;
        check.check = true;
        assert!(!should_observe_deploy(&check));
    }

    #[test]
    fn admitted_run_id_survives_public_error_propagation() {
        let error = attach_admitted_run_id(
            Error::validation_invalid_argument("deploy", "failed", None, None),
            Some("target-observation"),
        );

        assert_eq!(
            error.details["deploy_run_id"],
            serde_json::Value::String("target-observation".to_string())
        );
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

    #[test]
    fn prepared_payload_preflight_detects_managed_path_roots() {
        with_isolated_home(|home| {
            let base_path = home.path().join("runtime");
            std::fs::create_dir_all(&base_path).expect("runtime root");
            let managed_root = base_path.join("wp-content");
            homeboy_extension::save_manifest(&ExtensionManifest {
                id: "managed-paths".to_string(),
                name: "Managed Paths".to_string(),
                version: "1.0.0".to_string(),
                deploy: Some(DeployCapability {
                    verifications: Vec::new(),
                    overrides: Vec::new(),
                    protected_path_suffixes: Vec::new(),
                    owner_hints: Vec::new(),
                    archive_install: Vec::new(),
                    remote_path_inference: Vec::new(),
                    path_roots: vec![RemotePathRootRule {
                        path_prefix: "wp-content".to_string(),
                        root: "wp_content".to_string(),
                        strip_prefix: true,
                        detect_command: Some(format!("printf {}", managed_root.to_string_lossy())),
                    }],
                    version_patterns: Vec::new(),
                    since_tag: None,
                }),
                ..serde_json::from_value(serde_json::json!({
                    "name": "Managed Paths",
                    "version": "1.0.0"
                }))
                .expect("extension manifest")
            })
            .expect("save extension");
            server::save(&Server {
                id: "local".to_string(),
                host: "localhost".to_string(),
                user: "test".to_string(),
                port: 22,
                identity_file: None,
                aliases: vec![],
                kind: None,
                auth: None,
                env: Default::default(),
                runner: None,
            })
            .expect("save local server");
            let project = Project {
                id: "site".to_string(),
                server_id: Some("local".to_string()),
                base_path: Some(base_path.display().to_string()),
                components: vec![ProjectComponentAttachment {
                    id: "plugin".to_string(),
                    local_path: "/stale/plugin".to_string(),
                    remote_path: Some("../wp-content/plugins/plugin".to_string()),
                    ..Default::default()
                }],
                ..Project::default()
            };
            project::save(&project).expect("save project");
            let component = Component {
                id: "plugin".to_string(),
                local_path: "/release/plugin".to_string(),
                remote_path: "../wp-content/plugins/plugin".to_string(),
                extensions: Some(HashMap::from([(
                    "managed-paths".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let config = DeployConfig {
                expected_version: Some("1.2.3".to_string()),
                prepared_projection: Some(PreparedDeployProjection {
                    components: BTreeMap::from([("site:plugin".to_string(), component)]),
                }),
                prepared_artifact: Some(PreparedDeployArtifact {
                    component_id: "plugin".to_string(),
                    path: "/source/plugin.zip".to_string(),
                    durable_path: "/durable/plugin.zip".to_string(),
                    size_bytes: 7,
                    sha256: "hash".to_string(),
                    version: "1.2.3".to_string(),
                    tag: "v1.2.3".to_string(),
                    source_commit: "commit".to_string(),
                }),
                ..deploy_config()
            };

            preflight_prepared_payload_binding(&project, "site", &config)
                .expect("detector-backed managed path binds before lifecycle creation");
        });
    }
}
