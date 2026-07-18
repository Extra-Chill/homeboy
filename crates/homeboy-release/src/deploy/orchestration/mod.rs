use std::collections::HashMap;

use crate::release::version;
use homeboy_core::component::Component;
use homeboy_core::context::RemoteProjectContext;
use homeboy_core::error::{Error, Result};
use homeboy_core::project::Project;

use super::execution::{
    execute_preflighted_component_deploy, release_artifact_plan, resolve_planned_release_artifact,
    ReleaseArtifactPlan,
};
use super::orchestration_ref_checkout::{ExactRefCheckout, ExactRefIdentity};
use super::orchestration_tag_checkout::{checkout_deploy_tags, restore_branches};
use super::path_roots::{project_with_detected_path_roots, resolve_effective_remote_path};
use super::planning::{load_project_components, plan_components};
use super::types::{ComponentDeployResult, DeployConfig, DeployOrchestrationResult, DeploySummary};
use super::version_overrides::fetch_remote_versions_for_project;
use homeboy_core::git::release_download::{ReleaseArtifactLease, ReleaseArtifactStore};

mod modes;
mod preflight;
mod prepared_payloads;
mod smoke_check;

use modes::{extension_skipped_results, run_check_mode, run_dry_run_mode};
use preflight::{
    check_uncommitted_changes, check_unreleased_commits, guard_local_build_downgrades,
    guard_local_build_source_freshness, local_build_components, sync_components,
    verify_expected_version, warn_non_default_branch,
};
use prepared_payloads::prepare_component_deployments;
use smoke_check::run_post_deploy_smoke;

/// Main deploy orchestration entry point.
/// Handles component selection, building, and deployment.
pub(super) fn deploy_components(
    config: &DeployConfig,
    project: &Project,
    ctx: &RemoteProjectContext,
    base_path: &str,
    release_artifacts: &mut ReleaseArtifactStore,
) -> Result<DeployOrchestrationResult> {
    let loaded = load_project_components(project, &config.component_ids, config.check)?;
    validate_supported_build_configs(&loaded.deployable)?;

    // In check mode, components whose required extensions are missing are skipped
    // (not hard-failed) so the read-only diff still reports everything else.
    // If nothing is deployable but components were extension-skipped, surface those
    // as skipped check results instead of erroring with "no deployable components".
    if config.check && loaded.deployable.is_empty() && !loaded.extension_skipped.is_empty() {
        let results = extension_skipped_results(&loaded.extension_skipped, project, base_path);
        let skipped = results.len() as u32;
        return Ok(DeployOrchestrationResult {
            results,
            summary: DeploySummary {
                total: skipped,
                succeeded: 0,
                failed: 0,
                skipped,
            },
        });
    }

    if loaded.deployable.is_empty() {
        let message = if loaded.skipped.is_empty() {
            "No components configured for project".to_string()
        } else {
            format!(
                "No deployable components found — {} component(s) skipped (no build artifact or deploy strategy): {}",
                loaded.skipped.len(),
                loaded.skipped.join(", ")
            )
        };
        return Err(Error::validation_invalid_argument(
            "componentIds",
            message,
            None,
            Some(vec![
                "A component is deployable when it has a buildArtifact, an extension that resolves an artifact_pattern, or deploy_strategy: \"git\".".to_string(),
                "If the component builds via an extension, declare that extension in its homeboy.json (e.g. \"extensions\": { \"<ext>\": {} }) so the artifact can be resolved.".to_string(),
                "Sync the component's homeboy.json config into the project: homeboy project components attach-path <project> <local_path>.".to_string(),
                "Inspect the effective config with: homeboy component show <id>".to_string(),
            ]),
        ));
    }

    let project =
        project_with_detected_path_roots(project, &loaded.deployable, base_path, &ctx.client);

    let components = plan_components(
        config,
        &loaded.deployable,
        &loaded.skipped,
        &project,
        base_path,
        &ctx.client,
    )?;

    if components.is_empty() {
        return Ok(DeployOrchestrationResult {
            results: vec![],
            summary: DeploySummary {
                total: 0,
                succeeded: 0,
                failed: 0,
                skipped: 0,
            },
        });
    }

    validate_effective_remote_paths(&components, &project, base_path)?;

    // Release assets are immutable remote inputs. Resolve and verify them before
    // touching any configured checkout, then reuse the same run-scoped bytes for
    // every target/project that requests this component.
    let mut resolved_release_artifacts: HashMap<String, ReleaseArtifactLease> = HashMap::new();
    for component in &components {
        if let ReleaseArtifactPlan::Reuse { tag, .. } =
            release_artifact_plan(component, config, false, false)
        {
            let artifact = resolve_planned_release_artifact(component, &tag, release_artifacts)
                .map_err(|error| {
                    Error::validation_invalid_argument("releaseArtifact", error, None, None)
                })?;
            homeboy_core::log_status!(
                "deploy",
                "Verified release asset: tag={} name={} size={} sha256={} source={}",
                artifact.tag,
                artifact.name,
                artifact.size,
                artifact.sha256,
                artifact.url
            );
            resolved_release_artifacts.insert(component.id.clone(), artifact);
        }
    }

    // Resolve first, then materialize immutable detached worktrees for real deploys.
    // Dry-run resolves in `run_dry_run_mode` and never creates a worktree.
    let exact_ref_checkouts = if !config.dry_run {
        if let Some(requested_ref) = config.requested_ref.as_deref() {
            components
                .iter()
                .map(|component| ExactRefCheckout::materialize(component, requested_ref))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let exact_ref_identities: HashMap<String, ExactRefIdentity> = exact_ref_checkouts
        .iter()
        .map(|checkout| (checkout.component.id.clone(), checkout.identity.clone()))
        .collect();
    for checkout in &exact_ref_checkouts {
        checkout.verify()?;
        checkout.hydrate_dependencies(config.skip_deps_hydration)?;
    }
    if let Some(artifact) = config.prepared_artifact.as_ref() {
        for checkout in &exact_ref_checkouts {
            artifact.validate_exact_source(
                &checkout.component.id,
                config.expected_version.as_deref(),
                &checkout.identity.resolved_sha,
            )?;
        }
    }
    let components = if exact_ref_checkouts.is_empty() {
        components
    } else {
        exact_ref_checkouts
            .iter()
            .map(|checkout| checkout.component.clone())
            .collect()
    };

    // Gather versions
    let mut local_versions: HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    let local_build_components = local_build_components(&components, config);
    let remote_versions = if config.outdated
        || config.behind_upstream
        || config.dry_run
        || config.check
        || !local_build_components.is_empty()
    {
        fetch_remote_versions_for_project(&components, Some(&project), base_path, &ctx.client)
            .versions
    } else {
        HashMap::new()
    };

    // Check and dry-run modes return early without building or deploying
    if config.check {
        return Ok(run_check_mode(
            &components,
            &local_versions,
            &remote_versions,
            &loaded.extension_skipped,
            &project,
            base_path,
            config,
        ));
    }
    if config.dry_run {
        return Ok(run_dry_run_mode(
            &components,
            &local_versions,
            &remote_versions,
            &project,
            base_path,
            config,
        )?);
    }

    // Only local builds require mutable checkout safety checks. Release assets are
    // resolved and verified above and must not read or alter a source checkout.
    let local_build_components: Vec<Component> = components
        .iter()
        .filter(|component| !resolved_release_artifacts.contains_key(&component.id))
        .cloned()
        .collect();

    // Sync: pull latest changes before deploying (unless --no-pull or --skip-build)
    if config.requested_ref.is_none() && !config.no_pull && !config.skip_build {
        sync_components(&local_build_components)?;
    }

    guard_local_build_source_freshness(&local_build_components, config)?;

    // Warn when --head deploys from a non-default branch (safety guardrail)
    if config.head && !config.skip_build {
        warn_non_default_branch(&local_build_components, config)?;
    }

    if config.requested_ref.is_none() && !config.force {
        check_uncommitted_changes(&local_build_components)?;
    }

    // Check for HEAD-vs-tag gap before the tag checkout.
    if config.requested_ref.is_none() && !config.head && !config.skip_build {
        check_unreleased_commits(&local_build_components, config)?;
    }

    // Checkout the deploy tag for each component (unless --head or --skip-build).
    let tag_checkouts = if config.requested_ref.is_none() && !config.head && !config.skip_build {
        checkout_deploy_tags(&local_build_components, config.expected_version.as_deref())?
    } else {
        Vec::new()
    };

    // Verify expected version if --version was specified
    if let Some(ref expected) = config.expected_version {
        if let Err(err) = verify_expected_version(&local_build_components, expected) {
            if !tag_checkouts.is_empty() {
                restore_branches(&tag_checkouts);
            }
            return Err(err);
        }
    }

    local_versions = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();

    guard_local_build_downgrades(
        &local_build_components,
        &local_versions,
        &remote_versions,
        config,
    )?;

    // Build and validate every local artifact before the first remote write.
    let prepared_deployments = match prepare_component_deployments(
        &components,
        config,
        &project,
        base_path,
        &local_versions,
        &remote_versions,
        &resolved_release_artifacts,
    ) {
        Ok(prepared) => prepared,
        Err(failures) => {
            let failed = failures.len() as u32;
            if !tag_checkouts.is_empty() {
                restore_branches(&tag_checkouts);
            }
            return Ok(DeployOrchestrationResult {
                results: failures,
                summary: DeploySummary {
                    total: failed,
                    succeeded: 0,
                    failed,
                    skipped: 0,
                },
            });
        }
    };

    // Execute deployments only after every component passed the local preflight.
    let mut results: Vec<ComponentDeployResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for prepared in prepared_deployments.iter() {
        let component = &prepared.component;

        let mut result = execute_preflighted_component_deploy(prepared, ctx, base_path, &project);

        // Record which git ref was deployed. The same label feeds build provenance
        // so `deployed_ref` and `build_provenance.built_from_ref` never disagree.
        let exact_ref_identity = exact_ref_identities.get(&component.id);
        let deployed_ref = if let Some(identity) = exact_ref_identity {
            Some(identity.requested_ref.clone())
        } else if let Some(artifact) = resolved_release_artifacts.get(&component.id) {
            Some(match artifact.commit.as_deref() {
                Some(commit) => format!("{} ({commit})", artifact.tag),
                None => artifact.tag.clone(),
            })
        } else if let Some(checkout) = tag_checkouts
            .iter()
            .find(|c| c.component_id == component.id)
        {
            Some(checkout.provenance_ref())
        } else if config.head {
            // Deploying from HEAD — record the current branch
            homeboy_core::engine::command::run_in_optional(
                &component.local_path,
                "git",
                &["rev-parse", "--abbrev-ref", "HEAD"],
            )
            .map(|branch| format!("{} (HEAD)", branch))
        } else if let Some(prepared_artifact) = config.prepared_artifact.as_ref() {
            Some(prepared_artifact.tag.clone())
        } else {
            None
        };

        if let Some(ref git_ref) = deployed_ref {
            result = result.with_deployed_ref(git_ref.clone());
        }
        if let Some(identity) = exact_ref_identity {
            result = result.with_exact_ref_identity(
                &identity.requested_ref,
                &identity.resolved_sha,
                &identity.source,
                &identity.resolution_mode,
            );
        }
        if let Some(prepared_artifact) = config.prepared_artifact.clone() {
            result = result.with_prepared_artifact(prepared_artifact);
        }

        // Attach explicit build provenance to every result, regardless of strategy.
        let mut build_provenance = prepared.build_provenance.clone();
        build_provenance.built_from_ref = deployed_ref;
        if let Some(identity) = exact_ref_identity {
            build_provenance.built_from_commit = Some(identity.resolved_sha.clone());
        } else if let Some(artifact) = resolved_release_artifacts.get(&component.id) {
            build_provenance.built_from_commit = artifact.commit.clone();
        } else if let Some(prepared_artifact) = config.prepared_artifact.as_ref() {
            build_provenance.built_from_commit = Some(prepared_artifact.source_commit.clone());
        }
        result = result.with_build_provenance(build_provenance);

        if result.status == "deployed" {
            succeeded += 1;
        } else {
            failed += 1;
        }
        results.push(result);
    }

    // Restore original branches after deployment
    if !tag_checkouts.is_empty() {
        restore_branches(&tag_checkouts);
    }

    // Post-deploy front-end smoke check (opt-in, project-scoped). Runs only when
    // something actually deployed — a runtime-fataling release that returns 500
    // to fresh visitors should fail the deploy here so it gets rolled back
    // instead of sitting live. Catches runtime errors that a syntax-only
    // preflight structurally cannot. See homeboy#5471.
    if succeeded > 0 {
        if let Some(smoke) = run_post_deploy_smoke(&project, &mut results) {
            if smoke {
                // Smoke failed and was not warn-only: flip every just-deployed
                // component to failed so the overall deploy exit code is non-zero
                // and the operator/automation treats it as a rollback candidate.
                for result in results.iter_mut() {
                    if result.status == "deployed" {
                        result.status = "failed".to_string();
                    }
                }
                failed += succeeded;
                succeeded = 0;
            }
        }
    }

    Ok(DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total: succeeded + failed,
            succeeded,
            failed,
            skipped: 0,
        },
    })
}

fn validate_supported_build_configs(components: &[Component]) -> Result<()> {
    for component in components {
        component.validate_supported_build_config()?;
    }

    Ok(())
}

fn validate_effective_remote_paths(
    components: &[Component],
    project: &Project,
    base_path: &str,
) -> Result<()> {
    for component in components {
        resolve_effective_remote_path(project, component, base_path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::orchestration_tag_checkout::{
        checkout_deploy_tags, deploy_tag_for_version, restore_branches, TagCheckout,
    };
    use crate::deploy::planning::{load_project_components, ExtensionSkippedComponent};
    use homeboy_core::component::ComponentScriptsConfig;
    use homeboy_core::project::ProjectComponentAttachment;
    use homeboy_core::test_support::{home_env_guard, with_isolated_home};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    use super::modes::{extension_skipped_results, run_dry_run_mode};
    use super::preflight::{
        auto_pull_version_drift_message, check_uncommitted_changes, check_unreleased_commits,
        verify_expected_version,
    };

    fn make_component(id: &str, local_path: &str) -> Component {
        Component::new(id.to_string(), local_path.to_string(), String::new(), None)
    }

    fn artifact_component(id: &str, local_path: &str, artifact: &str) -> Component {
        let mut component = Component::new(
            id.to_string(),
            local_path.to_string(),
            format!("wp-content/plugins/{id}"),
            Some(artifact.to_string()),
        );
        component.extract_command = Some("unzip -o {artifact}".to_string());
        component
    }

    fn failing_build_artifact_component(id: &str, local_path: &str, artifact: &str) -> Component {
        let mut component = artifact_component(id, local_path, artifact);
        component.scripts = Some(ComponentScriptsConfig {
            build: vec![
                "mkdir -p .homeboy-build && printf artifact > .homeboy-build/plugin.zip && exit 42"
                    .to_string(),
            ],
            ..ComponentScriptsConfig::default()
        });
        component
    }

    fn base_deploy_config() -> DeployConfig {
        DeployConfig {
            component_ids: vec![],
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
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
            resume_run_id: None,
        }
    }

    fn init_repo_with_tag_gap(path: &Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("git command")
        };
        assert!(run(&["init", "-q"]).status.success());
        assert!(run(&["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(run(&["config", "user.name", "Test"]).status.success());
        assert!(run(&["commit", "--allow-empty", "-q", "-m", "release"])
            .status
            .success());
        assert!(run(&["tag", "v1.0.0"]).status.success());
        assert!(run(&["commit", "--allow-empty", "-q", "-m", "fix: next"])
            .status
            .success());
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_clean_repo(path: &Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("git command")
        };
        assert!(run(&["init", "-q"]).status.success());
        assert!(run(&["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(run(&["config", "user.name", "Test"]).status.success());
        assert!(run(&["commit", "--allow-empty", "-q", "-m", "init"])
            .status
            .success());
    }

    #[test]
    fn deploy_tag_for_version_formats_regular_release_tag() {
        let component = make_component("sample-plugin", "/tmp/not-a-git-repo");

        assert_eq!(deploy_tag_for_version(&component, "0.139.18"), "v0.139.18");
        assert_eq!(deploy_tag_for_version(&component, "v0.139.18"), "v0.139.18");
    }

    #[test]
    fn deploy_tag_for_version_formats_monorepo_release_tag() {
        let dir = TempDir::new().expect("temp dir");
        init_clean_repo(dir.path());
        let component_dir = dir.path().join("packages/plugin");
        std::fs::create_dir_all(&component_dir).expect("component dir");
        let component = make_component("plugin", &component_dir.to_string_lossy());

        assert_eq!(deploy_tag_for_version(&component, "1.2.3"), "plugin-v1.2.3");
    }

    #[test]
    fn check_uncommitted_changes_reports_non_git_local_path() {
        // A directory exists but is not a git repo — the error must say so clearly
        // instead of reporting "uncommitted changes". (#1141)
        let dir = TempDir::new().expect("temp dir");
        let component = make_component("test", &dir.path().to_string_lossy());

        let result = check_uncommitted_changes(&[component]);
        let err = result.expect_err("should fail for non-git local_path");
        let message = format!("{}", err);
        assert!(
            message.contains("not a git repository"),
            "error should identify the non-git local_path issue, got: {message}"
        );
        assert!(
            !message.contains("uncommitted changes"),
            "error must not conflate non-git with dirty git, got: {message}"
        );
    }

    fn write_component_manifest(dir: &Path, id: &str, build_command: Option<&str>) {
        let mut manifest = serde_json::json!({
            "id": id,
            "remote_path": format!("wp-content/plugins/{id}"),
            "build_artifact": "dist/plugin.zip"
        });

        if let Some(command) = build_command {
            manifest["build_command"] = serde_json::Value::String(command.to_string());
        }

        std::fs::write(dir.join("homeboy.json"), manifest.to_string()).expect("write manifest");
    }

    fn project_with_component_dirs(component_dirs: &[(&str, &Path)]) -> Project {
        Project {
            id: "site".to_string(),
            components: component_dirs
                .iter()
                .map(|(id, path)| ProjectComponentAttachment {
                    id: (*id).to_string(),
                    local_path: path.to_string_lossy().to_string(),
                    remote_path: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn targeted_deploy_validation_ignores_unrequested_invalid_component() {
        let selected = TempDir::new().expect("selected dir");
        let unrelated = TempDir::new().expect("unrelated dir");
        write_component_manifest(selected.path(), "selected", None);
        write_component_manifest(unrelated.path(), "unrelated", Some("npm run legacy-build"));

        let project = project_with_component_dirs(&[
            ("selected", selected.path()),
            ("unrelated", unrelated.path()),
        ]);
        let loaded = load_project_components(&project, &["selected".to_string()], false)
            .expect("targeted component load should ignore unrelated invalid config");

        assert_eq!(
            loaded
                .deployable
                .iter()
                .map(|component| component.id.as_str())
                .collect::<Vec<_>>(),
            vec!["selected"]
        );
        validate_supported_build_configs(&loaded.deployable)
            .expect("unrequested legacy build_command should not block targeted deploy");
    }

    #[test]
    fn targeted_deploy_validation_still_rejects_selected_invalid_component() {
        let selected = TempDir::new().expect("selected dir");
        let unrelated = TempDir::new().expect("unrelated dir");
        write_component_manifest(selected.path(), "selected", Some("npm run legacy-build"));
        write_component_manifest(unrelated.path(), "unrelated", None);

        let project = project_with_component_dirs(&[
            ("selected", selected.path()),
            ("unrelated", unrelated.path()),
        ]);
        let loaded = load_project_components(&project, &["selected".to_string()], false)
            .expect("targeted component load should include selected component");

        let err = validate_supported_build_configs(&loaded.deployable)
            .expect_err("selected legacy build_command should still fail targeted deploy");

        assert!(err.message.contains("unsupported legacy build_command"));
        assert_eq!(err.details["field"].as_str(), Some("build_command"));
    }

    /// Write a component manifest that declares a (missing) required extension.
    fn write_component_manifest_with_extension(dir: &Path, id: &str, extension_id: &str) {
        let manifest = serde_json::json!({
            "id": id,
            "remote_path": format!("wp-content/plugins/{id}"),
            "build_artifact": "dist/plugin.zip",
            "extensions": { extension_id: {} },
        });
        std::fs::write(dir.join("homeboy.json"), manifest.to_string()).expect("write manifest");
    }

    #[test]
    fn check_mode_skips_component_with_missing_extension_instead_of_aborting() {
        with_isolated_home(|_| {
            let gated = TempDir::new().expect("gated dir");
            let wp = TempDir::new().expect("wp dir");
            // One component requires an uninstalled extension; the other is a plain
            // deployable WP component the operator actually cares about.
            write_component_manifest_with_extension(
                gated.path(),
                "gated",
                "nonexistent-extension-xyz789",
            );
            write_component_manifest(wp.path(), "wp", None);

            let project =
                project_with_component_dirs(&[("gated", gated.path()), ("wp", wp.path())]);

            // --all --check: requested_ids empty, check = true.
            let loaded = load_project_components(&project, &[], true)
                .expect("check mode must not hard-fail on missing extension");

            // The WP component is still deployable/inspectable.
            assert_eq!(
                loaded
                    .deployable
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["wp"]
            );

            // The extension-gated component is recorded with a reason, not dropped silently.
            assert_eq!(loaded.extension_skipped.len(), 1);
            let skip = &loaded.extension_skipped[0];
            assert_eq!(skip.id, "gated");
            assert!(
                skip.reason.contains("nonexistent-extension-xyz789"),
                "reason should name the missing extension, got: {}",
                skip.reason
            );
        });
    }

    #[test]
    fn non_check_mode_still_hard_fails_on_missing_extension() {
        with_isolated_home(|_| {
            let gated = TempDir::new().expect("gated dir");
            write_component_manifest_with_extension(
                gated.path(),
                "gated",
                "nonexistent-extension-xyz789",
            );

            let project = project_with_component_dirs(&[("gated", gated.path())]);

            // --all (deploy, not check): a missing extension must still abort.
            let err = match load_project_components(&project, &[], false) {
                Ok(_) => panic!("non-check mode must hard-fail on missing extension"),
                Err(err) => err,
            };

            assert_eq!(err.code, homeboy_core::error::ErrorCode::ExtensionNotFound);
            assert!(err.message.contains("nonexistent-extension-xyz789"));
        });
    }

    #[test]
    fn extension_skipped_results_report_skipped_status_with_reason() {
        let project = Project {
            id: "site".to_string(),
            ..Default::default()
        };
        let skipped = vec![ExtensionSkippedComponent {
            id: "gated".to_string(),
            reason: "missing extension rust".to_string(),
        }];

        let results = extension_skipped_results(&skipped, &project, "/var/www/site");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "gated");
        assert_eq!(results[0].status, "skipped");
        assert!(results[0]
            .warnings
            .iter()
            .any(|w| w.contains("missing extension rust")));
    }

    #[test]
    fn check_uncommitted_changes_passes_for_clean_git_repo() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path();

        // Initialize an empty git repo with one commit so the working dir is clean.
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("git command")
        };
        assert!(run(&["init", "-q"]).status.success());
        assert!(run(&["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(run(&["config", "user.name", "Test"]).status.success());
        assert!(run(&["commit", "--allow-empty", "-q", "-m", "init"])
            .status
            .success());

        let component = make_component("test", &path.to_string_lossy());
        check_uncommitted_changes(&[component]).expect("clean git repo should pass");
    }

    #[test]
    fn check_uncommitted_changes_ignores_homeboy_build_artifacts() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path();

        init_clean_repo(path);
        std::fs::create_dir_all(path.join(".homeboy-build")).expect("build dir");
        std::fs::write(path.join(".homeboy-build/plugin.zip"), "artifact").expect("artifact");

        let component = make_component("test", &path.to_string_lossy());
        check_uncommitted_changes(&[component])
            .expect("generated Homeboy deploy artifacts should not block deploy");
    }

    #[test]
    fn check_uncommitted_changes_ignores_untracked_deploy_target_debris() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path();

        init_clean_repo(path);
        std::fs::create_dir_all(path.join("wp-content/plugins/sample-plugin/sample-plugin"))
            .expect("deploy debris dir");
        std::fs::write(
            path.join("wp-content/plugins/sample-plugin/sample-plugin/plugin.php"),
            "<?php",
        )
        .expect("deploy debris file");

        let mut component = make_component("sample-plugin", &path.to_string_lossy());
        component.remote_path = "wp-content/plugins/sample-plugin".to_string();
        component.build_artifact = Some("dist/sample-plugin.zip".to_string());

        check_uncommitted_changes(&[component])
            .expect("untracked deploy-target debris should not block deploy");
    }

    #[test]
    fn check_uncommitted_changes_still_rejects_source_changes() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path();

        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .expect("git command")
        };
        assert!(run(&["init", "-q"]).status.success());
        assert!(run(&["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(run(&["config", "user.name", "Test"]).status.success());
        assert!(run(&["commit", "--allow-empty", "-q", "-m", "init"])
            .status
            .success());
        std::fs::create_dir_all(path.join(".homeboy-build")).expect("build dir");
        std::fs::write(path.join(".homeboy-build/plugin.zip"), "artifact").expect("artifact");
        std::fs::write(path.join("src.rs"), "source\n").expect("source");

        let component = make_component("test", &path.to_string_lossy());
        let err = check_uncommitted_changes(&[component])
            .expect_err("source changes should still block deploy");

        assert!(err.message.contains("uncommitted changes"));
        assert!(err.message.contains("src.rs"));
        assert!(err.message.contains("known generated artifacts ignored"));
        assert!(err.message.contains(".homeboy-build/"));
    }

    #[test]
    fn auto_pull_version_drift_message_reports_changed_version() {
        let component = make_component("sample-plugin", "/tmp/sample-plugin");

        let message =
            auto_pull_version_drift_message(&component, Some("0.139.12"), Some("0.139.13"))
                .expect("version drift message");

        assert!(message.contains("sample-plugin"));
        assert!(message.contains("0.139.12"));
        assert!(message.contains("0.139.13"));
    }

    #[test]
    fn auto_pull_version_drift_message_skips_unchanged_version() {
        let component = make_component("sample-plugin", "/tmp/sample-plugin");

        assert!(
            auto_pull_version_drift_message(&component, Some("0.139.12"), Some("0.139.12"))
                .is_none()
        );
    }

    #[test]
    fn tagged_deploy_allows_head_ahead_of_latest_tag() {
        let dir = TempDir::new().expect("temp dir");
        init_repo_with_tag_gap(dir.path());

        let component = make_component("test", &dir.path().to_string_lossy());
        let mut config = base_deploy_config();
        config.tagged = true;

        check_unreleased_commits(&[component], &config)
            .expect("--tagged deploys the latest tag and should not require --force");
    }

    #[test]
    fn default_tagged_release_guard_still_blocks_unreleased_head() {
        let dir = TempDir::new().expect("temp dir");
        init_repo_with_tag_gap(dir.path());

        let component = make_component("test", &dir.path().to_string_lossy());
        let config = base_deploy_config();

        let err = check_unreleased_commits(&[component], &config)
            .expect_err("default tag deploy should still require an explicit override");
        assert!(
            err.message.contains("HEAD has unreleased commits"),
            "unexpected error: {}",
            err.message
        );
    }

    #[test]
    fn default_release_guard_accepts_component_prefixed_tag_at_head() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        let plugin_path = root.join("plugins/host/studio-native");
        let theme_path = root.join("themes/host/studio-native");

        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(&plugin_path).expect("plugin path");
        std::fs::create_dir_all(&theme_path).expect("theme path");
        std::fs::write(plugin_path.join("package.json"), r#"{"version":"0.13.3"}"#)
            .expect("plugin file");
        std::fs::write(theme_path.join("style.css"), "Version: 0.1.3\n").expect("theme file");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "release components"]);
        run_git(root, &["tag", "studio-native-v0.13.3"]);
        run_git(root, &["tag", "studio-native-theme-v0.1.3"]);
        run_git(root, &["tag", "v0.13.2"]);

        let components = vec![
            make_component("studio-native", &plugin_path.to_string_lossy()),
            make_component("studio-native-theme", &theme_path.to_string_lossy()),
        ];

        check_unreleased_commits(&components, &base_deploy_config())
            .expect("freshly released nested components must pass the default deploy gate");
    }

    #[test]
    fn default_release_guard_rejects_commits_after_component_prefixed_tag() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path();
        let plugin_path = root.join("plugins/host/studio-native");

        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(&plugin_path).expect("plugin path");
        std::fs::write(plugin_path.join("package.json"), r#"{"version":"0.13.3"}"#)
            .expect("plugin file");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "release studio native"]);
        run_git(root, &["tag", "studio-native-v0.13.3"]);
        run_git(root, &["tag", "v0.13.2"]);

        std::fs::write(plugin_path.join("package.json"), r#"{"version":"0.13.4"}"#)
            .expect("plugin file update");
        run_git(root, &["add", "."]);
        run_git(
            root,
            &["commit", "-q", "-m", "fix: unreleased plugin change"],
        );

        let component = make_component("studio-native", &plugin_path.to_string_lossy());
        let err = check_unreleased_commits(&[component], &base_deploy_config())
            .expect_err("component commits after the prefixed release tag must still be refused");

        assert!(
            err.message
                .contains("studio-native (1 commits ahead of studio-native-v0.13.3)"),
            "unexpected error: {}",
            err.message
        );
        assert!(
            !err.message.contains("v0.13.2"),
            "deploy gate must not fall back to the stale plain tag: {}",
            err.message
        );
    }

    #[test]
    fn default_dry_run_plans_latest_tag_not_feature_branch_head() {
        let dir = TempDir::new().expect("temp dir");
        init_repo_with_tag_gap(dir.path());
        assert!(std::process::Command::new("git")
            .args(["checkout", "-q", "-b", "feature/deploy"])
            .current_dir(dir.path())
            .status()
            .expect("git checkout")
            .success());

        let component = make_component("demo", &dir.path().to_string_lossy());
        let mut config = base_deploy_config();
        config.dry_run = true;

        let result = run_dry_run_mode(
            &[component],
            &HashMap::new(),
            &HashMap::new(),
            &Project::default(),
            "",
            &config,
        )
        .expect("default dry-run should plan the latest deploy tag");

        let planned_ref = result.results[0]
            .deployed_ref
            .as_deref()
            .expect("planned deploy ref");
        assert!(
            planned_ref.starts_with("v1.0.0"),
            "default dry-run should plan the latest tag, got {planned_ref}"
        );
        assert!(
            !planned_ref.contains("feature/deploy"),
            "default dry-run must not plan the current feature branch HEAD: {planned_ref}"
        );
    }

    #[test]
    fn head_dry_run_explicitly_plans_current_branch_head() {
        let dir = TempDir::new().expect("temp dir");
        init_repo_with_tag_gap(dir.path());
        assert!(std::process::Command::new("git")
            .args(["checkout", "-q", "-b", "feature/deploy"])
            .current_dir(dir.path())
            .status()
            .expect("git checkout")
            .success());

        let component = make_component("demo", &dir.path().to_string_lossy());
        let mut config = base_deploy_config();
        config.dry_run = true;
        config.head = true;

        let result = run_dry_run_mode(
            &[component],
            &HashMap::new(),
            &HashMap::new(),
            &Project::default(),
            "",
            &config,
        )
        .expect("--head dry-run should plan the current branch");

        assert_eq!(
            result.results[0].deployed_ref.as_deref(),
            Some("feature/deploy (HEAD)")
        );
    }

    #[test]
    fn checkout_deploy_tags_restores_prior_checkout_when_later_checkout_fails() {
        let first = TempDir::new().expect("first temp dir");
        let second = TempDir::new().expect("second temp dir");
        init_repo_with_tag_gap(first.path());
        init_clean_repo(second.path());

        let starting_ref = git_stdout(first.path(), &["symbolic-ref", "--short", "HEAD"]);
        let starting_head = git_stdout(first.path(), &["rev-parse", "HEAD"]);
        let components = vec![
            make_component("first", &first.path().to_string_lossy()),
            make_component("second", &second.path().to_string_lossy()),
        ];

        let err = match checkout_deploy_tags(&components, Some("1.0.0")) {
            Ok(_) => panic!("missing later tag should fail checkout_deploy_tags"),
            Err(err) => err,
        };

        assert!(
            err.message
                .contains("Failed to checkout tag v1.0.0 for 'second'"),
            "unexpected error: {}",
            err.message
        );
        assert_eq!(
            git_stdout(first.path(), &["symbolic-ref", "--short", "HEAD"]),
            starting_ref,
            "first component should be restored to its starting branch"
        );
        assert_eq!(
            git_stdout(first.path(), &["rev-parse", "HEAD"]),
            starting_head,
            "first component should be restored to its starting commit"
        );
    }

    #[test]
    fn provenance_ref_reports_tag_and_sha_without_gap() {
        let checkout = TagCheckout {
            component_id: "demo".to_string(),
            tag: "v1.0.0".to_string(),
            original_ref: "main".to_string(),
            local_path: "/tmp/demo".to_string(),
            tag_sha: Some("abc1234".to_string()),
            head_ahead: 0,
        };

        assert_eq!(checkout.provenance_ref(), "v1.0.0 (abc1234)");
    }

    #[test]
    fn provenance_ref_flags_stale_tag_when_head_was_ahead() {
        let checkout = TagCheckout {
            component_id: "demo".to_string(),
            tag: "v1.0.0".to_string(),
            original_ref: "release/main".to_string(),
            local_path: "/tmp/demo".to_string(),
            tag_sha: Some("abc1234".to_string()),
            head_ahead: 2,
        };

        let label = checkout.provenance_ref();
        assert!(
            label.starts_with("v1.0.0 (abc1234)"),
            "stale ref must still name the exact tag and sha: {label}"
        );
        assert!(
            label.contains("HEAD was 2 commit(s) ahead, not deployed"),
            "stale ref must disclose the undeployed HEAD commits: {label}"
        );
    }

    #[test]
    fn checkout_deploy_tags_records_head_ahead_for_stale_tag() {
        let dir = TempDir::new().expect("temp dir");
        init_repo_with_tag_gap(dir.path());

        let component = make_component("demo", &dir.path().to_string_lossy());
        let checkouts =
            checkout_deploy_tags(&[component], None).expect("stale-tag checkout should succeed");

        assert_eq!(checkouts.len(), 1, "one component should be checked out");
        let checkout = &checkouts[0];
        assert_eq!(checkout.tag, "v1.0.0");
        assert_eq!(
            checkout.head_ahead, 1,
            "HEAD was one commit ahead of the deployed tag"
        );
        assert!(
            checkout.tag_sha.is_some(),
            "deployed tag sha should be resolved for provenance"
        );
        assert!(
            checkout
                .provenance_ref()
                .contains("HEAD was 1 commit(s) ahead, not deployed"),
            "provenance must disclose the stale-tag gap: {}",
            checkout.provenance_ref()
        );

        // checkout_deploy_tags leaves the repo on the tag; restore for cleanliness.
        restore_branches(&checkouts);
    }

    #[test]
    fn expected_version_rejects_stale_component_worktree() {
        let dir = TempDir::new().expect("temp dir");
        let package_json = dir.path().join("package.json");
        std::fs::write(&package_json, r#"{"version":"1.0.0"}"#).expect("write package.json");

        let mut component = make_component("demo", &dir.path().to_string_lossy());
        component.version_targets = Some(vec![homeboy_core::component::VersionTarget {
            file: "package.json".to_string(),
            pattern: Some(r#""version"\s*:\s*"([^"]+)""#.to_string()),
            artifact_path: None,
        }]);

        let err = verify_expected_version(&[component], "1.0.1")
            .expect_err("stale registered worktree must not pass release deploy preflight");

        assert!(
            err.message
                .contains("local version is 1.0.0 (expected 1.0.1)"),
            "unexpected error: {}",
            err.message
        );
    }

    #[test]
    fn deploy_preflight_aborts_batch_when_later_artifact_is_missing() {
        let dir = TempDir::new().expect("temp dir");
        let ready_path = dir.path().join("ready");
        let missing_path = dir.path().join("missing");
        std::fs::create_dir_all(ready_path.join("dist")).expect("ready dist");
        std::fs::create_dir_all(&missing_path).expect("missing component dir");
        std::fs::write(ready_path.join("dist/ready.zip"), b"artifact").expect("ready artifact");

        let components = vec![
            artifact_component("ready", &ready_path.to_string_lossy(), "dist/ready.zip"),
            artifact_component(
                "missing",
                &missing_path.to_string_lossy(),
                "dist/missing.zip",
            ),
        ];
        let project = Project {
            id: "site".to_string(),
            ..Project::default()
        };
        let mut config = base_deploy_config();
        config.skip_build = true;
        config.force = true;

        let failures = match prepare_component_deployments(
            &components,
            &config,
            &project,
            "/srv/site",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        ) {
            Ok(_) => panic!("a later missing artifact must abort the whole deploy batch"),
            Err(failures) => failures,
        };

        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].id, "missing");
        assert_eq!(failures[0].status, "failed");
        assert!(
            failures[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("missing.zip"),
            "unexpected error: {:?}",
            failures[0].error
        );
    }

    #[test]
    fn exact_ref_preflight_packages_verified_subpath_not_stale_checkout_artifact() {
        let _home_env = home_env_guard();
        exact_ref_preflight_fixture(None);
    }

    #[test]
    fn exact_ref_command_preparation_rebuilds_extension_owned_artifact() {
        with_isolated_home(|home| {
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/fixture-packager");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("fixture-packager.json"),
                r#"{"name":"fixture-packager","version":"1.0.0","build":{"extension_script":"build.sh","artifact_pattern":"build/plugin.zip"}}"#,
            )
            .expect("extension manifest");
            std::fs::write(
                extension_dir.join("build.sh"),
                "mkdir -p build\n[ -f build/plugin.zip ] || git archive --format=zip --output=build/plugin.zip HEAD\n",
            )
            .expect("extension build script");

            let repo = TempDir::new().expect("repo");
            run_git(repo.path(), &["init", "-q"]);
            run_git(repo.path(), &["config", "user.email", "test@example.com"]);
            run_git(repo.path(), &["config", "user.name", "Test"]);
            std::fs::write(repo.path().join("payload.txt"), "stale\n").expect("stale payload");
            std::fs::create_dir_all(repo.path().join("build")).expect("build directory");
            std::fs::write(repo.path().join("build/plugin.zip"), "stale artifact\n")
                .expect("stale artifact");
            run_git(repo.path(), &["add", "."]);
            run_git(repo.path(), &["commit", "-q", "-m", "stale artifact"]);

            std::fs::write(repo.path().join("payload.txt"), "requested\n")
                .expect("requested payload");
            run_git(repo.path(), &["commit", "-am", "requested", "-q"]);
            run_git(repo.path(), &["branch", "requested"]);
            let requested_sha = git_stdout(repo.path(), &["rev-parse", "requested"]);
            std::fs::write(repo.path().join("payload.txt"), "configured\n")
                .expect("configured payload");
            run_git(repo.path(), &["commit", "-am", "configured", "-q"]);

            let mut component = Component::new(
                "plugin".to_string(),
                repo.path().display().to_string(),
                "plugins/plugin".to_string(),
                None,
            );
            component.extract_command = Some("unzip -o {{artifact}}".to_string());
            component.extensions = Some(HashMap::from([(
                "fixture-packager".to_string(),
                homeboy_core::component::ScopedExtensionConfig::default(),
            )]));
            let checkout = ExactRefCheckout::materialize(&component, "requested")
                .expect("materialize requested ref");
            checkout.verify().expect("verify requested ref");

            let mut config = base_deploy_config();
            config.requested_ref = Some("requested".to_string());
            config.force = true;
            let prepared = prepare_component_deployments(
                &[checkout.component.clone()],
                &config,
                &Project::default(),
                "/srv/site",
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .expect("prepare exact-ref extension artifact");

            let artifact = prepared[0].artifact_path.as_ref().expect("artifact path");
            let file = std::fs::File::open(artifact).expect("open artifact");
            let mut archive = zip::ZipArchive::new(file).expect("read artifact");
            assert_eq!(
                std::io::read_to_string(archive.by_name("payload.txt").expect("payload entry"))
                    .expect("payload content"),
                "requested\n"
            );
            assert_eq!(
                prepared[0]
                    .config
                    .prepared_artifact
                    .as_ref()
                    .expect("prepared artifact")
                    .source_commit,
                requested_sha
            );
        });
    }

    #[test]
    fn exact_ref_hydration_fixtures_build_concurrently_without_crossing_worktrees() {
        let _home_env = home_env_guard();
        let barrier = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            for _ in 0..2 {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || exact_ref_preflight_fixture(Some(barrier)));
            }
        });
    }

    fn exact_ref_preflight_fixture(barrier: Option<Arc<Barrier>>) {
        let repo = TempDir::new().expect("repo");
        let root = repo.path();
        let component_path = root.join("packages/plugin");
        std::fs::create_dir_all(&component_path).expect("component path");
        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Test"]);

        std::fs::write(component_path.join("target-marker.txt"), "target\n")
            .expect("target marker");
        std::fs::write(
            root.join("homeboy-deps.json"),
            r#"{
                "provider": "fixture-workspace",
                "commands": {
                    "install": {
                        "argv": ["sh", "-c", "mkdir -p packages/plugin/node_modules/.bin && printf '#!/bin/sh\\ncat target-marker.txt\\n' > packages/plugin/node_modules/.bin/fixture-tool && chmod +x packages/plugin/node_modules/.bin/fixture-tool"]
                    }
                }
            }"#,
        )
        .expect("workspace dependency provider");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "target"]);
        run_git(root, &["branch", "target"]);

        std::fs::remove_file(component_path.join("target-marker.txt")).expect("remove target");
        std::fs::write(component_path.join("stale-marker.txt"), "stale\n").expect("stale marker");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-q", "-m", "stale"]);

        let stale_artifact = component_path.join("dist/plugin.zip");
        std::fs::create_dir_all(stale_artifact.parent().expect("artifact parent")).expect("dist");
        let stale_file = std::fs::File::create(&stale_artifact).expect("stale artifact");
        let mut stale_zip = zip::ZipWriter::new(stale_file);
        use std::io::Write;
        stale_zip
            .start_file(
                "plugin/stale-marker.txt",
                zip::write::FileOptions::default(),
            )
            .expect("stale entry");
        stale_zip.write_all(b"stale\n").expect("stale bytes");
        stale_zip.finish().expect("finish stale zip");

        let component = Component {
            id: "plugin".to_string(),
            local_path: component_path.to_string_lossy().to_string(),
            remote_path: "plugins/plugin".to_string(),
            build_artifact: Some(stale_artifact.to_string_lossy().to_string()),
            extract_command: Some("unzip -o {{artifact}}".to_string()),
            scripts: Some(ComponentScriptsConfig {
                build: vec![
                    "mkdir -p dist && node_modules/.bin/fixture-tool > dist/target-marker.txt && (cd dist && zip -q plugin.zip target-marker.txt)".to_string(),
                ],
                ..ComponentScriptsConfig::default()
            }),
            ..Component::default()
        };
        let checkout =
            ExactRefCheckout::materialize(&component, "target").expect("materialize target ref");
        checkout.verify().expect("verify target ref");
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let hydration = checkout
            .hydrate_dependencies(false)
            .expect("hydrate the materialized workspace dependencies");
        assert_eq!(
            hydration
                .expect("workspace provider must be discovered")
                .component_path,
            Path::new(&checkout.component.local_path)
                .ancestors()
                .nth(2)
                .expect("repository root")
                .display()
                .to_string()
        );
        assert!(
            Path::new(&checkout.component.local_path)
                .join("node_modules/.bin/fixture-tool")
                .is_file(),
            "workspace dependency hydration must create the checkout-local build tool"
        );
        let mut config = base_deploy_config();
        config.requested_ref = Some("target".to_string());
        config.force = true;
        let prepared = prepare_component_deployments(
            &[checkout.component.clone()],
            &config,
            &Project::default(),
            "/srv/site",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("prepare exact-ref artifact");

        let artifact = prepared[0].artifact_path.as_ref().expect("artifact path");
        let file = std::fs::File::open(artifact).expect("open artifact");
        let mut archive = zip::ZipArchive::new(file).expect("read artifact");
        assert_eq!(
            std::io::read_to_string(archive.by_name("target-marker.txt").expect("target entry"))
                .expect("target content"),
            "target\n"
        );
        assert!(archive.by_name("stale-marker.txt").is_err());
        assert!(
            !Path::new(&component.local_path)
                .join("node_modules/.bin/fixture-tool")
                .exists(),
            "configured checkout must not supply the build tool"
        );
        assert!(
            !artifact.starts_with(&component.local_path),
            "the stale configured-checkout artifact must not be reused"
        );
    }

    #[test]
    fn deploy_preflight_preserves_preexisting_artifact_after_failed_build() {
        with_isolated_home(|_| {
            let dir = TempDir::new().expect("temp dir");
            let component = failing_build_artifact_component(
                "failing",
                &dir.path().to_string_lossy(),
                ".homeboy-build/plugin.zip",
            );
            let project = Project {
                id: "site".to_string(),
                ..Project::default()
            };
            let mut config = base_deploy_config();
            config.force = true;
            config.head = true;

            let (build_exit_code, build_error) =
                homeboy_extension::build::build_component(&component);
            assert_eq!(build_exit_code, Some(42));
            assert!(
                build_error.is_some(),
                "fixture build must fail before deploy cleanup can validate failure handling"
            );

            let failures = match prepare_component_deployments(
                &[component],
                &config,
                &project,
                "/srv/site",
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            ) {
                Ok(_) => panic!("failed build should abort preflight"),
                Err(failures) => failures,
            };

            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].build_exit_code, Some(42));
            assert!(
                dir.path().join(".homeboy-build/plugin.zip").exists(),
                "failed preparation must preserve an artifact that existed before it started"
            );
        });
    }
}
