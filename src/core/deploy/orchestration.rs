use std::collections::HashMap;

use crate::core::component::Component;
use crate::core::context::RemoteProjectContext;
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::project::Project;
use crate::core::release::version;

use super::execution::{
    execute_preflighted_component_deploy, prepare_component_deploy, release_artifact_plan,
    PreparedComponentDeploy, ReleaseArtifactPlan,
};
use super::generated_artifacts::uncommitted_file_report_excluding_known_generated;
use super::orchestration_tag_checkout::{
    checkout_deploy_tags, deploy_tag_for_version, restore_branches, TagCheckout,
};
use super::path_roots::{project_with_detected_path_roots, resolve_effective_remote_path};
use super::planning::{
    calculate_component_status_with_git_cache, calculate_release_state, load_project_components,
    plan_components, ExtensionSkippedComponent, GitProbeCache,
};
use super::types::{
    ComponentDeployResult, ComponentStatus, DeployArtifactSource, DeployConfig,
    DeployOrchestrationResult, DeploySummary,
};
use super::version_overrides::fetch_remote_versions_for_project;

/// Main deploy orchestration entry point.
/// Handles component selection, building, and deployment.
pub(super) fn deploy_components(
    config: &DeployConfig,
    project: &Project,
    ctx: &RemoteProjectContext,
    base_path: &str,
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

    // Gather versions
    let mut local_versions: HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    let remote_versions =
        if config.outdated || config.behind_upstream || config.dry_run || config.check {
            fetch_remote_versions_for_project(&components, Some(&project), base_path, &ctx.client)
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

    // Sync: pull latest changes before deploying (unless --no-pull or --skip-build)
    if !config.no_pull && !config.skip_build {
        sync_components(&components)?;
    }

    // Warn when --head deploys from a non-default branch (safety guardrail)
    if config.head && !config.skip_build {
        warn_non_default_branch(&components, config)?;
    }

    if !config.force {
        check_uncommitted_changes(&components)?;
    }

    // Check for HEAD-vs-tag gap before the tag checkout.
    if !config.head && !config.skip_build {
        check_unreleased_commits(&components, config)?;
    }

    // Checkout the deploy tag for each component (unless --head or --skip-build).
    let tag_checkouts = if !config.head && !config.skip_build {
        checkout_deploy_tags(&components, config.expected_version.as_deref())?
    } else {
        Vec::new()
    };

    // Verify expected version if --version was specified
    if let Some(ref expected) = config.expected_version {
        if let Err(err) = verify_expected_version(&components, expected) {
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

    // Build and validate every local artifact before the first remote write.
    let prepared_deployments = match prepare_component_deployments(
        &components,
        config,
        &project,
        base_path,
        &local_versions,
        &remote_versions,
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

    for prepared in &prepared_deployments {
        let component = &prepared.component;

        let mut result = execute_preflighted_component_deploy(prepared, ctx, base_path, &project);

        // Record which git ref was deployed
        if let Some(checkout) = tag_checkouts
            .iter()
            .find(|c| c.component_id == component.id)
        {
            result = result.with_deployed_ref(checkout.provenance_ref());
        } else if config.head {
            // Deploying from HEAD — record the current branch
            if let Some(branch) = crate::core::engine::command::run_in_optional(
                &component.local_path,
                "git",
                &["rev-parse", "--abbrev-ref", "HEAD"],
            ) {
                result = result.with_deployed_ref(format!("{} (HEAD)", branch));
            }
        }

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

/// Run the project's post-deploy smoke check, recording the outcome on the
/// deploy results.
///
/// Returns:
/// - `None` when no smoke check is configured/enabled,
/// - `Some(true)` when the smoke FAILED and should fail the deploy,
/// - `Some(false)` when the smoke passed or only warned.
///
/// Warnings/errors are appended to the first deployed component result so they
/// surface in CLI/JSON output alongside the deploy that triggered them.
fn run_post_deploy_smoke(project: &Project, results: &mut [ComponentDeployResult]) -> Option<bool> {
    let config = project.smoke_check.as_ref()?;
    let outcome = super::smoke::run_smoke_check(config)?;

    if outcome.is_ok() {
        log_status!(
            "deploy",
            "Post-deploy smoke check passed for '{}' ({})",
            project.id,
            config.url
        );
        return Some(false);
    }

    let detail = outcome
        .failure_detail()
        .unwrap_or("post-deploy smoke check failed")
        .to_string();

    if config.warn_only {
        log_status!("deploy", "Warning: {} (warn_only)", detail);
        if let Some(first) = results.iter_mut().find(|r| r.status == "deployed") {
            first.warnings.push(format!("{} (warn_only)", detail));
        }
        return Some(false);
    }

    log_status!(
        "deploy",
        "{} — failing deploy; roll back the release",
        detail
    );
    if let Some(first) = results.iter_mut().find(|r| r.status == "deployed") {
        first.error = Some(detail);
    }
    Some(true)
}

fn validate_supported_build_configs(components: &[Component]) -> Result<()> {
    for component in components {
        component.validate_supported_build_config()?;
    }

    Ok(())
}

fn prepare_component_deployments(
    components: &[Component],
    config: &DeployConfig,
    project: &Project,
    base_path: &str,
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
) -> std::result::Result<Vec<PreparedComponentDeploy>, Vec<ComponentDeployResult>> {
    let mut prepared_deployments = Vec::new();
    let mut failures = Vec::new();

    for component in components {
        let component = crate::core::project::apply_component_overrides(component, project);
        let effective_config = config.clone();

        match prepare_component_deploy(
            &component,
            &effective_config,
            base_path,
            project,
            local_versions.get(&component.id).cloned(),
            remote_versions.get(&component.id).cloned(),
        ) {
            Ok(prepared) => prepared_deployments.push(prepared),
            Err(result) => failures.push(result),
        }
    }

    if failures.is_empty() {
        Ok(prepared_deployments)
    } else {
        Err(failures)
    }
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

/// Check mode: return component status without building or deploying.
fn run_check_mode(
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
fn extension_skipped_results(
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
fn run_dry_run_mode(
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

/// Verify no components have uncommitted changes before deployment.
/// Warn when `--head` would deploy from a non-default branch.
///
/// Detects the current branch for each component and compares it against the
/// default branch (via `git symbolic-ref refs/remotes/origin/HEAD`, falling
/// back to "main"). If a component is on a feature branch, this is likely
/// unintentional — the user probably meant to deploy the default branch.
///
/// With `--force`, this emits a log warning but proceeds. Without `--force`,
/// it returns an error so the user can switch branches or confirm intent.
fn warn_non_default_branch(components: &[Component], config: &DeployConfig) -> Result<()> {
    for component in components {
        if component.is_file_component() {
            continue;
        }

        let path = &component.local_path;

        // Get current branch
        let current_branch = match crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
        ) {
            Some(branch) if branch != "HEAD" => branch, // "HEAD" means detached
            _ => continue,                              // detached or error — skip
        };

        // Detect default branch from remote HEAD symref, fallback to "main"
        let default_branch = crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        )
        .map(|s| {
            // Output is like "origin/main" — strip the remote prefix
            s.strip_prefix("origin/").unwrap_or(&s).to_string()
        })
        .unwrap_or_else(|| "main".to_string());

        if current_branch != default_branch {
            let message = format!(
                "Component '{}' is on branch '{}', not '{}' (default)",
                component.id, current_branch, default_branch
            );

            if config.force {
                log_status!("deploy", "Warning: {}", message);
            } else {
                return Err(Error::validation_invalid_argument(
                    "head",
                    message,
                    None,
                    Some(vec![
                        format!(
                            "Switch to the default branch: git -C {} checkout {}",
                            component.local_path, default_branch
                        ),
                        "Use --force to deploy from the current branch anyway".to_string(),
                    ]),
                ));
            }
        }
    }
    Ok(())
}

fn check_uncommitted_changes(components: &[Component]) -> Result<()> {
    // Partition components into "non-git local_path" vs "dirty git repo" so we can
    // emit the right diagnostic. Conflating the two leaves users chasing a
    // nonexistent uncommitted-changes problem when the real issue is that
    // local_path doesn't point at a git checkout. (#1141)
    let mut non_git: Vec<&Component> = Vec::new();
    let mut dirty: Vec<DirtyComponent> = Vec::new();

    for component in components {
        if component.is_file_component() {
            continue;
        }
        if !git::is_git_repo(&component.local_path) {
            non_git.push(component);
            continue;
        }
        match uncommitted_file_report_excluding_known_generated(component) {
            Ok(report) if report.unexpected.is_empty() => {}
            Ok(report) => dirty.push(DirtyComponent {
                id: component.id.clone(),
                unexpected_paths: report.unexpected,
                known_generated_paths: report.known_generated,
            }),
            Err(_) => dirty.push(DirtyComponent {
                id: component.id.clone(),
                unexpected_paths: Vec::new(),
                known_generated_paths: Vec::new(),
            }),
        }
    }

    if !non_git.is_empty() {
        let ids: Vec<&str> = non_git.iter().map(|c| c.id.as_str()).collect();
        let mut hints: Vec<String> = non_git
            .iter()
            .map(|c| {
                format!(
                    "Repoint '{}' at a git checkout: homeboy component set {} --local-path <path-to-git-workspace>",
                    c.id, c.id
                )
            })
            .collect();
        hints.push(
            "Initialize a git repo at the existing local_path if the contents are the source of truth: git -C <local_path> init && git add . && git commit -m 'initial'"
                .to_string(),
        );
        hints.push("Or deploy with --force to bypass the git-clean check".to_string());
        let paths: Vec<String> = non_git
            .iter()
            .map(|c| format!("{} ({})", c.id, c.local_path))
            .collect();
        return Err(Error::validation_invalid_argument(
            "components",
            format!(
                "local_path is not a git repository for: {}. The uncommitted-changes check requires a git checkout.",
                paths.join(", ")
            ),
            Some(ids.join(",")),
            Some(hints),
        ));
    }

    if !dirty.is_empty() {
        let dirty_ids: Vec<&str> = dirty.iter().map(|row| row.id.as_str()).collect();
        return Err(Error::validation_invalid_argument(
            "components",
            format!(
                "Components have uncommitted changes: {}. {}",
                dirty_ids.join(", "),
                dirty_worktree_path_summary(&dirty)
            ),
            None,
            Some(vec![
                "Commit your changes before deploying to ensure deployed code is tracked"
                    .to_string(),
                "Known generated artifacts are ignored by the deploy dirty gate; remove them with `homeboy cleanup --apply` if you want a clean worktree"
                    .to_string(),
                "Use --force to deploy anyway".to_string(),
            ]),
        ));
    }
    Ok(())
}

struct DirtyComponent {
    id: String,
    unexpected_paths: Vec<String>,
    known_generated_paths: Vec<String>,
}

fn dirty_worktree_path_summary(dirty: &[DirtyComponent]) -> String {
    dirty
        .iter()
        .map(|row| {
            let unexpected = if row.unexpected_paths.is_empty() {
                "<unable to read git status>".to_string()
            } else {
                row.unexpected_paths.join(", ")
            };
            let known_generated = if row.known_generated_paths.is_empty() {
                String::new()
            } else {
                format!(
                    "; known generated artifacts ignored: {}",
                    row.known_generated_paths.join(", ")
                )
            };
            format!(
                "{} unexpected paths: {}{}",
                row.id, unexpected, known_generated
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fetch and pull latest changes for each component before deploying.
///
/// Prevents deploying stale code when the local clone is behind remote.
/// Runs `git fetch` + `git pull` for each component that has an upstream.
/// Aborts if pull fails (e.g., merge conflicts).
fn sync_components(components: &[Component]) -> Result<()> {
    for component in components {
        // File components are not git repos — skip sync
        if component.is_file_component() {
            continue;
        }

        let path = &component.local_path;

        let version_before_pull = version::get_component_version(component);

        // Check if behind remote
        match git::fetch_and_get_behind_count(path) {
            Ok(Some(behind)) => {
                log_status!(
                    "deploy",
                    "'{}' is {} commit(s) behind remote — pulling...",
                    component.id,
                    behind
                );
                let pull_result = git::pull(Some(&component.id))?;
                if !pull_result.success {
                    return Err(Error::git_command_failed(format!(
                        "Failed to pull '{}': {}",
                        component.id,
                        pull_result.stderr.lines().next().unwrap_or("unknown error")
                    )));
                }
                if let Some(message) = auto_pull_version_drift_message(
                    component,
                    version_before_pull.as_deref(),
                    version::get_component_version(component).as_deref(),
                ) {
                    log_status!("deploy", "{}", message);
                }
                log_status!("deploy", "'{}' is now up to date", component.id);
            }
            Ok(None) => {
                // Not behind or no upstream — nothing to do
            }
            Err(_) => {
                // git fetch failed — warn but don't block (might be offline)
                log_status!(
                    "deploy",
                    "Warning: could not check remote status for '{}' — deploying local state",
                    component.id
                );
            }
        }
    }
    Ok(())
}

fn auto_pull_version_drift_message(
    component: &Component,
    before: Option<&str>,
    after: Option<&str>,
) -> Option<String> {
    if before == after {
        return None;
    }

    Some(format!(
        "Warning: auto-pull changed '{}' deploy version from {} to {}",
        component.id,
        before.unwrap_or("unknown"),
        after.unwrap_or("unknown")
    ))
}

/// Check for unreleased commits ahead of the latest tag.
///
/// Checks each component for commits between the latest tag and HEAD.
/// When found and `--force` is not set, returns an error to prevent
/// silently deploying stale code. Use `deploy --head` to deploy
/// unreleased commits, or `homeboy release` to tag them first.
fn check_unreleased_commits(
    components: &[Component],
    config: &DeployConfig,
) -> crate::core::Result<()> {
    let mut gaps = Vec::new();

    for component in components {
        if let Some(gap) = super::provenance::detect_tag_gap(component) {
            super::provenance::warn_tag_gap(&component.id, &gap, "deploy");
            gaps.push((component.id.clone(), gap));
        }
    }

    if gaps.is_empty() {
        return Ok(());
    }

    if config.tagged {
        log_status!(
            "deploy",
            "Deploying from tagged releases (--tagged). Use `deploy --head` to include unreleased commits, or `homeboy release` to tag them."
        );
        return Ok(());
    }

    if config.force {
        log_status!(
            "deploy",
            "Deploying from tagged releases (--force). Use `deploy --head` to include unreleased commits, or `homeboy release` to tag them."
        );
        return Ok(());
    }

    let component_list: Vec<String> = gaps
        .iter()
        .map(|(id, gap)| format!("{} ({} commits ahead of {})", id, gap.ahead, gap.tag))
        .collect();

    Err(crate::core::Error::validation_invalid_argument(
        "deploy",
        format!(
            "Refusing to deploy: HEAD has unreleased commits for: {}",
            component_list.join(", ")
        ),
        None,
        Some(vec![
            "Run `homeboy release` to tag the commits first".to_string(),
            "Use `deploy --head` to deploy unreleased commits directly".to_string(),
            "Use `deploy --force` to deploy the stale tag anyway".to_string(),
        ]),
    ))
}

/// Verify that component versions match the expected version.
///
/// When `--version` is used, ensures the local version of each component
/// matches the asserted version. This catches cases where the local copy
/// has a different version than what was just released.
fn verify_expected_version(components: &[Component], expected: &str) -> Result<()> {
    let mut mismatches = Vec::new();

    for component in components {
        match version::get_component_version(component) {
            Some(local_version) if local_version == expected => {}
            Some(local_version) => mismatches.push(format!(
                "'{}': local version is {} (expected {})",
                component.id, local_version, expected
            )),
            None => mismatches.push(format!(
                "'{}': local version could not be read (expected {})",
                component.id, expected
            )),
        }
    }

    if !mismatches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "version",
            format!("Version mismatch: {}", mismatches.join("; ")),
            None,
            Some(vec![
                "Pull latest changes: git pull".to_string(),
                "Or remove --version to deploy the current local version".to_string(),
            ]),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::ComponentScriptsConfig;
    use crate::core::project::ProjectComponentAttachment;
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

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
            expected_version: None,
            no_pull: false,
            head: false,
            tagged: false,
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

            assert_eq!(err.code, crate::core::error::ErrorCode::ExtensionNotFound);
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
        component.version_targets = Some(vec![crate::core::component::VersionTarget {
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

    fn deployed_result(id: &str) -> ComponentDeployResult {
        let component = make_component(id, "/tmp/does-not-matter");
        ComponentDeployResult::new(&component, "/srv/site").with_status("deployed")
    }

    #[test]
    fn post_deploy_smoke_is_noop_without_config() {
        let project = Project {
            id: "site".to_string(),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), None);
        assert_eq!(results[0].status, "deployed");
    }

    #[test]
    fn post_deploy_smoke_noop_when_disabled() {
        let project = Project {
            id: "site".to_string(),
            smoke_check: Some(crate::core::project::SmokeCheckConfig {
                enabled: false,
                url: "https://example.test/".to_string(),
                ..Default::default()
            }),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), None);
    }

    #[test]
    fn post_deploy_smoke_failure_records_error_and_fails() {
        // enabled smoke against an unreachable URL fails the deploy and records
        // the error on the deployed component.
        let project = Project {
            id: "site".to_string(),
            smoke_check: Some(crate::core::project::SmokeCheckConfig {
                enabled: true,
                // Reserved TEST-NET address (RFC 5737) so the request fails fast.
                url: "http://192.0.2.1:9/".to_string(),
                timeout_secs: 1,
                ..Default::default()
            }),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), Some(true));
        assert!(
            results[0].error.is_some(),
            "failed smoke must record an error on the deployed component"
        );
    }

    #[test]
    fn post_deploy_smoke_warn_only_does_not_fail() {
        let project = Project {
            id: "site".to_string(),
            smoke_check: Some(crate::core::project::SmokeCheckConfig {
                enabled: true,
                url: "http://192.0.2.1:9/".to_string(),
                timeout_secs: 1,
                warn_only: true,
                ..Default::default()
            }),
            ..Project::default()
        };
        let mut results = vec![deployed_result("plugin")];

        assert_eq!(run_post_deploy_smoke(&project, &mut results), Some(false));
        assert_eq!(
            results[0].status, "deployed",
            "warn_only smoke must not fail the deploy"
        );
        assert!(
            results[0].warnings.iter().any(|w| w.contains("warn_only")),
            "warn_only smoke failure should be surfaced as a warning"
        );
    }

    #[test]
    fn deploy_preflight_cleans_homeboy_build_dir_after_failed_build() {
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

            let (build_exit_code, build_error) = crate::core::build::build_component(&component);
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
            ) {
                Ok(_) => panic!("failed build should abort preflight"),
                Err(failures) => failures,
            };

            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].build_exit_code, Some(42));
            assert!(
                !dir.path().join(".homeboy-build").exists(),
                "deploy-context failed builds must clean Homeboy-generated build artifacts"
            );
        });
    }
}
