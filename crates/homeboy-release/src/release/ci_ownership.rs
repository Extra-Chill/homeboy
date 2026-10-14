//! Release ownership: defer `homeboy release` to a component whose CI owns its
//! releases (`release.owner: "ci"`, #15896).
//!
//! Components that release on merge run the reusable homeboy-action release
//! workflow, which computes the version and publishes it. A local release of
//! such a component either loses the race (source drift when CI's
//! `release: vX.Y.Z` commit lands) or wins it and double-publishes. Ownership
//! is an explicit, reviewable component declaration, never sniffed from
//! workflow files.
//!
//! The CI caller itself must keep releasing. It is recognized by the
//! invocation environment:
//!
//! - `HOMEBOY_RELEASE_CALLER=ci` marks the invocation as the CI release caller
//!   explicitly (any CI system). `HOMEBOY_RELEASE_CALLER=local` forces the
//!   local deferral even inside CI.
//! - Otherwise `GITHUB_ACTIONS=true`, which GitHub sets on every Actions job,
//!   including the homeboy-action reusable release workflow, marks the CI
//!   caller. homeboy-action therefore needs no change to keep releasing.
//!
//! The deferral reads only local repository state: no fetch, no planning, no
//! gates, no tag, no operation record.

use homeboy_core::component::Component;
use homeboy_core::error::Result;

use super::context::load_component;
use super::scope::ReleaseScope;
use super::types::{CiReleaseOwnership, ReleaseCommandInput, ReleaseCommandResult, ReleaseOptions};

/// Explicit caller marker. `ci` = the CI release caller; `local` = force the
/// local (deferring) behaviour.
pub const RELEASE_CALLER_ENV: &str = "HOMEBOY_RELEASE_CALLER";

/// Command status reported when a release is deferred to the CI owner.
pub const CI_OWNED_STATUS: &str = "ci_owned";

/// Skipped reason reported alongside [`CI_OWNED_STATUS`].
pub const CI_OWNED_REASON: &str = "release-owned-by-ci";

const CI_CALLER_MARKER: &str = "HOMEBOY_RELEASE_CALLER=ci, or GITHUB_ACTIONS=true";

/// Whether this process is the CI release caller, given an env lookup.
pub(crate) fn is_ci_release_caller_with(env: impl Fn(&str) -> Option<String>) -> bool {
    match env(RELEASE_CALLER_ENV)
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("ci") => return true,
        Some("local") => return false,
        _ => {}
    }
    env("GITHUB_ACTIONS").is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

fn is_ci_release_caller() -> bool {
    is_ci_release_caller_with(|key| std::env::var(key).ok())
}

/// Whether `input` must defer to CI. Recovery is an explicit operator repair of
/// existing release state and is never deferred.
fn should_defer(component: &Component, input: &ReleaseCommandInput, ci_caller: bool) -> bool {
    component.release.ci_owns_release() && !ci_caller && !input.recover
}

/// Resolve the component for `input` and, when its CI owns releases and this is
/// not the CI caller, return the deferred result. `None` means: release
/// normally. Callers run this before gates or planning.
///
/// A component that does not resolve is not CI-owned as far as this check can
/// tell: it returns `None` so the normal release path records readiness first
/// and then reports the resolution error through its own contract.
pub fn ci_owned_release_result(
    input: &ReleaseCommandInput,
) -> Result<Option<ReleaseCommandResult>> {
    if input.recover {
        return Ok(None);
    }
    let Ok(component) = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    ) else {
        return Ok(None);
    };
    Ok(ci_owned_release_result_for(
        &component,
        input,
        is_ci_release_caller(),
    ))
}

pub(crate) fn ci_owned_release_result_for(
    component: &Component,
    input: &ReleaseCommandInput,
    ci_caller: bool,
) -> Option<ReleaseCommandResult> {
    if !should_defer(component, input, ci_caller) {
        return None;
    }
    let ownership = ci_release_ownership(component, &input.component_id);
    let mut summary = vec![format!(
        "Releases for '{}' are owned by CI (release.owner: ci); no local release was planned, gated, tagged, or published.",
        input.component_id
    )];
    match (&ownership.latest_tag, ownership.head_released) {
        (Some(tag), true) => summary.push(format!("HEAD is already released as {tag}.")),
        (Some(tag), false) => summary.push(format!(
            "Latest release is {tag}; HEAD is not released yet. CI releases it on merge to the release branch."
        )),
        (None, _) => summary.push("No local release tag found yet.".to_string()),
    }
    summary.push(format!("Follow the CI release: {}", ownership.next_action));
    let new_version = super::version::read_component_version(component)
        .ok()
        .map(|info| info.version);

    Some(ReleaseCommandResult {
        component_id: input.component_id.clone(),
        status: CI_OWNED_STATUS.to_string(),
        phase: super::workflow::release_execution_plan(input).phase,
        bump_type: "none".to_string(),
        dry_run: input.dry_run,
        releasable_commits: 0,
        new_version,
        tag: ownership.latest_tag.clone(),
        skipped_reason: Some(CI_OWNED_REASON.to_string()),
        plan: None,
        run: None,
        deployment: None,
        continuation_command: None,
        release_summary: summary,
        changelog_history_recovery: None,
        readiness: None,
        ci_release: Some(ownership),
    })
}

fn ci_release_ownership(component: &Component, component_id: &str) -> CiReleaseOwnership {
    let scope = ReleaseScope::resolve(component, component_id).ok();
    let git_root = scope
        .as_ref()
        .map(|scope| scope.git_root.clone())
        .unwrap_or_else(|| component.local_path.clone());
    // Local tags only: deferral must not fetch or otherwise mutate the checkout.
    let latest_tag = scope
        .as_ref()
        .and_then(|scope| scope.latest_tag().ok().flatten());
    let latest_tag_commit = latest_tag
        .as_deref()
        .and_then(|tag| homeboy_core::git::get_tag_commit(&git_root, tag).ok());
    let head_commit = homeboy_core::git::get_head_commit(&git_root).ok();
    let head_released = matches!(
        (&latest_tag_commit, &head_commit),
        (Some(tag), Some(head)) if tag == head
    );

    let github = component
        .remote_url
        .clone()
        .or_else(|| {
            homeboy_core::git::release_download::detect_remote_url(std::path::Path::new(
                &component.local_path,
            ))
        })
        .as_deref()
        .and_then(homeboy_core::git::release_download::parse_github_url);
    let (ci_release_url, next_action) = match github {
        Some(repo) => (
            Some(format!(
                "https://{}/{}/{}/actions",
                repo.host, repo.owner, repo.repo
            )),
            format!("gh run list --repo {}/{} --limit 5", repo.owner, repo.repo),
        ),
        None => (
            None,
            "check the component's CI release workflow run".to_string(),
        ),
    };

    CiReleaseOwnership {
        owner: "ci".to_string(),
        latest_tag,
        latest_tag_commit,
        head_commit,
        head_released,
        ci_release_url,
        next_action,
        ci_caller_marker: CI_CALLER_MARKER.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::component::ReleaseOwner;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn ci_caller_is_recognized_by_explicit_marker_or_github_actions() {
        assert!(!is_ci_release_caller_with(env(&[])));
        assert!(is_ci_release_caller_with(env(&[(
            "GITHUB_ACTIONS",
            "true"
        )])));
        assert!(is_ci_release_caller_with(env(&[(
            RELEASE_CALLER_ENV,
            "ci"
        )])));
        assert!(is_ci_release_caller_with(env(&[(
            RELEASE_CALLER_ENV,
            "CI"
        )])));
        assert!(!is_ci_release_caller_with(env(&[
            ("GITHUB_ACTIONS", "true"),
            (RELEASE_CALLER_ENV, "local"),
        ])));
        assert!(!is_ci_release_caller_with(env(&[(
            "GITHUB_ACTIONS",
            "false"
        )])));
    }

    #[test]
    fn release_owner_config_parses_and_defaults_to_local() {
        let config: homeboy_core::component::ComponentReleaseConfig =
            serde_json::from_str(r#"{"owner":"ci","publish":false}"#).expect("parse");
        assert_eq!(config.owner, Some(ReleaseOwner::Ci));
        assert!(config.ci_owns_release());

        let config: homeboy_core::component::ComponentReleaseConfig =
            serde_json::from_str(r#"{"publish":false}"#).expect("parse");
        assert_eq!(config.release_owner(), ReleaseOwner::Local);
        assert!(!config.ci_owns_release());

        assert!(
            serde_json::from_str::<homeboy_core::component::ComponentReleaseConfig>(
                r#"{"owner":"robot"}"#
            )
            .is_err()
        );
    }
}
