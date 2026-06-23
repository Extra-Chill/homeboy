//! Shared GitHub CLI client helpers.

use std::process::{Command, Output, Stdio};

use serde::de::DeserializeOwned;

use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct GhClient {
    host: String,
    repo: Option<String>,
    env: Vec<(String, String)>,
}

impl GhClient {
    pub fn for_repo(repo: &GitHubRepo) -> Self {
        Self::for_repo_with_config(repo, &GithubConfig::default())
    }

    pub fn for_repo_with_config(repo: &GitHubRepo, config: &GithubConfig) -> Self {
        Self {
            host: repo.host.clone(),
            repo: Some(format!("{}/{}", repo.owner, repo.repo)),
            env: github_cli_env(&repo.host, config),
        }
    }

    pub fn for_host(host: impl Into<String>) -> Self {
        let host = host.into();
        Self {
            env: github_cli_env(&host, &GithubConfig::default()),
            host,
            repo: None,
        }
    }

    pub fn from_repo_arg(repo: &str) -> Result<Self> {
        let repo = repo.trim();
        let parts: Vec<&str> = repo.split('/').collect();
        let (host, repo_slug) = match parts.as_slice() {
            [owner, name] if !owner.is_empty() && !name.is_empty() => {
                let host = std::env::var("GH_HOST")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "github.com".to_string());
                (host, format!("{owner}/{name}"))
            }
            [host, owner, name] if !host.is_empty() && !owner.is_empty() && !name.is_empty() => {
                ((*host).to_string(), format!("{owner}/{name}"))
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "repo",
                    "expected owner/repo or host/owner/repo form",
                    Some(repo.to_string()),
                    None,
                ));
            }
        };

        Ok(Self {
            env: github_cli_env(&host, &GithubConfig::default()),
            host,
            repo: Some(repo_slug),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn repo(&self) -> Option<&str> {
        self.repo.as_deref()
    }

    pub fn repo_path(&self) -> Result<&str> {
        self.repo.as_deref().ok_or_else(|| {
            Error::validation_missing_argument(vec!["GitHub repository".to_string()])
        })
    }

    pub fn ensure_ready(&self) -> Result<()> {
        if !self.probe(&["--version"]) {
            return Err(Error::internal_io(
                "`gh` CLI not found on PATH".to_string(),
                Some("gh".to_string()),
            )
            .with_hint("Install the GitHub CLI: https://cli.github.com"));
        }

        if !self.probe(&["auth", "status", "--hostname", &self.host]) {
            return Err(Error::internal_io(
                format!("`gh` is not authenticated for {}", self.host),
                Some(format!("gh auth status --hostname {}", self.host)),
            )
            .with_hint("Authenticate with: gh auth login"));
        }

        Ok(())
    }

    pub fn probe(&self, args: &[&str]) -> bool {
        command_probe_succeeds(self.command(args))
    }

    pub fn run(&self, args: &[String]) -> Result<String> {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = self.output(&arg_refs)?;
        if !output.status.success() {
            let action = format!("gh {}", args.first().map(String::as_str).unwrap_or(""));
            return Err(self.command_failed(&action, output));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn output(&self, args: &[&str]) -> Result<Output> {
        self.command(args)
            .output()
            .map_err(|e| Error::internal_io(format!("Failed to invoke gh: {e}"), Some("gh".into())))
    }

    pub fn api_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let args = ["api", path];
        let output = self.output(&args)?;
        if !output.status.success() {
            return Err(self.command_failed(&format!("gh api {path}"), output));
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|e| Error::internal_json(e.to_string(), Some(format!("parse gh api {path}"))))
    }

    pub fn api_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let args = ["api", path];
        let output = self.output(&args)?;
        if !output.status.success() {
            return Err(self.command_failed(&format!("gh api {path}"), output));
        }
        Ok(output.stdout)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("gh");
        command.args(args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    fn command_failed(&self, action: &str, output: Output) -> Error {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let combined = if stderr.is_empty() { stdout } else { stderr };
        Error::git_command_failed(format!("{action} failed: {combined}"))
    }
}

pub(crate) fn pr_merge_api_args(repo: &str, number: u64, method: &str) -> Vec<String> {
    vec![
        "api".to_string(),
        "-X".to_string(),
        "PUT".to_string(),
        format!("repos/{repo}/pulls/{number}/merge"),
        "-f".to_string(),
        format!("merge_method={method}"),
    ]
}

pub(crate) fn delete_branch_ref_api_args(repo: &str, branch: &str) -> Vec<String> {
    vec![
        "api".to_string(),
        "-X".to_string(),
        "DELETE".to_string(),
        format!("repos/{repo}/git/refs/heads/{branch}"),
    ]
}

/// Run a prepared `gh`/CLI command swallowing stdout/stderr, returning whether
/// it exited successfully.
///
/// This is the single probe primitive shared by both [`GhClient::probe`] (which
/// configures host/repo/env on the command) and the free [`gh_probe_succeeds`]
/// helper (which runs a bare `gh` invocation). Both only care about the exit
/// code, so they consolidate on this `null stdio + status` pattern here.
pub fn command_probe_succeeds(mut command: Command) -> bool {
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn github_cli_env(host: &str, config: &GithubConfig) -> Vec<(String, String)> {
    github_cli_env_with_inherited(host, config, |key| std::env::var(key).ok())
}

fn github_cli_env_with_inherited(
    host: &str,
    config: &GithubConfig,
    inherited_env: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if host != "github.com" {
        env.push(("GH_HOST".to_string(), host.to_string()));
    }

    let host_config = config.hosts.get(host);
    if let Some(proxy) = host_config
        .and_then(|host_config| host_config.proxy.clone())
        .or_else(|| inherited_enterprise_https_proxy(host, &inherited_env))
        .filter(|proxy| !proxy.is_empty())
    {
        env.push(("HTTPS_PROXY".to_string(), proxy));
    }

    if host != "github.com" {
        for key in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            if env.iter().any(|(existing, _)| existing == key) {
                continue;
            }
            if let Some(value) = inherited_env(key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            {
                env.push((key.to_string(), value));
            }
        }
    }

    let Some(host_config) = host_config else {
        return env;
    };

    for (key, value) in &host_config.env {
        if !key.is_empty() && key != "GH_HOST" {
            env.retain(|(existing, _)| existing != key);
            env.push((key.clone(), value.clone()));
        }
    }

    env
}

fn inherited_enterprise_https_proxy(
    host: &str,
    inherited_env: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if host == "github.com" {
        return None;
    }

    inherited_env("HTTPS_PROXY")
        .or_else(|| inherited_env("https_proxy"))
        .map(|proxy| proxy.trim().to_string())
        .filter(|proxy| !proxy.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::component::{GithubConfig, GithubHostConfig};

    use super::{
        delete_branch_ref_api_args, github_cli_env, github_cli_env_with_inherited,
        pr_merge_api_args, GhClient,
    };

    #[test]
    fn repo_arg_accepts_host_qualified_repo() {
        let client = GhClient::from_repo_arg("github.example.com/acme/widgets").unwrap();

        assert_eq!(client.host(), "github.example.com");
        assert_eq!(client.repo(), Some("acme/widgets"));
    }

    #[test]
    fn github_cli_env_sets_enterprise_host_and_configured_proxy() {
        let mut hosts = HashMap::new();
        hosts.insert(
            "github.example.com".to_string(),
            GithubHostConfig {
                proxy: Some("https://proxy.example.test:8443".to_string()),
                env: HashMap::new(),
            },
        );
        let config = GithubConfig { hosts };

        let env = github_cli_env("github.example.com", &config);

        assert!(env.contains(&("GH_HOST".to_string(), "github.example.com".to_string())));
        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://proxy.example.test:8443".to_string()
        )));
    }

    #[test]
    fn github_cli_env_sets_enterprise_host_without_implicit_proxy() {
        let env = github_cli_env_with_inherited(
            "github.enterprise.test",
            &GithubConfig::default(),
            |_| None,
        );

        assert_eq!(
            env,
            vec![("GH_HOST".to_string(), "github.enterprise.test".to_string())]
        );
    }

    #[test]
    fn github_cli_env_inherits_enterprise_proxy_context() {
        let inherited = HashMap::from([
            (
                "HTTP_PROXY".to_string(),
                "http://proxy.example.test:8080".to_string(),
            ),
            (
                "ALL_PROXY".to_string(),
                "socks5://127.0.0.1:8080".to_string(),
            ),
        ]);

        let env = github_cli_env_with_inherited(
            "github.enterprise.test",
            &GithubConfig::default(),
            |key| inherited.get(key).cloned(),
        );

        assert_eq!(
            env,
            vec![
                ("GH_HOST".to_string(), "github.enterprise.test".to_string()),
                (
                    "HTTP_PROXY".to_string(),
                    "http://proxy.example.test:8080".to_string()
                ),
                (
                    "ALL_PROXY".to_string(),
                    "socks5://127.0.0.1:8080".to_string()
                ),
            ]
        );
    }

    #[test]
    fn github_cli_env_keeps_configured_proxy_ahead_of_inherited_context() {
        let mut hosts = HashMap::new();
        hosts.insert(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("https://configured-proxy.example.test:8443".to_string()),
                env: HashMap::new(),
            },
        );
        let config = GithubConfig { hosts };
        let inherited = HashMap::from([
            (
                "HTTPS_PROXY".to_string(),
                "https://inherited-proxy.example.test:9443".to_string(),
            ),
            (
                "HTTP_PROXY".to_string(),
                "http://proxy.example.test:8080".to_string(),
            ),
        ]);

        let env = github_cli_env_with_inherited("github.enterprise.test", &config, |key| {
            inherited.get(key).cloned()
        });

        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://configured-proxy.example.test:8443".to_string()
        )));
        assert!(env.contains(&(
            "HTTP_PROXY".to_string(),
            "http://proxy.example.test:8080".to_string()
        )));
        assert!(!env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://inherited-proxy.example.test:9443".to_string()
        )));
    }

    #[test]
    fn github_cli_env_uses_configured_enterprise_proxy() {
        let mut hosts = HashMap::new();
        hosts.insert(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("https://proxy.example.test:9443".to_string()),
                env: HashMap::new(),
            },
        );
        let config = GithubConfig { hosts };

        let env = github_cli_env("github.enterprise.test", &config);

        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://proxy.example.test:9443".to_string()
        )));
    }

    #[test]
    fn pr_merge_api_args_do_not_use_checkout_merger() {
        let args = pr_merge_api_args("Extra-Chill/homeboy", 6006, "merge");

        assert_eq!(
            args,
            vec![
                "api",
                "-X",
                "PUT",
                "repos/Extra-Chill/homeboy/pulls/6006/merge",
                "-f",
                "merge_method=merge"
            ]
        );
        assert!(!args.windows(2).any(|window| window == ["pr", "merge"]));
    }

    #[test]
    fn delete_branch_ref_api_args_do_not_checkout_default_branch() {
        let args = delete_branch_ref_api_args("Extra-Chill/homeboy", "fix/example");

        assert_eq!(
            args,
            vec![
                "api",
                "-X",
                "DELETE",
                "repos/Extra-Chill/homeboy/git/refs/heads/fix/example"
            ]
        );
        assert!(!args.windows(2).any(|window| window == ["pr", "merge"]));
    }
}
