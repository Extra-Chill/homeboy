use std::collections::HashMap;
use std::path::Path;

use crate::component::Component;
use crate::context::RemoteProjectContext;
use crate::error::{Error, Result};
use crate::git;
use crate::project::Project;
use crate::version;

use super::execution::execute_component_deploy;
use super::planning::{
    calculate_component_status, calculate_release_state, load_project_components, plan_components,
};
use super::types::{
    ComponentDeployResult, ComponentStatus, DeployConfig, DeployOrchestrationResult, DeploySummary,
};
use super::version_overrides::fetch_remote_versions;

/// Main deploy orchestration entry point.
/// Handles component selection, building, and deployment.
pub(super) fn deploy_components(
    config: &DeployConfig,
    project: &Project,
    ctx: &RemoteProjectContext,
    base_path: &str,
) -> Result<DeployOrchestrationResult> {
    let loaded = load_project_components(project, &config.component_ids)?;
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

    let components = plan_components(
        config,
        &loaded.deployable,
        &loaded.skipped,
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

    // Gather versions
    let local_versions: HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    let remote_versions = if config.outdated || config.dry_run || config.check {
        fetch_remote_versions(&components, base_path, &ctx.client)
    } else {
        HashMap::new()
    };

    // Check and dry-run modes return early without building or deploying
    if config.check {
        return Ok(run_check_mode(
            &components,
            &local_versions,
            &remote_versions,
            base_path,
        ));
    }
    if config.dry_run {
        return Ok(run_dry_run_mode(
            &components,
            &local_versions,
            &remote_versions,
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

    // Execute deployments
    let mut results: Vec<ComponentDeployResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for component in &components {
        // Apply per-project overrides (e.g. different extract_command or remote_owner)
        let component = crate::project::apply_component_overrides(component, project);

        let effective_config = clone_config(config);

        let mut result = execute_component_deploy(
            &component,
            &effective_config,
            ctx,
            base_path,
            project,
            local_versions.get(&component.id).cloned(),
            remote_versions.get(&component.id).cloned(),
        );

        // Record which git ref was deployed
        if let Some(checkout) = tag_checkouts
            .iter()
            .find(|c| c.component_id == component.id)
        {
            result = result.with_deployed_ref(checkout.tag.clone());
        } else if config.head {
            // Deploying from HEAD — record the current branch
            if let Some(branch) = crate::engine::command::run_in_optional(
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

/// Check mode: return component status without building or deploying.
fn run_check_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    base_path: &str,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = calculate_component_status(c, remote_versions);
            let release_state = calculate_release_state(c);
            let mut result = ComponentDeployResult::new(c, base_path)
                .with_status("checked")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                )
                .with_component_status(status);
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
            let mut result = ComponentDeployResult::new(c, base_path)
                .with_status("planned")
                .with_versions(
                    local_versions.get(&c.id).cloned(),
                    remote_versions.get(&c.id).cloned(),
                );
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
        let current_branch = match crate::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
        ) {
            Some(branch) if branch != "HEAD" => branch, // "HEAD" means detached
            _ => continue,                              // detached or error — skip
        };

        // Detect default branch from remote HEAD symref, fallback to "main"
        let default_branch = crate::engine::command::run_in_optional(
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
    let dirty: Vec<&str> = components
        .iter()
        .filter(|c| !c.is_file_component())
        .filter(|c| !git::is_workdir_clean(Path::new(&c.local_path)))
        .map(|c| c.id.as_str())
        .collect();

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
        let original_ref = crate::engine::command::run_in_optional(
            path,
            "git",
            &["symbolic-ref", "--short", "HEAD"],
        )
        .or_else(|| {
            // Detached HEAD — save the commit hash as fallback
            crate::engine::command::run_in_optional(path, "git", &["rev-parse", "HEAD"])
        })
        .unwrap_or_else(|| "main".to_string());

        // If already on this tag's commit, skip checkout
        let tag_commit = crate::engine::command::run_in_optional(path, "git", &["rev-parse", &tag]);
        let head_commit =
            crate::engine::command::run_in_optional(path, "git", &["rev-parse", "HEAD"]);
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
        match crate::engine::command::run_in(path, "git", &["checkout", &tag], "git checkout tag") {
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
        let restore = crate::engine::command::run_in(
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
fn check_unreleased_commits(components: &[Component], config: &DeployConfig) -> crate::Result<()> {
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

    Err(crate::Error::validation_invalid_argument(
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

/// Create a value copy of DeployConfig for per-component overrides.
fn clone_config(config: &DeployConfig) -> DeployConfig {
    DeployConfig {
        component_ids: config.component_ids.clone(),
        all: config.all,
        outdated: config.outdated,
        dry_run: config.dry_run,
        check: config.check,
        force: config.force,
        skip_build: config.skip_build,
        keep_deps: config.keep_deps,
        expected_version: config.expected_version.clone(),
        no_pull: config.no_pull,
        head: config.head,
        tagged: config.tagged,
    }
}

/// Verify that component versions match the expected version.
///
/// When `--version` is used, ensures the local version of each component
/// matches the asserted version. This catches cases where the local copy
/// has a different version than what was just released.
fn verify_expected_version(components: &[Component], expected: &str) -> Result<()> {
    let mut mismatches = Vec::new();

    for component in components {
        if let Some(local_version) = version::get_component_version(component) {
            if local_version != expected {
                mismatches.push(format!(
                    "'{}': local version is {} (expected {})",
                    component.id, local_version, expected
                ));
            }
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
