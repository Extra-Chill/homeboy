use std::collections::HashMap;

use homeboy_core::component::{resolve_component_scope, Component, ScopeCommand};
use homeboy_core::error::{Error, Result};
use homeboy_core::git::release_download::ReleaseArtifactLease;
use homeboy_core::project::Project;
use homeboy_core::server::SshClient;

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
    client: &SshClient,
    canonical_packages: &HashMap<String, ReleaseArtifactLease>,
    unavailable_canonical_packages: &HashMap<String, String>,
) -> DeployOrchestrationResult {
    let mut git_probe_cache = GitProbeCache::default();
    let mut results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let mut status =
                calculate_component_status_with_git_cache(c, remote_versions, &mut git_probe_cache);
            let release_state = calculate_release_state(c);
            let prepared = config
                .prepared_artifact
                .as_ref()
                .filter(|artifact| artifact.component_id == c.id);
            let target = super::super::path_roots::resolve_effective_remote_path(project, c, base_path)
                .unwrap_or_default();
            let manifest = if let Some(artifact) = prepared {
                super::super::content_manifest::compare_prepared_archive(
                    std::path::Path::new(artifact.effective_path()),
                    &target,
                    client,
                    &resolve_component_scope(c, ScopeCommand::Deploy).exclude,
                    artifact,
                )
            } else if let Some(package) = canonical_packages.get(&c.id) {
                match super::super::content_manifest::verify_archive_hash(&package.path, package) {
                    Ok(()) => super::super::content_manifest::compare_archive(
                        &package.path,
                        &target,
                        client,
                        &resolve_component_scope(c, ScopeCommand::Deploy).exclude,
                        Some(package),
                    ),
                    Err(error) => {
                        super::super::content_manifest::canonical_package_unavailable_for_artifact(
                            error, package,
                        )
                    }
                }
            } else if let Some(diagnostic) = unavailable_canonical_packages.get(&c.id) {
                super::super::content_manifest::canonical_package_unavailable(
                    diagnostic.clone(),
                    c.build_artifact.as_deref(),
                )
            } else {
                let version = local_versions.get(&c.id).map(String::as_str).unwrap_or_default();
                match super::super::receipt::load(
                    project,
                    &c.id,
                    &target,
                    version,
                    &resolve_component_scope(c, ScopeCommand::Deploy).exclude,
                ) {
                    Ok(Some(receipt)) => super::super::content_manifest::compare_saved_package_manifest(
                        &receipt.manifest,
                        &target,
                        client,
                        &receipt.exclusions,
                        receipt.provenance(),
                    ),
                    Ok(None) => super::super::content_manifest::local_build_package_unavailable(
                        "canonical package is unavailable in check mode and no deployed-package receipt matches this target and version".to_string(),
                        c.build_artifact.as_deref(),
                    ),
                    Err(error) => super::super::content_manifest::local_build_package_unavailable(
                        format!("deployed-package receipt unavailable: {error}"),
                        c.build_artifact.as_deref(),
                    ),
                }
            };
            if manifest.status == "missing" {
                status = ComponentStatus::Missing;
            } else if manifest.status == "different" && matches!(status, ComponentStatus::UpToDate)
            {
                status = ComponentStatus::RemoteModified;
            } else if manifest.status == "different" && !matches!(status, ComponentStatus::UpToDate)
            {
                status = ComponentStatus::MixedDrift;
            }
            let mut result = ComponentDeployResult::new_for_project(c, project, base_path)
                .with_status("checked")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                )
                .with_component_status(status)
                .with_content_manifest(manifest)
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
        deploy_run_id: None,
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
            if let Some(requested_ref) = config.requested_ref_for(&c.id) {
                let identity = if let Some(resolved_sha) = config.resolved_ref_for(&c.id) {
                    super::super::orchestration_ref_checkout::ExactRefIdentity {
                        requested_ref: requested_ref.to_string(),
                        resolved_sha: resolved_sha.to_string(),
                        source: c.local_path.clone(),
                        resolution_mode: "release-set-preflight".to_string(),
                    }
                } else {
                    resolve_exact_ref(c, requested_ref)?
                };
                if let Some(artifact) = config.prepared_artifact.as_ref() {
                    artifact.validate_exact_source(
                        &c.id,
                        config.expected_version.as_deref(),
                        &identity.resolved_sha,
                    )?;
                }
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
        deploy_run_id: None,
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

    if let Some(artifact) = config.prepared_artifact.as_ref() {
        result.warnings.push(format!(
            "artifact source: verified prepared artifact from commit {}; build phase: skipped; deploy phase: would upload prepared artifact",
            artifact.source_commit
        ));
        return result
            .with_artifact_path(Some(artifact.effective_path().to_string()))
            .with_artifact_source(DeployArtifactSource::Prepared);
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
    if let Some(requested_ref) = config.requested_ref_for(&component.id) {
        resolve_exact_ref(component, requested_ref)?;
        return Ok(Some(requested_ref.to_string()));
    }
    if config.head {
        return Ok(homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
        )
        .map(|branch| format!("{} (HEAD)", branch)));
    }

    let tag = latest_deploy_tag(component, config.expected_version.as_deref())?;
    let tag_sha = homeboy_core::engine::command::run_in_optional(
        path,
        "git",
        &["rev-parse", "--short", &tag],
    );
    let head_ahead = homeboy_core::engine::command::run_in_optional(
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

    match crate::release::latest_component_tag(component) {
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
    use crate::deploy::PreparedDeployArtifact;
    use std::collections::BTreeMap;
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
            requested_refs: Default::default(),
            resolved_refs: Default::default(),
            preflighted_source_paths: Default::default(),
            preflighted_component_identities: Default::default(),
            prepared_projection: None,
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
    fn multi_ref_dry_run_resolves_each_component_ref_without_mutating_either_checkout() {
        let first_repo = tempfile::tempdir().expect("first repo");
        let second_repo = tempfile::tempdir().expect("second repo");
        for (repo, branch) in [(&first_repo, "first-ref"), (&second_repo, "second-ref")] {
            git(repo.path(), &["init", "-q"]);
            git(repo.path(), &["config", "user.name", "Homeboy Test"]);
            git(
                repo.path(),
                &["config", "user.email", "homeboy@example.test"],
            );
            std::fs::write(repo.path().join("payload.txt"), branch).expect("payload");
            git(repo.path(), &["add", "payload.txt"]);
            git(repo.path(), &["commit", "-q", "-m", branch]);
            git(repo.path(), &["branch", branch]);
        }
        let components = vec![
            Component {
                id: "first".to_string(),
                local_path: first_repo.path().to_string_lossy().to_string(),
                remote_path: "components/first".to_string(),
                build_artifact: Some("build/first.zip".to_string()),
                ..Component::default()
            },
            Component {
                id: "second".to_string(),
                local_path: second_repo.path().to_string_lossy().to_string(),
                remote_path: "components/second".to_string(),
                build_artifact: Some("build/second.zip".to_string()),
                ..Component::default()
            },
        ];
        let config = DeployConfig {
            component_ids: vec!["first".to_string(), "second".to_string()],
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
            requested_refs: BTreeMap::from([
                ("first".to_string(), "first-ref".to_string()),
                ("second".to_string(), "second-ref".to_string()),
            ]),
            resolved_refs: Default::default(),
            preflighted_source_paths: Default::default(),
            preflighted_component_identities: Default::default(),
            prepared_projection: None,
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        };

        let result = run_dry_run_mode(
            &components,
            &HashMap::new(),
            &HashMap::new(),
            &Project::default(),
            "/srv/site",
            &config,
        )
        .expect("multi-ref dry-run plan");

        assert_eq!(
            result.results[0].requested_ref.as_deref(),
            Some("first-ref")
        );
        assert_eq!(
            result.results[1].requested_ref.as_deref(),
            Some("second-ref")
        );

        let mut invalid_config = config.clone();
        invalid_config
            .requested_refs
            .insert("second".to_string(), "missing-ref".to_string());
        let error = run_dry_run_mode(
            &components,
            &HashMap::new(),
            &HashMap::new(),
            &Project::default(),
            "/srv/site",
            &invalid_config,
        )
        .expect_err("an unresolved member ref must fail before any mutation");
        assert!(error.message.contains("missing-ref"));

        for repo in [&first_repo, &second_repo] {
            assert_eq!(git_output(repo.path(), &["status", "--porcelain=v1"]), "");
            assert_eq!(
                git_output(repo.path(), &["worktree", "list", "--porcelain"])
                    .matches("worktree ")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn exact_ref_dry_run_reports_verified_prepared_artifact_strategy() {
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
        let sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        let artifact_path = repo.path().join("fixture.zip");
        std::fs::write(&artifact_path, "prepared bytes").expect("artifact");
        let component = Component {
            id: "fixture".to_string(),
            local_path: repo.path().to_string_lossy().to_string(),
            remote_path: "components/fixture".to_string(),
            build_artifact: Some("build/fixture.zip".to_string()),
            ..Component::default()
        };
        let mut config = DeployConfig {
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
            requested_ref: Some(sha.clone()),
            requested_refs: Default::default(),
            resolved_refs: Default::default(),
            preflighted_source_paths: Default::default(),
            preflighted_component_identities: Default::default(),
            prepared_projection: None,
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        };
        config.prepared_artifact = Some(PreparedDeployArtifact {
            component_id: "fixture".to_string(),
            path: artifact_path.display().to_string(),
            durable_path: artifact_path.display().to_string(),
            size_bytes: "prepared bytes".len() as u64,
            sha256: crate::deploy::sha256_file(&artifact_path).expect("sha"),
            version: String::new(),
            tag: "prepared".to_string(),
            source_commit: sha,
        });

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

        assert_eq!(
            evidence.artifact_source,
            Some(DeployArtifactSource::Prepared)
        );
        assert!(evidence
            .warnings
            .iter()
            .any(|warning| warning.contains("verified prepared artifact")));
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
            requested_refs: Default::default(),
            resolved_refs: Default::default(),
            preflighted_source_paths: Default::default(),
            preflighted_component_identities: Default::default(),
            prepared_projection: None,
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
            &local_client(),
            &HashMap::new(),
            &HashMap::new(),
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

    #[test]
    fn check_uses_canonical_package_for_clean_and_modified_production_trees() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        let remote = temp.path().join("plugin");
        std::fs::create_dir_all(source.join("tests")).expect("source tests");
        std::fs::create_dir_all(&remote).expect("remote");
        std::fs::write(source.join("README.md"), "source only").expect("readme");
        std::fs::write(source.join("tests/test.php"), "source only").expect("test");
        std::fs::write(source.join("plugin.php"), "Version: 1.0.0\nrelease")
            .expect("source plugin");
        std::fs::write(remote.join("plugin.php"), "Version: 1.0.0\nrelease")
            .expect("remote plugin");

        let package = temp.path().join("plugin.zip");
        let file = std::fs::File::create(&package).expect("package");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("plugin/plugin.php", zip::write::FileOptions::default())
            .expect("entry");
        use std::io::Write as _;
        zip.write_all(b"Version: 1.0.0\nrelease").expect("contents");
        zip.finish().expect("finish");
        let bytes = std::fs::read(&package).expect("package bytes");
        let package =
            ReleaseArtifactLease::test_new(homeboy_core::git::release_download::ReleaseArtifact {
                path: package,
                tag: "v1.0.0".to_string(),
                commit: Some("release".to_string()),
                url: "https://example.test/plugin.zip".to_string(),
                name: "plugin.zip".to_string(),
                size: bytes.len() as u64,
                sha256: crate::deploy::sha256_file(temp.path().join("plugin.zip").as_path())
                    .expect("sha"),
            })
            .expect("lease");
        let component = Component {
            id: "plugin".to_string(),
            local_path: source.to_string_lossy().to_string(),
            remote_path: remote.to_string_lossy().to_string(),
            version_targets: Some(vec![homeboy_core::component::VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                artifact_path: None,
            }]),
            ..Component::default()
        };
        let versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        let config = DeployConfig::check_all_no_pull_head();
        let packages = HashMap::from([("plugin".to_string(), package)]);

        let clean = run_check_mode(
            std::slice::from_ref(&component),
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &config,
            &local_client(),
            &packages,
            &HashMap::new(),
        );
        assert_eq!(
            clean.results[0].component_status,
            Some(ComponentStatus::UpToDate)
        );
        let manifest = clean.results[0]
            .content_manifest
            .as_ref()
            .expect("package manifest");
        assert_eq!(manifest.scope, "canonical-package-installed-tree");
        assert_eq!(
            manifest
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.artifact_tag.as_deref()),
            Some("v1.0.0")
        );

        std::fs::write(remote.join("plugin.php"), "modified").expect("drift");
        let drift = run_check_mode(
            std::slice::from_ref(&component),
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &config,
            &local_client(),
            &packages,
            &HashMap::new(),
        );
        assert_eq!(
            drift.results[0].component_status,
            Some(ComponentStatus::RemoteModified)
        );

        std::fs::write(&packages["plugin"].path, "mutated after lease").expect("mutate package");
        let mutated = run_check_mode(
            std::slice::from_ref(&component),
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &config,
            &local_client(),
            &packages,
            &HashMap::new(),
        );
        let manifest = mutated.results[0]
            .content_manifest
            .as_ref()
            .expect("unavailable canonical package evidence");
        assert_eq!(manifest.status, "unavailable");
        assert_eq!(manifest.scope, "canonical-package-unavailable");
        assert_eq!(
            manifest
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.artifact_sha256.as_deref()),
            None
        );
    }

    #[test]
    fn prepared_artifact_is_selected_ahead_of_a_release_asset_during_check() {
        let temp = tempfile::tempdir().expect("temp");
        let remote = temp.path().join("plugin");
        std::fs::create_dir_all(&remote).expect("remote");
        std::fs::write(remote.join("plugin.php"), "prepared").expect("remote payload");
        let prepared_path = temp.path().join("prepared.zip");
        let release_path = temp.path().join("release.zip");
        for (path, contents) in [
            (&prepared_path, b"prepared".as_slice()),
            (&release_path, b"release".as_slice()),
        ] {
            let file = std::fs::File::create(path).expect("package");
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("plugin.php", zip::write::FileOptions::default())
                .expect("entry");
            use std::io::Write as _;
            zip.write_all(contents).expect("contents");
            zip.finish().expect("finish");
        }
        let release =
            ReleaseArtifactLease::test_new(homeboy_core::git::release_download::ReleaseArtifact {
                path: release_path.clone(),
                tag: "v1.0.0".to_string(),
                commit: None,
                url: "https://example.test/release.zip".to_string(),
                name: "release.zip".to_string(),
                size: std::fs::metadata(&release_path).expect("metadata").len(),
                sha256: crate::deploy::sha256_file(&release_path).expect("sha"),
            })
            .expect("lease");
        let component = Component {
            id: "plugin".to_string(),
            remote_path: remote.display().to_string(),
            ..Component::default()
        };
        let versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        let mut config = DeployConfig::check_all_no_pull_head();
        config.prepared_artifact = Some(PreparedDeployArtifact {
            component_id: "plugin".to_string(),
            path: prepared_path.display().to_string(),
            durable_path: prepared_path.display().to_string(),
            size_bytes: std::fs::metadata(&prepared_path).expect("metadata").len(),
            sha256: crate::deploy::sha256_file(&prepared_path).expect("sha"),
            version: "1.0.0".to_string(),
            tag: "v1.0.0".to_string(),
            source_commit: "prepared-commit".to_string(),
        });
        let result = run_check_mode(
            &[component],
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &config,
            &local_client(),
            &HashMap::from([("plugin".to_string(), release)]),
            &HashMap::new(),
        );
        let manifest = result.results[0]
            .content_manifest
            .as_ref()
            .expect("manifest");
        assert_eq!(manifest.status, "match");
        assert_eq!(
            manifest.provenance.as_ref().expect("provenance").source,
            "prepared-artifact"
        );
    }

    #[test]
    fn check_reports_mixed_version_and_content_drift_against_the_package() {
        let temp = tempfile::tempdir().expect("temp");
        let remote = temp.path().join("plugin");
        std::fs::create_dir_all(&remote).expect("remote");
        std::fs::write(remote.join("plugin.php"), "Version: 2.0.0\nmodified")
            .expect("remote plugin");
        let package = temp.path().join("plugin.zip");
        let file = std::fs::File::create(&package).expect("package");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("plugin/plugin.php", zip::write::FileOptions::default())
            .expect("entry");
        use std::io::Write as _;
        zip.write_all(b"Version: 1.0.0\nrelease").expect("contents");
        zip.finish().expect("finish");
        let bytes = std::fs::read(&package).expect("package bytes");
        let lease =
            ReleaseArtifactLease::test_new(homeboy_core::git::release_download::ReleaseArtifact {
                path: package.clone(),
                tag: "v1.0.0".to_string(),
                commit: Some("release-commit".to_string()),
                url: "https://example.test/plugin.zip".to_string(),
                name: "plugin.zip".to_string(),
                size: bytes.len() as u64,
                sha256: crate::deploy::sha256_file(&package).expect("sha"),
            })
            .expect("lease");
        let component = Component {
            id: "plugin".to_string(),
            local_path: temp.path().display().to_string(),
            remote_path: remote.display().to_string(),
            ..Component::default()
        };
        let local_versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        let remote_versions = HashMap::from([("plugin".to_string(), "2.0.0".to_string())]);
        let result = run_check_mode(
            &[component],
            &local_versions,
            &remote_versions,
            &[],
            &Project::default(),
            ".",
            &DeployConfig::check_all_no_pull_head(),
            &local_client(),
            &HashMap::from([("plugin".to_string(), lease)]),
            &HashMap::new(),
        );
        assert_eq!(
            result.results[0].component_status,
            Some(ComponentStatus::MixedDrift)
        );
        assert_eq!(
            result.results[0]
                .content_manifest
                .as_ref()
                .expect("manifest")
                .status,
            "different"
        );
    }

    #[test]
    fn check_reports_unavailable_canonical_package_without_reading_the_source_tree() {
        let component = Component {
            id: "plugin".to_string(),
            local_path: "/source/tree/must-not-be-read".to_string(),
            build_artifact: Some("dist/plugin.zip".to_string()),
            ..Component::default()
        };
        let versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        let result = run_check_mode(
            &[component],
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &DeployConfig::check_all_no_pull_head(),
            &local_client(),
            &HashMap::new(),
            &HashMap::from([(
                "plugin".to_string(),
                "release asset download failed".to_string(),
            )]),
        );
        let manifest = result.results[0]
            .content_manifest
            .as_ref()
            .expect("canonical unavailable evidence");
        assert_eq!(manifest.status, "unavailable");
        assert_eq!(manifest.scope, "canonical-package-unavailable");
        assert_eq!(
            manifest
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.artifact_name.as_deref()),
            Some("dist/plugin.zip")
        );
    }

    #[test]
    fn tagged_and_skip_build_checks_require_a_canonical_package_or_receipt() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("monorepo");
        let remote = temp.path().join("installed");
        std::fs::create_dir_all(source.join("packages/plugin")).expect("source");
        std::fs::create_dir_all(&remote).expect("remote");
        std::fs::write(source.join("README.md"), "source only").expect("source file");
        std::fs::write(source.join("packages/plugin/plugin.php"), "installed")
            .expect("source file");
        std::fs::write(remote.join("plugin.php"), "installed").expect("remote file");
        let component = Component {
            id: "plugin".to_string(),
            local_path: source.display().to_string(),
            remote_path: remote.display().to_string(),
            build_artifact: Some("packages/plugin/plugin.zip".to_string()),
            ..Component::default()
        };
        let versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        for mut config in [
            DeployConfig {
                tagged: true,
                ..DeployConfig::check_all_no_pull_head()
            },
            DeployConfig {
                skip_build: true,
                ..DeployConfig::check_all_no_pull_head()
            },
        ] {
            config.check = true;
            let result = run_check_mode(
                std::slice::from_ref(&component),
                &versions,
                &versions,
                &[],
                &Project::default(),
                ".",
                &config,
                &local_client(),
                &HashMap::new(),
                &HashMap::new(),
            );
            let manifest = result.results[0]
                .content_manifest
                .as_ref()
                .expect("canonical package evidence");
            assert_eq!(manifest.status, "unavailable");
            assert_eq!(manifest.scope, "canonical-package-unavailable");
            assert_eq!(
                manifest
                    .provenance
                    .as_ref()
                    .map(|provenance| provenance.source.as_str()),
                Some("local-build")
            );
        }
    }

    #[test]
    fn check_reports_unavailable_when_local_build_artifact_has_no_package_coverage() {
        let component = Component {
            id: "plugin".to_string(),
            local_path: "/source/tree/must-not-be-read".to_string(),
            build_artifact: Some("dist/plugin.zip".to_string()),
            ..Component::default()
        };
        let versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        let result = run_check_mode(
            &[component],
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &DeployConfig::check_all_no_pull_head(),
            &local_client(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(
            result.results[0]
                .content_manifest
                .as_ref()
                .expect("manifest")
                .status,
            "unavailable"
        );
    }

    #[test]
    fn check_does_not_use_package_coverage_without_an_immutable_payload() {
        let component = Component {
            id: "plugin".to_string(),
            local_path: "/source/tree/must-not-be-read".to_string(),
            build_artifact: Some("dist/plugin.zip".to_string()),
            ..Component::default()
        };
        let versions = HashMap::from([("plugin".to_string(), "1.0.0".to_string())]);
        let result = run_check_mode(
            &[component],
            &versions,
            &versions,
            &[],
            &Project::default(),
            ".",
            &DeployConfig::check_all_no_pull_head(),
            &local_client(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let manifest = result.results[0]
            .content_manifest
            .as_ref()
            .expect("manifest");
        assert_eq!(manifest.status, "unavailable");
        let diagnostic = manifest.diagnostic.as_deref().expect("diagnostic");
        assert!(
            diagnostic.contains("no deployed-package receipt"),
            "{diagnostic}"
        );
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

    fn local_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "test".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: Default::default(),
        }
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
