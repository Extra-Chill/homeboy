use std::path::Path;
use std::time::{Duration, Instant};

use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

#[derive(Debug, serde::Serialize)]
pub(super) struct ReleaseSourceFreshness {
    pub(super) local_head: String,
    upstream_ref: String,
    upstream_head: String,
    ahead: u32,
    behind: u32,
    fetched_at: String,
}

impl ReleaseSourceFreshness {
    pub(super) fn fetch_default_checkout(component: &Component) -> Result<Option<Self>> {
        let path = Path::new(&component.local_path);
        let remote = git::resolve_default_remote(path);
        if !git::run_git(path, &["remote"], "list release source remotes")?
            .lines()
            .any(|name| name == remote)
        {
            return Ok(None);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        let branch = if let Some(branch) = git::default_branch_name(path) {
            branch
        } else {
            // A checkout with a restricted fetch refspec may not know the
            // repository's default branch yet.
            let head = git::run_git_with_env_timeout(
                path,
                &["ls-remote", "--symref", &remote, "HEAD"],
                "resolve release remote default branch",
                &[],
                deadline.saturating_duration_since(Instant::now()),
            )?;
            head.lines()
                .find_map(|line| {
                    line.strip_prefix("ref: refs/heads/")?
                        .strip_suffix("\tHEAD")
                })
                .unwrap_or("main")
                .to_string()
        };
        if git::current_branch(path).as_deref() != Some(branch.as_str()) {
            return Ok(None);
        }

        // Fetch the exact branch even if the checkout has no configured tracking
        // branch or the remote's default refspec doesn't include it.
        let upstream_ref = format!("{remote}/{branch}");
        let tracking_ref = format!("refs/remotes/{upstream_ref}");
        let refspec = format!("+refs/heads/{branch}:{tracking_ref}");
        git::fetch_remote_tracking_refs_until(
            path,
            &["fetch", &remote, &refspec],
            &format!("fetch release source {upstream_ref}"),
            &[],
            deadline,
        )
        .map_err(|error| {
            error.with_hint(format!(
                "Could not verify release source against {upstream_ref}. Restore access to the remote and rerun the release preview."
            ))
        })?;
        let fetched_at = chrono::Utc::now().to_rfc3339();
        let local_head = git::get_head_commit(&component.local_path)?;
        let upstream_head = git::run_git(
            path,
            &[
                "rev-parse",
                "--verify",
                &format!("{tracking_ref}^{{commit}}"),
            ],
            "resolve fetched release source",
        )?
        .trim()
        .to_string();
        let range = format!("{local_head}...{upstream_head}");
        let counts = git::run_git(
            path,
            &["rev-list", "--left-right", "--count", &range],
            "compare release source with upstream",
        )?;
        let mut counts = counts.split_whitespace().map(str::parse::<u32>);
        let (Some(Ok(ahead)), Some(Ok(behind))) = (counts.next(), counts.next()) else {
            return Err(Error::git_command_failed(
                "invalid release source ahead/behind counts",
            ));
        };
        Ok(Some(Self {
            local_head,
            upstream_ref,
            upstream_head,
            ahead,
            behind,
            fetched_at,
        }))
    }

    pub(super) fn summary(&self) -> String {
        format!(
            "Local HEAD {} vs. remote default {} at {}: {} ahead, {} behind (fetched at {}).",
            self.local_head,
            self.upstream_ref,
            self.upstream_head,
            self.ahead,
            self.behind,
            self.fetched_at
        )
    }

    pub(super) fn require_current(&self, component_id: &str) -> Result<()> {
        if self.behind == 0 {
            return Ok(());
        }
        let mut error = Error::validation_invalid_argument(
            "release-stale-checkout",
            format!("Release checkout is stale. {}", self.summary()),
            None,
            None,
        )
        .with_hint(format!(
            "Refresh through your workspace provider if its policy allows it (for DMC: `wp datamachine-code workspace git pull {component_id} --allow-primary-refresh`). On an unmanaged checkout, use `git merge --ff-only {}`. Resolve any divergence first, then rerun `homeboy release {component_id} --dry-run`.",
            self.upstream_ref
        ));
        error.details["source_freshness"] = serde_json::json!(self);
        Err(error)
    }

    pub(super) fn revalidate(&self, component: &Component) -> Result<()> {
        require_head(component, &self.local_head)?;
        let current = Self::fetch_default_checkout(component)?;
        if let Some(current) = &current {
            if self.local_head == current.local_head
                && self.upstream_head == current.upstream_head
                && self.upstream_ref == current.upstream_ref
            {
                return Ok(());
            }
        }
        Err(source_drift(&format!(
            "{} Now: {}",
            self.summary(),
            current
                .as_ref()
                .map(Self::summary)
                .as_deref()
                .unwrap_or("not on the selected default branch")
        )))
    }
}

pub(super) fn require_head(component: &Component, expected: &str) -> Result<()> {
    let actual = git::get_head_commit(&component.local_path)?;
    if actual == expected {
        return Ok(());
    }
    Err(source_drift(&format!(
        "expected HEAD {expected}, found {actual}"
    )))
}

fn source_drift(message: &str) -> Error {
    Error::invalid_argument(
        "release.source",
        format!("Release source drift: {message}. Replan with `homeboy release --dry-run`."),
    )
}

pub(super) fn record_source(
    plan: &mut super::types::ReleasePlan,
    sha: Option<&str>,
    freshness: Option<&ReleaseSourceFreshness>,
) {
    if let Some(sha) = sha {
        plan.plan
            .policy
            .insert("source_sha".into(), serde_json::json!(sha));
    }
    if let Some(freshness) = freshness {
        plan.plan
            .policy
            .insert("source_freshness".into(), serde_json::json!(freshness));
    }
}

/// Check default-branch freshness before a caller dispatches portable gates.
/// The release command will recheck after those gates and bind their evidence.
pub fn check_portable_release_source(
    component: &Component,
    commit: &str,
    check_default: bool,
) -> Result<()> {
    if check_default {
        if let Some(source) = ReleaseSourceFreshness::fetch_default_checkout(component)? {
            homeboy_core::log_status!("release", "{}", source.summary());
            source.require_current(&component.id)?;
        }
    }
    require_head(component, commit)
}
