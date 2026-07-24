//! Tag checkout and branch restoration for tagged deploys.
//!
//! Split out of `orchestration.rs` to keep the main deploy flow focused on
//! component selection and execution. These helpers check out the release tag
//! for each component before building and restore the original branch afterward.

use crate::release;
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};

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

/// A deploy tag resolved against the pristine, pre-checkout worktree.
///
/// Resolution is deliberately separated from checkout. `latest_component_tag`
/// resolves through `git tag --merged HEAD`, so resolving lazily inside the
/// checkout loop let an earlier component's detached checkout rewind HEAD and
/// silently downgrade every later component to its *previous* release tag
/// (#9963).
pub(super) struct ResolvedDeployTag {
    pub(super) component: Component,
    pub(super) tag: String,
    pub(super) tag_sha: Option<String>,
    /// Full commit sha the tag peels to. Annotated tags resolve to a tag object,
    /// so this is always peeled with `^{commit}` to name the deployed content.
    pub(super) tag_commit_sha: Option<String>,
    head_commit: Option<String>,
    pub(super) head_ahead: u32,
    original_ref: String,
    /// Git root that owns this component, used to detect components that would
    /// otherwise contend for a single shared worktree.
    pub(super) git_root: String,
}

impl ResolvedDeployTag {
    /// Whether the pristine checkout already contains exactly this tag.
    fn already_at_tag(&self) -> bool {
        self.tag_commit_sha.is_some() && self.tag_commit_sha == self.head_commit
    }

    /// Human-readable provenance label for this resolved tag.
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

    fn into_checkout(self) -> TagCheckout {
        TagCheckout {
            component_id: self.component.id,
            tag: self.tag,
            original_ref: self.original_ref,
            local_path: self.component.local_path,
            tag_sha: self.tag_sha,
            head_ahead: self.head_ahead,
        }
    }
}

/// Resolve the deploy tag for every component against the pristine checkout.
///
/// This performs no mutation: every tag, sha, and HEAD-gap measurement is taken
/// before any component checks anything out, so resolution cannot be perturbed
/// by a sibling component's checkout.
///
/// Components without tags fail closed. Deploying the current checkout must be
/// requested explicitly with `--head`.
pub(super) fn resolve_deploy_tags(
    components: &[Component],
    expected_version: Option<&str>,
) -> Result<Vec<ResolvedDeployTag>> {
    let mut resolved = Vec::new();

    for component in components {
        // File components don't have tags — skip
        if component.is_file_component() {
            continue;
        }

        let path = &component.local_path;

        let tag = match expected_version {
            Some(version) => deploy_tag_for_version(component, version),
            None => match release::latest_component_tag(component) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    return Err(Error::validation_invalid_argument(
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
                    ));
                }
                Err(err) => {
                    return Err(Error::git_command_failed(format!(
                        "Could not read version tags for '{}': {}",
                        component.id, err
                    )));
                }
            },
        };

        // Save the current branch name. Use symbolic-ref which returns the
        // actual branch name and fails cleanly on detached HEAD (unlike
        // --abbrev-ref which returns the literal "HEAD" string). If HEAD is
        // already detached, save the commit hash so we can at least restore
        // to the same commit afterward.
        let original_ref = homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["symbolic-ref", "--short", "HEAD"],
        )
        .or_else(|| {
            // Detached HEAD — save the commit hash as fallback
            homeboy_core::engine::command::run_in_optional(path, "git", &["rev-parse", "HEAD"])
        })
        .unwrap_or_else(|| "main".to_string());

        // Peel to the commit so annotated tags (which rev-parse to a tag object)
        // compare correctly against HEAD and name real deployable content.
        let tag_commit_sha = homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--verify", &format!("{tag}^{{commit}}")],
        )
        .map(|sha| sha.trim().to_string());
        let head_commit =
            homeboy_core::engine::command::run_in_optional(path, "git", &["rev-parse", "HEAD"])
                .map(|sha| sha.trim().to_string());

        // Short sha of the tag being deployed, for unambiguous provenance.
        let tag_sha = homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--short", &tag],
        );

        // How many commits the (pre-checkout) HEAD was ahead of this tag.
        // Non-zero means a stale tag is being deployed and those HEAD-only
        // commits are NOT shipped — recorded so provenance can say so.
        let head_ahead = homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-list", "--count", &format!("{}..HEAD", tag)],
        )
        .and_then(|out| out.trim().parse::<u32>().ok())
        .unwrap_or(0);

        let git_root = homeboy_core::git::get_git_root(path).unwrap_or_else(|_| path.clone());

        resolved.push(ResolvedDeployTag {
            component: component.clone(),
            tag,
            tag_sha,
            tag_commit_sha,
            head_commit,
            head_ahead,
            original_ref,
            git_root,
        });
    }

    Ok(resolved)
}

/// Partition resolved tags into those safe to check out in place and those that
/// must be materialized in isolation.
///
/// A single worktree can only hold one commit, so when two components share a
/// git root and resolve to different commits, checking both out in place makes
/// the last checkout win for every component (#9963). Those components are
/// routed to per-component detached materialization instead.
pub(super) fn partition_shared_root_conflicts(
    resolved: Vec<ResolvedDeployTag>,
) -> (Vec<ResolvedDeployTag>, Vec<ResolvedDeployTag>) {
    let mut commits_by_root: std::collections::HashMap<&str, std::collections::BTreeSet<&str>> =
        std::collections::HashMap::new();
    for entry in &resolved {
        if let Some(sha) = entry.tag_commit_sha.as_deref() {
            commits_by_root
                .entry(entry.git_root.as_str())
                .or_default()
                .insert(sha);
        }
    }

    let conflicted: std::collections::HashSet<String> = commits_by_root
        .iter()
        .filter(|(_, commits)| commits.len() > 1)
        .map(|(root, _)| (*root).to_string())
        .collect();

    resolved
        .into_iter()
        .partition(|entry| !conflicted.contains(&entry.git_root))
}

/// Check out already-resolved deploy tags in place.
///
/// Callers must only pass components that exclusively own their git root — a
/// single worktree cannot simultaneously hold two different tags, so sharing one
/// silently packages every component from whichever tag was checked out last.
/// Use [`resolve_deploy_tags`] plus per-component materialization for shared
/// roots.
pub(super) fn checkout_resolved_deploy_tags(
    resolved: Vec<ResolvedDeployTag>,
) -> Result<Vec<TagCheckout>> {
    let mut checkouts: Vec<TagCheckout> = Vec::new();

    for entry in resolved {
        let path = entry.component.local_path.clone();
        let component_id = entry.component.id.clone();
        let tag = entry.tag.clone();

        if entry.already_at_tag() {
            homeboy_core::log_status!(
                "deploy",
                "'{}' is already at tag {} — no checkout needed",
                component_id,
                tag
            );
            checkouts.push(entry.into_checkout());
            continue;
        }

        homeboy_core::log_status!(
            "deploy",
            "'{}' checking out tag {} for deploy...",
            component_id,
            tag
        );
        match homeboy_core::engine::command::run_in(
            &path,
            "git",
            &["checkout", &tag],
            "git checkout tag",
        ) {
            Ok(_) => checkouts.push(entry.into_checkout()),
            Err(e) => {
                if !checkouts.is_empty() {
                    restore_branches(&checkouts);
                }
                return Err(Error::git_command_failed(format!(
                    "Failed to checkout tag {} for '{}': {}",
                    tag, component_id, e
                )));
            }
        }
    }

    Ok(checkouts)
}

/// Resolve and check out the deploy tag for each component before building.
///
/// Retained for single-root callers and tests; the orchestrator resolves and
/// materializes separately so monorepo components never share one worktree.
#[cfg(test)]
pub(super) fn checkout_deploy_tags(
    components: &[Component],
    expected_version: Option<&str>,
) -> Result<Vec<TagCheckout>> {
    checkout_resolved_deploy_tags(resolve_deploy_tags(components, expected_version)?)
}

pub(super) fn deploy_tag_for_version(component: &Component, version: &str) -> String {
    release::component_tag_name(component, version).unwrap_or_else(|_| {
        let version = version.trim_start_matches('v');
        format!("v{}", version)
    })
}

/// Restore original branches after deployment.
///
/// Best-effort: logs warnings on failure but does not abort.
/// The deployment already completed — failing to restore a branch
/// is inconvenient but not destructive.
pub(super) fn restore_branches(checkouts: &[TagCheckout]) {
    for checkout in checkouts {
        let restore = homeboy_core::engine::command::run_in(
            &checkout.local_path,
            "git",
            &["checkout", &checkout.original_ref],
            "git checkout restore",
        );
        match restore {
            Ok(_) => {
                homeboy_core::log_status!(
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
                homeboy_core::log_status!(
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
    homeboy_core::engine::command::run_in_optional(
        path,
        "git",
        &["symbolic-ref", "--short", "HEAD"],
    )
    .or_else(|| {
        homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--short", "HEAD"],
        )
    })
    .unwrap_or_else(|| "unknown".to_string())
}

fn dirty_checkout_files(path: &str) -> Vec<String> {
    homeboy_core::engine::command::run_in_optional(path, "git", &["status", "--porcelain"])
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
