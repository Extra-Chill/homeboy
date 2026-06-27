use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::release::version;

use super::super::generated_artifacts::uncommitted_file_report_excluding_known_generated;
use super::super::types::DeployConfig;

/// Warn when `--head` would deploy from a non-default branch.
///
/// Detects the current branch for each component and compares it against the
/// default branch (via [`git::default_branch_name`], falling back to "main").
/// If a component is on a feature branch, this is likely
/// unintentional — the user probably meant to deploy the default branch.
///
/// With `--force`, this emits a log warning but proceeds. Without `--force`,
/// it returns an error so the user can switch branches or confirm intent.
pub(super) fn warn_non_default_branch(
    components: &[Component],
    config: &DeployConfig,
) -> Result<()> {
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

        // Detect default branch from the resolved remote HEAD symref, fallback to "main"
        let default_branch = git::default_branch_name(std::path::Path::new(path))
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

pub(super) fn check_uncommitted_changes(components: &[Component]) -> Result<()> {
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
pub(super) fn sync_components(components: &[Component]) -> Result<()> {
    for component in components {
        // File components are not git repos — skip sync
        if component.is_file_component() {
            continue;
        }

        let version_before_pull = version::get_component_version(component);

        // Check if behind remote
        match git::fetch_and_get_behind_count(&component.local_path) {
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

pub(super) fn auto_pull_version_drift_message(
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
pub(super) fn check_unreleased_commits(
    components: &[Component],
    config: &DeployConfig,
) -> crate::core::Result<()> {
    let mut gaps = Vec::new();

    for component in components {
        if let Some(gap) = super::super::provenance::detect_tag_gap(component) {
            super::super::provenance::warn_tag_gap(&component.id, &gap, "deploy");
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
pub(super) fn verify_expected_version(components: &[Component], expected: &str) -> Result<()> {
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
