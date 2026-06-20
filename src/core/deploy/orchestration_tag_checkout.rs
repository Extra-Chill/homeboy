//! Tag checkout and branch restoration for tagged deploys.
//!
//! Split out of `orchestration.rs` to keep the main deploy flow focused on
//! component selection and execution. These helpers check out the release tag
//! for each component before building and restore the original branch afterward.

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;

/// Record of a tag checkout for later branch restoration.
pub(super) struct TagCheckout {
    pub(super) component_id: String,
    pub(super) tag: String,
    pub(super) original_ref: String,
    pub(super) local_path: String,
    /// Resolved commit sha of the deployed tag (short form), if known.
    pub(super) tag_sha: Option<String>,
    /// Number of commits the original HEAD was ahead of this tag, if any.
    /// Non-zero means a stale tag was deployed (e.g. via `--force`) and these
    /// HEAD-only commits were NOT shipped — recorded so provenance can say so.
    pub(super) head_ahead: u32,
}

impl TagCheckout {
    /// Build the human-readable provenance ref for this deployed tag.
    ///
    /// Always resolves to the exact tag and (when known) its commit sha so the
    /// reported ref is unambiguous. When the original HEAD was ahead of the
    /// deployed tag, the annotation makes explicit that those HEAD-only commits
    /// were not deployed — preventing the misleading impression that HEAD
    /// content shipped (e.g. after a `--force` deploy of a stale tag).
    pub(super) fn provenance_ref(&self) -> String {
        let mut label = match &self.tag_sha {
            Some(sha) => format!("{} ({})", self.tag, sha),
            None => self.tag.clone(),
        };
        if self.head_ahead > 0 {
            label.push_str(&format!(
                " [stale tag: HEAD was {} commit(s) ahead, not deployed]",
                self.head_ahead
            ));
        }
        label
    }
}

/// Checkout the latest version tag for each component before building.
///
/// For each component, finds the latest semver tag, saves the current
/// branch/ref, and checks out the tag. Returns a list of checkouts
/// so branches can be restored after deployment.
///
/// Components without tags are skipped with a warning — they deploy
/// from HEAD as before (the pre-tag-checkout behavior).
pub(super) fn checkout_deploy_tags(
    components: &[Component],
    expected_version: Option<&str>,
) -> Result<Vec<TagCheckout>> {
    let mut checkouts = Vec::new();

    for component in components {
        // File components don't have tags — skip
        if component.is_file_component() {
            continue;
        }

        let path = &component.local_path;

        let tag = match expected_version {
            Some(version) => deploy_tag_for_version(component, version),
            None => match git::get_latest_tag(path) {
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
            },
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

        // Short sha of the tag being deployed, for unambiguous provenance.
        let tag_sha = crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--short", &tag],
        );

        // How many commits the (pre-checkout) HEAD was ahead of this tag.
        // Non-zero means a stale tag is being deployed and those HEAD-only
        // commits are NOT shipped — recorded so provenance can say so.
        let head_ahead = crate::core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-list", "--count", &format!("{}..HEAD", tag)],
        )
        .and_then(|out| out.trim().parse::<u32>().ok())
        .unwrap_or(0);

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
                tag_sha,
                head_ahead,
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
                    tag_sha,
                    head_ahead,
                });
            }
            Err(e) => {
                if !checkouts.is_empty() {
                    restore_branches(&checkouts);
                }
                return Err(Error::git_command_failed(format!(
                    "Failed to checkout tag {} for '{}': {}",
                    tag, component.id, e
                )));
            }
        }
    }

    Ok(checkouts)
}

pub(super) fn deploy_tag_for_version(component: &Component, version: &str) -> String {
    let version = version.trim_start_matches('v');
    match git::MonorepoContext::detect(&component.local_path, &component.id) {
        Some(context) => context.format_tag(version),
        None => format!("v{}", version),
    }
}

/// Restore original branches after deployment.
///
/// Best-effort: logs warnings on failure but does not abort.
/// The deployment already completed — failing to restore a branch
/// is inconvenient but not destructive.
pub(super) fn restore_branches(checkouts: &[TagCheckout]) {
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
                let current_ref = current_checkout_ref(&checkout.local_path);
                let dirty_files = dirty_checkout_files(&checkout.local_path);
                let dirty_summary = if dirty_files.is_empty() {
                    "none".to_string()
                } else {
                    dirty_files.join(", ")
                };
                let recovery_command = format!(
                    "git -C {:?} checkout {:?}",
                    checkout.local_path, checkout.original_ref
                );
                log_status!(
                    "deploy",
                    "Warning: could not restore '{}' after tagged deploy. starting_ref={}, current_ref={}, dirty_files=[{}], recovery_command=`{}`. Error: {}",
                    checkout.component_id,
                    checkout.original_ref,
                    current_ref,
                    dirty_summary,
                    recovery_command,
                    e
                );
            }
        }
    }
}

fn current_checkout_ref(path: &str) -> String {
    crate::core::engine::command::run_in_optional(path, "git", &["symbolic-ref", "--short", "HEAD"])
        .or_else(|| {
            crate::core::engine::command::run_in_optional(
                path,
                "git",
                &["rev-parse", "--short", "HEAD"],
            )
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn dirty_checkout_files(path: &str) -> Vec<String> {
    crate::core::engine::command::run_in_optional(path, "git", &["status", "--porcelain"])
        .map(|status| {
            status
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
