use std::collections::HashMap;

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::project::Project;

use super::super::execution::{release_artifact_plan, ReleaseArtifactPlan};
use super::super::orchestration_tag_checkout::{deploy_tag_for_version, TagCheckout};
use super::super::planning::{
    calculate_component_status_with_git_cache, calculate_release_state, ExtensionSkippedComponent,
    GitProbeCache,
};
use super::super::types::{
    ComponentDeployResult, ComponentStatus, DeployArtifactSource, DeployConfig,
    DeployOrchestrationResult, DeploySummary,
};

/// Check mode: return component status without building or deploying.
pub(super) fn run_check_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    extension_skipped: &[ExtensionSkippedComponent],
    project: &Project,
    base_path: &str,
    config: &DeployConfig,
) -> DeployOrchestrationResult {
    let mut git_probe_cache = GitProbeCache::default();
    let mut results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status =
                calculate_component_status_with_git_cache(c, remote_versions, &mut git_probe_cache);
            let release_state = calculate_release_state(c);
            let mut result = ComponentDeployResult::new_for_project(c, project, base_path)
                .with_status("checked")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                )
                .with_component_status(status)
                .with_source_identity(c, config.head);
            if let Some(state) = release_state {
                result = result.with_release_state(state);
            }
            result
        })
        .collect();

    // Append components skipped because a required extension is not installed, so the
    // check-mode diff reports per-component status for the whole project (issue #4587).
    let skipped_results = extension_skipped_results(extension_skipped, project, base_path);
    let skipped = skipped_results.len() as u32;
    results.extend(skipped_results);

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: 0,
            failed: 0,
            skipped,
        },
    }
}

/// Build check-mode result rows for components skipped due to missing extensions.
///
/// Each row is `status: "skipped"` with a warning explaining the missing extension,
/// so operators see `skipped: missing extension <id>` instead of the whole pass aborting.
pub(super) fn extension_skipped_results(
    extension_skipped: &[ExtensionSkippedComponent],
    project: &Project,
    base_path: &str,
) -> Vec<ComponentDeployResult> {
    extension_skipped
        .iter()
        .map(|skip| {
            let component = Component {
                id: skip.id.clone(),
                ..Default::default()
            };
            let mut result = ComponentDeployResult::new_for_project(&component, project, base_path)
                .with_status("skipped");
            result.warnings.push(format!("skipped: {}", skip.reason));
            result
        })
        .collect()
}

/// Dry-run mode: return planned results without building or deploying.
pub(super) fn run_dry_run_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    project: &Project,
    base_path: &str,
    config: &DeployConfig,
) -> Result<DeployOrchestrationResult> {
    let mut git_probe_cache = GitProbeCache::default();
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = if config.check {
                calculate_component_status_with_git_cache(c, remote_versions, &mut git_probe_cache)
            } else {
                ComponentStatus::Unknown
            };
            let mut result = ComponentDeployResult::new_for_project(c, project, base_path)
                .with_status("planned")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                )
                .with_source_identity(c, config.head);
            if let Some(deploy_ref) = planned_deploy_ref(c, config)? {
                result = result.with_deployed_ref(deploy_ref);
            }
            result = with_dry_run_artifact_plan(result, c, config);
            if config.check {
                result = result.with_component_status(status);
            }
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?;

    let total = results.len() as u32;
    Ok(DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        },
    })
}

fn with_dry_run_artifact_plan(
    mut result: ComponentDeployResult,
    component: &Component,
    config: &DeployConfig,
) -> ComponentDeployResult {
    let deploy_config = component.deploy_config();
    let is_git_deploy = deploy_config.is_git_deploy();
    let is_file_deploy = deploy_config.is_file_deploy();
    if is_git_deploy || is_file_deploy {
        return result;
    }

    match release_artifact_plan(component, config, is_git_deploy, is_file_deploy) {
        ReleaseArtifactPlan::Reuse { url, tag } => {
            result.warnings.push(format!(
                "artifact source: release asset for tag {tag}; build phase: skipped if asset is available; deploy phase: would upload downloaded asset"
            ));
            result
                .with_artifact_path(Some(url))
                .with_artifact_source(DeployArtifactSource::ReleaseAsset)
        }
        ReleaseArtifactPlan::LocalBuild { reason } => {
            result.warnings.push(format!(
                "artifact source: local rebuild; reason: {reason}; build phase: would run before deploy; deploy phase: would upload local build_artifact"
            ));
            result.with_artifact_source(DeployArtifactSource::LocalBuild)
        }
    }
}

fn planned_deploy_ref(component: &Component, config: &DeployConfig) -> Result<Option<String>> {
    if component.is_file_component() {
        return Ok(None);
    }

    let path = &component.local_path;
    if config.head {
        return Ok(crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
        )
        .map(|branch| format!("{} (HEAD)", branch)));
    }

    let tag = latest_deploy_tag(component, config.expected_version.as_deref())?;
    let tag_sha =
        crate::core::engine::command::run_in_optional(path, "git", &["rev-parse", "--short", &tag]);
    let head_ahead = crate::core::engine::command::run_in_optional(
        path,
        "git",
        &["rev-list", "--count", &format!("{}..HEAD", tag)],
    )
    .and_then(|out| out.trim().parse::<u32>().ok())
    .unwrap_or(0);

    Ok(Some(
        TagCheckout {
            component_id: component.id.clone(),
            tag,
            original_ref: String::new(),
            local_path: path.clone(),
            tag_sha,
            head_ahead,
        }
        .provenance_ref(),
    ))
}

fn latest_deploy_tag(component: &Component, expected_version: Option<&str>) -> Result<String> {
    if let Some(version) = expected_version {
        return Ok(deploy_tag_for_version(component, version));
    }

    match git::get_latest_tag(&component.local_path) {
        Ok(Some(tag)) => Ok(tag),
        Ok(None) => Err(Error::validation_invalid_argument(
            "deploy",
            format!(
                "Refusing to deploy '{}': no version tags found for default tagged deploy",
                component.id
            ),
            None,
            Some(vec![
                "Run `homeboy release` to create a tagged release first".to_string(),
                "Use `homeboy deploy --head` to deploy the current branch HEAD explicitly"
                    .to_string(),
            ]),
        )),
        Err(err) => Err(Error::git_command_failed(format!(
            "Could not read version tags for '{}': {}",
            component.id, err
        ))),
    }
}
