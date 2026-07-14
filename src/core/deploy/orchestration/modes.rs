use std::collections::HashMap;

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::project::Project;

use super::super::execution::{release_artifact_plan, ReleaseArtifactPlan};
use super::super::orchestration_ref_checkout::resolve_exact_ref;
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
            if let Some(requested_ref) = config.requested_ref.as_deref() {
                let identity = resolve_exact_ref(c, requested_ref)?;
                result = result.with_exact_ref_identity(
                    &identity.requested_ref,
                    &identity.resolved_sha,
                    &identity.source,
                    &identity.resolution_mode,
                );
                result.warnings.push(format!(
                    "source: {}; resolution mode: {}; requested ref: {}; resolved SHA: {}; plan: materialize detached temporary worktree and build exact commit; destination: {}",
                    identity.source,
                    identity.resolution_mode,
                    identity.requested_ref,
                    identity.resolved_sha,
                    result.remote_path.as_deref().unwrap_or("unresolved")
                ));
            }
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
    if let Some(requested_ref) = config.requested_ref.as_deref() {
        resolve_exact_ref(component, requested_ref)?;
        return Ok(Some(requested_ref.to_string()));
    }
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

    match crate::core::release::latest_component_tag(component) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn exact_ref_dry_run_reports_identity_plan_and_destination_without_mutation() {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(repo.path().join("payload.txt"), "reviewed\n").expect("payload");
        git(repo.path(), &["add", "payload.txt"]);
        git(repo.path(), &["commit", "-q", "-m", "reviewed"]);
        git(repo.path(), &["branch", "reviewed"]);
        let sha = git_output(repo.path(), &["rev-parse", "reviewed"]);
        let before_status = git_output(repo.path(), &["status", "--porcelain=v1"]);
        let component = Component {
            id: "fixture".to_string(),
            local_path: repo.path().to_string_lossy().to_string(),
            remote_path: "components/fixture".to_string(),
            build_artifact: Some("build/fixture.zip".to_string()),
            ..Component::default()
        };
        let config = DeployConfig {
            component_ids: vec!["fixture".to_string()],
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
            requested_ref: Some("reviewed".to_string()),
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        };

        let result = run_dry_run_mode(
            std::slice::from_ref(&component),
            &HashMap::new(),
            &HashMap::new(),
            &Project::default(),
            "/srv/site",
            &config,
        )
        .expect("dry-run plan");
        let evidence = &result.results[0];

        assert_eq!(evidence.status, "planned");
        assert_eq!(evidence.requested_ref.as_deref(), Some("reviewed"));
        assert_eq!(evidence.resolved_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(
            evidence.remote_path.as_deref(),
            Some("/srv/site/components/fixture")
        );
        assert!(evidence.warnings.iter().any(|warning| {
            warning.contains("materialize detached temporary worktree")
                && warning.contains("destination: /srv/site/components/fixture")
        }));
        assert_eq!(
            git_output(repo.path(), &["status", "--porcelain=v1"]),
            before_status
        );
        assert_eq!(
            git_output(repo.path(), &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
    }

    #[test]
    fn check_and_dry_run_report_remote_newer_versions_without_safety_refusal() {
        let component = Component {
            id: "fixture".to_string(),
            local_path: "/not/a/checkout".to_string(),
            build_artifact: Some("build/fixture.zip".to_string()),
            ..Component::default()
        };
        let local_versions = HashMap::from([("fixture".to_string(), "1.2.3".to_string())]);
        let remote_versions = HashMap::from([("fixture".to_string(), "1.3.0".to_string())]);
        let config = DeployConfig {
            component_ids: vec!["fixture".to_string()],
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
            no_pull: true,
            allow_stale_source: false,
            allow_downgrade: false,
            head: true,
            requested_ref: None,
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        };

        let checked = run_check_mode(
            std::slice::from_ref(&component),
            &local_versions,
            &remote_versions,
            &[],
            &Project::default(),
            "/srv/site",
            &config,
        );
        assert_eq!(checked.results[0].status, "checked");
        assert_eq!(checked.results[0].local_version.as_deref(), Some("1.2.3"));
        assert_eq!(checked.results[0].remote_version.as_deref(), Some("1.3.0"));

        let planned = run_dry_run_mode(
            &[component],
            &local_versions,
            &remote_versions,
            &Project::default(),
            "/srv/site",
            &config,
        )
        .expect("dry-run must report rather than refuse a remote-newer version");
        assert_eq!(planned.results[0].status, "planned");
        assert_eq!(planned.results[0].local_version.as_deref(), Some("1.2.3"));
        assert_eq!(planned.results[0].remote_version.as_deref(), Some("1.3.0"));
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }
}
