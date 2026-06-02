//! GitHub Release helper result builders and probes.

use crate::core::deploy::release_download::GitHubRepo;
use std::process::Command;

use super::step_success;
use crate::core::release::types::ReleaseStepResult;

pub(super) fn skipped_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    fallback_command: Option<String>,
) -> ReleaseStepResult {
    let mut data = serde_json::json!({
        "skipped": true,
        "reason": reason,
        "tag": tag,
        "owner": github.owner,
        "repo": github.repo,
    });
    if let Some(fallback) = fallback_command {
        data["fallback_command"] = serde_json::json!(fallback);
    }

    step_success("github.release", "github.release", Some(data), Vec::new())
}

pub(super) fn upload_failed_result(
    tag: &str,
    github: &GitHubRepo,
    stdout: String,
    stderr: String,
    artifact_count: usize,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "skipped": true,
            "reason": "gh-upload-failed",
            "tag": tag,
            "owner": github.owner,
            "repo": github.repo,
            "stdout": stdout,
            "stderr": stderr,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
    )
}

pub(super) fn upload_success_result(
    tag: &str,
    github: &GitHubRepo,
    artifact_count: usize,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release.upload",
            "tag": tag,
            "owner": github.owner,
            "repo": github.repo,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
    )
}

pub(crate) fn github_command_env(github: &GitHubRepo) -> Vec<(String, String)> {
    let mut env = Vec::new();

    if github.host != "github.com" {
        env.push(("GH_HOST".to_string(), github.host.clone()));
    }

    if github.host == "github.a8c.com" && !ambient_proxy_is_configured() {
        env.push((
            "HTTPS_PROXY".to_string(),
            "socks5://127.0.0.1:8080".to_string(),
        ));
    }

    env
}

pub(super) fn gh_is_available(env: &[(String, String)]) -> bool {
    gh_probe_succeeds_with_env(&["--version"], env)
}

pub(super) fn gh_is_authenticated(host: &str, env: &[(String, String)]) -> bool {
    gh_probe_succeeds_with_env(&["auth", "status", "--hostname", host], env)
}

pub(super) fn gh_release_exists(tag: &str, repo_flag: &str, env: &[(String, String)]) -> bool {
    gh_probe_succeeds_with_env(&["release", "view", tag, "-R", repo_flag], env)
}

pub(super) fn fallback_gh_command(tag: &str) -> String {
    format!(
        "gh release create {} --title {} --notes-file <path-to-release-notes>",
        tag, tag
    )
}

pub(super) fn sanitize_tag_for_filename(tag: &str) -> String {
    tag.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn ambient_proxy_is_configured() -> bool {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .any(|key| {
        std::env::var(key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

fn gh_probe_succeeds_with_env(args: &[&str], env: &[(String, String)]) -> bool {
    Command::new("gh")
        .args(args)
        .envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_command_env_sets_ghe_host() {
        let repo = GitHubRepo {
            host: "github.example.com".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        };

        assert_eq!(
            github_command_env(&repo)
                .into_iter()
                .find(|(key, _)| key == "GH_HOST"),
            Some(("GH_HOST".to_string(), "github.example.com".to_string()))
        );
    }

    #[test]
    fn github_command_env_keeps_github_com_default_host() {
        let repo = GitHubRepo {
            host: "github.com".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        };

        assert!(github_command_env(&repo)
            .into_iter()
            .all(|(key, _)| key != "GH_HOST"));
    }
}
