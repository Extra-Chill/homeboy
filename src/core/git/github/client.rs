//! Shared GitHub client plumbing: component → owner/repo resolution, `gh`
//! readiness/execution wrappers, token discovery, and small parsing helpers
//! reused across the issue, pull-request, fleet, and readiness submodules.

use std::path::Path;
use std::process::Command;

use crate::core::component;
use crate::core::deploy::release_download::{detect_remote_url, parse_github_url, GitHubRepo};
use crate::core::error::{Error, Result};

use super::super::gh_client::GhClient;
use super::super::resolve_target;

/// Resolve a component ID to its GitHub owner/repo via `remote_url` (or git fallback).
///
/// `path_override` lets callers point at an unregistered checkout (e.g. a CI
/// runner workspace with a portable `homeboy.json` but no global component
/// registry entry). When set, the component is discovered from the portable
/// config at that path instead of the global registry.
pub(in crate::core::git) fn resolve_component_github(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> Result<(String, GitHubRepo)> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let comp = component::resolve_effective(Some(&id), path_override, None)?;

    let remote_url = comp
        .remote_url
        .clone()
        .or_else(|| detect_remote_url(Path::new(&path)))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "remote_url",
                format!(
                    "Component '{}' has no GitHub remote (remote_url not set and `git remote get-url origin` failed)",
                    id
                ),
                None,
                Some(vec![
                    "Set it: homeboy component set <id> --json '{\"remote_url\":\"https://github.com/<owner>/<repo>\"}'".to_string(),
                    "Or configure a git remote in the component's local_path".to_string(),
                    "Or pass --path <workspace> to discover from a portable homeboy.json".to_string(),
                ]),
            )
        })?;

    let repo = parse_github_url(&remote_url).ok_or_else(|| {
        Error::validation_invalid_argument(
            "remote_url",
            format!(
                "Remote URL '{}' is not a GitHub URL (only github.com is supported)",
                remote_url
            ),
            None,
            Some(vec![
                "Use an HTTPS (https://github.com/owner/repo) or SSH (git@github.com:owner/repo) URL".to_string(),
            ]),
        )
    })?;

    Ok((id, repo))
}

/// Run `gh <args>` swallowing stdout/stderr, return whether it exited successfully.
/// Used for probe-style `gh` invocations that only care about the exit code
/// (e.g. `gh --version`, `gh auth status`, `gh release view`).
///
/// Public so other modules can consolidate on one probe helper instead of
/// reimplementing the same `Command::new + null stdio + status` pattern.
pub fn gh_probe_succeeds(args: &[&str]) -> bool {
    let mut command = Command::new("gh");
    command.args(args);
    super::super::gh_client::command_probe_succeeds(command)
}

/// Resolve a GitHub token for scripts that require `GH_TOKEN` explicitly.
///
/// Prefer the caller's environment, then fall back to the authenticated GitHub
/// CLI token so extension scripts do not fail late after Homeboy has already
/// verified that `gh` is usable.
pub fn github_token_from_env_or_gh() -> Option<String> {
    select_github_token(
        std::env::var("GH_TOKEN").ok(),
        std::env::var("GITHUB_TOKEN").ok(),
        gh_auth_token,
    )
}

fn select_github_token(
    gh_token: Option<String>,
    github_token: Option<String>,
    gh_auth_token: impl FnOnce() -> Option<String>,
) -> Option<String> {
    gh_token
        .and_then(non_empty_token)
        .or_else(|| github_token.and_then(non_empty_token))
        .or_else(gh_auth_token)
}

fn non_empty_token(token: String) -> Option<String> {
    let token = token.trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn gh_auth_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }

    non_empty_token(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Error out if `gh` is missing or unauthenticated. Unlike `run_github_release`
/// (which soft-fails because the tag is already pushed), primitive operations
/// have no already-committed side effect to preserve — fail loudly.
pub(in crate::core::git) fn ensure_gh_ready() -> Result<()> {
    let host = std::env::var("GH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "github.com".to_string());
    GhClient::for_host(host).ensure_ready()
}

/// Run `gh <args>` and return stdout on success, or a structured error on
/// failure (with stderr captured in the error message).
pub(in crate::core::git) fn run_gh(args: &[String]) -> Result<String> {
    let host = std::env::var("GH_HOST")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "github.com".to_string());
    GhClient::for_host(host).run(args)
}

pub(super) fn parse_issue_number_from_url(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

pub(super) fn string_value(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_token_prefers_gh_token_env() {
        let token = select_github_token(
            Some(" env-gh-token \n".to_string()),
            Some("github-token".to_string()),
            || Some("cli-token".to_string()),
        );

        assert_eq!(token.as_deref(), Some("env-gh-token"));
    }

    #[test]
    fn github_token_falls_back_to_github_token_env() {
        let token = select_github_token(
            Some("  ".to_string()),
            Some("github-token".to_string()),
            || Some("cli-token".to_string()),
        );

        assert_eq!(token.as_deref(), Some("github-token"));
    }

    #[test]
    fn github_token_falls_back_to_gh_auth_token() {
        let token = select_github_token(None, None, || Some("cli-token".to_string()));

        assert_eq!(token.as_deref(), Some("cli-token"));
    }

    #[test]
    fn parse_issue_number_from_issue_url() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/issues/42"),
            Some(42)
        );
    }

    #[test]
    fn parse_issue_number_from_pr_url() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/pull/1337"),
            Some(1337)
        );
    }

    #[test]
    fn parse_issue_number_handles_trailing_slash() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/issues/42/"),
            Some(42)
        );
    }

    #[test]
    fn parse_issue_number_none_for_non_numeric() {
        assert_eq!(
            parse_issue_number_from_url("https://github.com/owner/repo/issues/not-a-number"),
            None
        );
    }
}
