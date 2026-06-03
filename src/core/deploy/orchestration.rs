use std::collections::HashMap;

use crate::core::component::Component;
use crate::core::context::RemoteProjectContext;
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::project::Project;
use crate::core::release::version;

use super::execution::{
    execute_preflighted_component_deploy, prepare_component_deploy, PreparedComponentDeploy,
};
use super::generated_artifacts::unexpected_uncommitted_files_excluding_homeboy_build;
use super::path_roots::{project_with_detected_path_roots, resolve_effective_remote_path};
use super::planning::{
    calculate_component_status, calculate_release_state, load_project_components, plan_components,
};
use super::types::{
    ComponentDeployResult, ComponentStatus, DeployConfig, DeployOrchestrationResult, DeploySummary,
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
    let loaded = load_project_components(project, &config.component_ids)?;
    validate_supported_build_configs(&loaded.deployable)?;
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
                "Ensure components have a buildArtifact, an extension with artifact_pattern, or deploy_strategy: \"git\"".to_string(),
                format!("Check with: homeboy component show <id>"),
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
        ));
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

    // Checkout latest tag for each component (unless --head or --skip-build).
    let tag_checkouts = if !config.head && !config.skip_build {
        checkout_latest_tags(&components)?
    } else {
        Vec::new()
    };

    // Verify expected version if --version was specified
    if let Some(ref expected) = config.expected_version {
        verify_expected_version(&components, expected)?;
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
            result = result.with_deployed_ref(checkout.tag.clone());
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
    project: &Project,
    base_path: &str,
    config: &DeployConfig,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = calculate_component_status(c, remote_versions);
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

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        },
    }
}

/// Dry-run mode: return planned results without building or deploying.
fn run_dry_run_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    project: &Project,
    base_path: &str,
    config: &DeployConfig,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = if config.check {
                calculate_component_status(c, remote_versions)
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
            if config.check {
                result = result.with_component_status(status);
            }
            result
        })
        .collect();

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        },
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
    let mut dirty: Vec<&str> = Vec::new();

    for component in components {
        if component.is_file_component() {
            continue;
        }
        if !git::is_git_repo(&component.local_path) {
            non_git.push(component);
            continue;
        }
        match unexpected_uncommitted_files_excluding_homeboy_build(&component.local_path) {
            Ok(unexpected) if unexpected.is_empty() => {}
            Ok(_) | Err(_) => dirty.push(component.id.as_str()),
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
        return Err(Error::validation_invalid_argument(
            "components",
            format!("Components have uncommitted changes: {}", dirty.join(", ")),
            None,
            Some(vec![
                "Commit your changes before deploying to ensure deployed code is tracked"
                    .to_string(),
                "Use --force to deploy anyway".to_string(),
            ]),
        ));
    }
    Ok(())
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

/// Record of a tag checkout for later branch restoration.
struct TagCheckout {
    component_id: String,
    tag: String,
    original_ref: String,
    local_path: String,
}

/// Checkout the latest version tag for each component before building.
///
/// For each component, finds the latest semver tag, saves the current
/// branch/ref, and checks out the tag. Returns a list of checkouts
/// so branches can be restored after deployment.
///
/// Components without tags are skipped with a warning — they deploy
/// from HEAD as before (the pre-tag-checkout behavior).
fn checkout_latest_tags(components: &[Component]) -> Result<Vec<TagCheckout>> {
    let mut checkouts = Vec::new();

    for component in components {
        // File components don't have tags — skip
        if component.is_file_component() {
            continue;
        }

        let path = &component.local_path;

        // Get the latest tag
        let tag = match git::get_latest_tag(path) {
            Ok(Some(t)) => t,
            Ok(None) => {
                log_status!(
                    "deploy",
                    "Warning: '{}' has no version tags — deploying from HEAD (use --head to suppress this warning)",
                    component.id
                );
                continue;
            }
            Err(_) => {
                log_status!(
                    "deploy",
                    "Warning: could not read tags for '{}' — deploying from HEAD",
                    component.id
                );
                continue;
            }
        };

        // Save the current branch name. Use symbolic-ref which returns the
        // actual branch name and fails cleanly on detached HEAD (unlike
        // --abbrev-ref which returns the literal "HEAD" string). If HEAD is
        // already detached, save the commit hash so we can at least restore
        // to the same commit afterward.
        let original_ref = crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["symbolic-ref", "--short", "HEAD"],
        )
        .or_else(|| {
            // Detached HEAD — save the commit hash as fallback
            crate::core::engine::command::run_in_optional(path, "git", &["rev-parse", "HEAD"])
        })
        .unwrap_or_else(|| "main".to_string());

        // If already on this tag's commit, skip checkout
        let tag_commit =
            crate::core::engine::command::run_in_optional(path, "git", &["rev-parse", &tag]);
        let head_commit =
            crate::core::engine::command::run_in_optional(path, "git", &["rev-parse", "HEAD"]);
        if tag_commit.is_some() && tag_commit == head_commit {
            log_status!(
                "deploy",
                "'{}' is already at tag {} — no checkout needed",
                component.id,
                tag
            );
            checkouts.push(TagCheckout {
                component_id: component.id.clone(),
                tag: tag.clone(),
                original_ref,
                local_path: path.clone(),
            });
            continue;
        }

        // Checkout the tag
        log_status!(
            "deploy",
            "'{}' checking out tag {} for deploy...",
            component.id,
            tag
        );
        match crate::core::engine::command::run_in(
            path,
            "git",
            &["checkout", &tag],
            "git checkout tag",
        ) {
            Ok(_) => {
                checkouts.push(TagCheckout {
                    component_id: component.id.clone(),
                    tag: tag.clone(),
                    original_ref,
                    local_path: path.clone(),
                });
            }
            Err(e) => {
                return Err(Error::git_command_failed(format!(
                    "Failed to checkout tag {} for '{}': {}",
                    tag, component.id, e
                )));
            }
        }
    }

    Ok(checkouts)
}

/// Restore original branches after deployment.
///
/// Best-effort: logs warnings on failure but does not abort.
/// The deployment already completed — failing to restore a branch
/// is inconvenient but not destructive.
fn restore_branches(checkouts: &[TagCheckout]) {
    for checkout in checkouts {
        let restore = crate::core::engine::command::run_in(
            &checkout.local_path,
            "git",
            &["checkout", &checkout.original_ref],
            "git checkout restore",
        );
        match restore {
            Ok(_) => {
                log_status!(
                    "deploy",
                    "'{}' restored to {}",
                    checkout.component_id,
                    checkout.original_ref
                );
            }
            Err(e) => {
                log_status!(
                    "deploy",
                    "Warning: could not restore '{}' to {}: {}",
                    checkout.component_id,
                    checkout.original_ref,
                    e
                );
            }
        }
    }
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

    #[test]
    fn deploy_validation_rejects_legacy_build_command_before_artifact_checks() {
        let dir = TempDir::new().expect("temp dir");
        let mut component = make_component("sample-codebox", &dir.path().to_string_lossy());
        component.build_artifact =
            Some("packages/browser-extension/dist/sample-codebox.zip".to_string());
        component.build_command = Some("npm run package:browser-extension".to_string());

        let err = validate_supported_build_configs(&[component])
            .expect_err("legacy build_command should fail deploy preflight");

        assert!(err.message.contains("unsupported legacy build_command"));
        assert!(err.message.contains("Use scripts.build instead"));
        assert_eq!(err.details["field"].as_str(), Some("build_command"));
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
        let loaded = load_project_components(&project, &["selected".to_string()])
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
        let loaded = load_project_components(&project, &["selected".to_string()])
            .expect("targeted component load should include selected component");

        let err = validate_supported_build_configs(&loaded.deployable)
            .expect_err("selected legacy build_command should still fail targeted deploy");

        assert!(err.message.contains("unsupported legacy build_command"));
        assert_eq!(err.details["field"].as_str(), Some("build_command"));
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

        let component = make_component("test", &path.to_string_lossy());
        check_uncommitted_changes(&[component])
            .expect("generated Homeboy deploy artifacts should not block deploy");
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
    fn expected_version_rejects_stale_component_worktree() {
        let dir = TempDir::new().expect("temp dir");
        let package_json = dir.path().join("package.json");
        std::fs::write(&package_json, r#"{"version":"1.0.0"}"#).expect("write package.json");

        let mut component = make_component("demo", &dir.path().to_string_lossy());
        component.version_targets = Some(vec![crate::core::component::VersionTarget {
            file: "package.json".to_string(),
            pattern: Some(r#""version"\s*:\s*"([^"]+)""#.to_string()),
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

    #[test]
    fn deploy_preflight_cleans_homeboy_build_dir_after_failed_build() {
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
    }
}
