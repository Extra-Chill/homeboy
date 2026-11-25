//! Shared GitHub CLI client helpers.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use crate::component::{GithubConfig, GithubHostConfig};
use crate::error::{Error, Result};
use crate::git::release_download::GitHubRepo;
use homeboy_engine_primitives::command::ExecutionOwner;
use std::collections::HashMap;

const API_CAPTURE_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Pauses before each retry of a transient GitHub failure.
#[cfg(not(test))]
const TRANSIENT_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
];
#[cfg(test)]
const TRANSIENT_RETRY_DELAYS: [Duration; 3] = [Duration::from_millis(1); 3];

/// GitHub's own server faults and network faults on the way to it, which a
/// later identical request can succeed past. Client errors, auth, rate
/// limits, and validation are not retried.
fn is_transient_github_failure(stderr: &str) -> bool {
    [
        "Something went wrong while executing your query",
        "HTTP 500",
        "HTTP 502",
        "HTTP 503",
        "HTTP 504",
        // Go net/http faults `gh` reports when GitHub is unreachable.
        "TLS handshake timeout",
        "i/o timeout",
        "Client.Timeout exceeded",
        "connection reset by peer",
        "connection refused",
        "unexpected EOF",
    ]
    .iter()
    .any(|fault| stderr.contains(fault))
}

#[derive(Clone)]
pub struct GhClient {
    host: String,
    repo: Option<String>,
    env: Vec<(String, String)>,
}

impl std::fmt::Debug for GhClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GhClient")
            .field("host", &self.host)
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
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

    /// Override ambient credentials for one invocation; never put tokens in argv.
    pub fn with_token(mut self, token: &str) -> Self {
        self.env
            .retain(|(key, _)| key != "GH_TOKEN" && key != "GH_ENTERPRISE_TOKEN");
        self.env.push(("GH_TOKEN".into(), token.into()));
        self.env.push(("GH_ENTERPRISE_TOKEN".into(), token.into()));
        self
    }

    /// Use a fixture executable without changing the process-wide PATH.
    #[cfg(test)]
    pub(super) fn with_test_path(mut self, path: &std::path::Path) -> Self {
        self.env
            .push(("PATH".into(), path.to_string_lossy().into_owned()));
        self
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

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        for (key, value) in &self.env {
            if key.ends_with("TOKEN") && !value.is_empty() {
                stdout = stdout.replace(value, "[REDACTED]");
            }
        }
        Ok(stdout)
    }

    pub fn output(&self, args: &[&str]) -> Result<Output> {
        retry_transient_github_failures(|| {
            self.command(args).output().map_err(|e| {
                Error::internal_io(format!("Failed to invoke gh: {e}"), Some("gh".into()))
            })
        })
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

    /// Read a GitHub API response without allowing a caller-wide deadline to be
    /// extended by one slow page. The child is reaped on timeout.
    pub fn api_json_until<T: DeserializeOwned>(&self, path: &str, deadline: Instant) -> Result<T> {
        let args = ["api", path];
        let output = self.output_until(&args, deadline)?;
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

    pub(crate) fn output_until(&self, args: &[&str], deadline: Instant) -> Result<Output> {
        if Instant::now() >= deadline {
            return Err(Error::internal_io(
                "GitHub API deadline exhausted before invocation",
                None,
            ));
        }
        let mut command = self.command(args);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut owner = ExecutionOwner::spawn(&mut command).map_err(|e| {
            Error::internal_io(format!("Failed to invoke gh: {e}"), Some("gh".into()))
        })?;
        let stdout = owner.take_stdout().expect("piped stdout");
        let stderr = owner.take_stderr().expect("piped stderr");
        let (stdout_tx, stdout_rx) = mpsc::channel();
        let stdout_reader = std::thread::spawn(move || {
            let _ = stdout_tx.send(read_bounded(stdout, API_CAPTURE_LIMIT_BYTES));
        });
        let (stderr_tx, stderr_rx) = mpsc::channel();
        let stderr_reader = std::thread::spawn(move || {
            let _ = stderr_tx.send(read_bounded(stderr, API_CAPTURE_LIMIT_BYTES));
        });
        let status = loop {
            match owner.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    let _ = owner.drain_and_reap();
                    join_gh_readers(stdout_reader, stderr_reader);
                    return Err(Error::internal_io("GitHub API deadline exhausted", None));
                }
                Err(error) => {
                    let _ = owner.drain_and_reap();
                    join_gh_readers(stdout_reader, stderr_reader);
                    return Err(Error::internal_io(
                        format!("Failed while waiting for gh: {error}"),
                        Some("gh".into()),
                    ));
                }
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let stdout = match stdout_rx.recv_timeout(remaining) {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                let _ = owner.drain_and_reap();
                join_gh_readers(stdout_reader, stderr_reader);
                return Err(error.into());
            }
            Err(_) => {
                let _ = owner.drain_and_reap();
                join_gh_readers(stdout_reader, stderr_reader);
                return Err(Error::internal_io(
                    "GitHub API stdout did not close before the deadline",
                    None,
                ));
            }
        };
        let stderr =
            match stderr_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    let _ = owner.drain_and_reap();
                    join_gh_readers(stdout_reader, stderr_reader);
                    return Err(error.into());
                }
                Err(_) => {
                    let _ = owner.drain_and_reap();
                    join_gh_readers(stdout_reader, stderr_reader);
                    return Err(Error::internal_io(
                        "GitHub API stderr did not close before the deadline",
                        None,
                    ));
                }
            };
        let stdout_join = stdout_reader.join();
        let stderr_join = stderr_reader.join();
        stdout_join.map_err(|_| Error::internal_io("GitHub API stdout reader panicked", None))?;
        stderr_join.map_err(|_| Error::internal_io("GitHub API stderr reader panicked", None))?;
        if stdout.truncated {
            return Err(Error::internal_io(
                format!(
                    "GitHub API stdout exceeded the {} byte capture limit",
                    API_CAPTURE_LIMIT_BYTES
                ),
                Some("gh api".into()),
            ));
        }
        if stderr.truncated {
            return Err(Error::internal_io(
                format!(
                    "GitHub API stderr exceeded the {} byte capture limit",
                    API_CAPTURE_LIMIT_BYTES
                ),
                Some("gh api".into()),
            ));
        }
        Ok(Output {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
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
        let mut combined = if stderr.is_empty() { stdout } else { stderr };
        for (key, value) in &self.env {
            if key.ends_with("TOKEN") && !value.is_empty() {
                combined = combined.replace(value, "[REDACTED]");
            }
        }
        Error::git_command_failed(format!("{action} failed: {combined}"))
    }
}

/// Runs a gh invocation, retrying GitHub server faults with backoff. A retried
/// write is safe: callers already reconcile "already exists" (PR publication
/// adopts the open PR), and edits are idempotent.
pub fn retry_transient_github_failures(
    mut invoke: impl FnMut() -> Result<Output>,
) -> Result<Output> {
    let mut delays = TRANSIENT_RETRY_DELAYS.iter();
    loop {
        let output = invoke()?;
        match delays.next() {
            Some(delay)
                if !output.status.success()
                    && is_transient_github_failure(&String::from_utf8_lossy(&output.stderr)) =>
            {
                std::thread::sleep(*delay)
            }
            _ => return Ok(output),
        }
    }
}

fn join_gh_readers(stdout: std::thread::JoinHandle<()>, stderr: std::thread::JoinHandle<()>) {
    let _ = stdout.join();
    let _ = stderr.join();
}

struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(BoundedRead { bytes, truncated });
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained != count;
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
    let global_config = crate::defaults::load_config();
    github_cli_env_with_global_and_inherited(host, config, &global_config.github_hosts, |key| {
        std::env::var(key).ok()
    })
}

fn github_cli_env_with_global_and_inherited(
    host: &str,
    config: &GithubConfig,
    global_hosts: &HashMap<String, GithubHostConfig>,
    inherited_env: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if host != "github.com" {
        env.push(("GH_HOST".to_string(), host.to_string()));
    }

    let global_host_config = global_hosts.get(host);
    let component_host_config = config.hosts.get(host);
    if let Some(proxy) = component_host_config
        .and_then(|host_config| host_config.proxy.clone())
        .or_else(|| global_host_config.and_then(|host_config| host_config.proxy.clone()))
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

    apply_host_env(&mut env, global_host_config);
    apply_host_env(&mut env, component_host_config);

    env
}

fn apply_host_env(env: &mut Vec<(String, String)>, host_config: Option<&GithubHostConfig>) {
    let Some(host_config) = host_config else {
        return;
    };
    let explicit = host_config
        .env
        .iter()
        .filter(|(key, _)| !key.is_empty() && key.as_str() != "GH_HOST")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    *env = super::transport::merge_explicit_git_env(std::mem::take(env), &explicit);
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
    use std::io::Cursor;

    use std::collections::HashMap;

    #[cfg(target_os = "linux")]
    use std::sync::{Mutex, OnceLock};
    #[cfg(target_os = "linux")]
    use std::time::{Duration, Instant};

    use crate::component::{GithubConfig, GithubHostConfig};

    #[test]
    fn invocation_token_overrides_ambient_without_entering_arguments_or_debug() {
        let client = GhClient::for_host("github.com").with_token("minted-secret");
        let command = client.command(&["pr", "create"]);
        let env: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[std::ffi::OsStr::new("GH_TOKEN")],
            Some(std::ffi::OsStr::new("minted-secret"))
        );
        assert_eq!(
            env[std::ffi::OsStr::new("GH_ENTERPRISE_TOKEN")],
            Some(std::ffi::OsStr::new("minted-secret"))
        );
        assert!(!format!("{client:?}").contains("minted-secret"));
        assert!(!command
            .get_args()
            .any(|arg| arg.to_string_lossy().contains("minted-secret")));
    }

    use super::{
        delete_branch_ref_api_args, github_cli_env, github_cli_env_with_global_and_inherited,
        pr_merge_api_args, GhClient,
    };

    #[test]
    fn repo_arg_accepts_host_qualified_repo() {
        let client = GhClient::from_repo_arg("github.example.com/acme/widgets").unwrap();

        assert_eq!(client.host(), "github.example.com");
        assert_eq!(client.repo(), Some("acme/widgets"));
    }

    #[test]
    fn for_repo_targets_parsed_enterprise_host_not_github_com() {
        // Regression guard: GitHub primitives must target the host parsed from
        // the component's remote_url, not silently default to github.com.
        let repo = crate::git::release_download::GitHubRepo {
            host: "github.a8c.com".to_string(),
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
        };

        let client = GhClient::for_repo(&repo);

        assert_eq!(client.host(), "github.a8c.com");
        assert_eq!(client.repo(), Some("acme/widgets"));
        // The enterprise host must be exported as GH_HOST for the `gh` process.
        assert!(client
            .env
            .contains(&("GH_HOST".to_string(), "github.a8c.com".to_string())));
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
        let config = GithubConfig {
            hosts,
            ..GithubConfig::default()
        };

        let env = github_cli_env("github.example.com", &config);

        assert!(env.contains(&("GH_HOST".to_string(), "github.example.com".to_string())));
        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://proxy.example.test:8443".to_string()
        )));
    }

    #[test]
    fn github_cli_env_sets_enterprise_host_without_implicit_proxy() {
        let env = github_cli_env_with_global_and_inherited(
            "github.enterprise.test",
            &GithubConfig::default(),
            &HashMap::new(),
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

        let env = github_cli_env_with_global_and_inherited(
            "github.enterprise.test",
            &GithubConfig::default(),
            &HashMap::new(),
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
        let config = GithubConfig {
            hosts,
            ..GithubConfig::default()
        };
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

        let env = github_cli_env_with_global_and_inherited(
            "github.enterprise.test",
            &config,
            &HashMap::new(),
            |key| inherited.get(key).cloned(),
        );

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
    fn github_cli_env_applies_global_host_env() {
        let mut global_hosts = HashMap::new();
        global_hosts.insert(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("socks5://127.0.0.1:8080".to_string()),
                env: HashMap::from([("NO_PROXY".to_string(), "metadata.internal".to_string())]),
            },
        );

        let env = github_cli_env_with_global_and_inherited(
            "github.enterprise.test",
            &GithubConfig::default(),
            &global_hosts,
            |_| None,
        );

        assert!(env.contains(&("GH_HOST".to_string(), "github.enterprise.test".to_string())));
        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "socks5://127.0.0.1:8080".to_string()
        )));
        assert!(env.contains(&("NO_PROXY".to_string(), "metadata.internal".to_string())));
    }

    #[test]
    fn github_cli_env_component_host_env_overrides_global_host_env() {
        let mut global_hosts = HashMap::new();
        global_hosts.insert(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("socks5://127.0.0.1:8080".to_string()),
                env: HashMap::from([("NO_PROXY".to_string(), "global".to_string())]),
            },
        );
        let mut component_hosts = HashMap::new();
        component_hosts.insert(
            "github.enterprise.test".to_string(),
            GithubHostConfig {
                proxy: Some("https://component-proxy.example.test".to_string()),
                env: HashMap::from([("NO_PROXY".to_string(), "component".to_string())]),
            },
        );
        let config = GithubConfig {
            release_latest: None,
            hosts: component_hosts,
        };

        let env = github_cli_env_with_global_and_inherited(
            "github.enterprise.test",
            &config,
            &global_hosts,
            |_| None,
        );

        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://component-proxy.example.test".to_string()
        )));
        assert!(env.contains(&("NO_PROXY".to_string(), "component".to_string())));
        assert!(!env.contains(&("NO_PROXY".to_string(), "global".to_string())));
    }

    /// Merges and branch deletes go through the REST API, never `gh pr merge`,
    /// which would check out the default branch in the caller's worktree.
    #[test]
    fn merge_and_branch_delete_use_rest_api_not_checkout_commands() {
        assert_eq!(
            pr_merge_api_args("Extra-Chill/homeboy", 6006, "merge"),
            vec![
                "api",
                "-X",
                "PUT",
                "repos/Extra-Chill/homeboy/pulls/6006/merge",
                "-f",
                "merge_method=merge"
            ]
        );
        assert_eq!(
            delete_branch_ref_api_args("Extra-Chill/homeboy", "fix/example"),
            vec![
                "api",
                "-X",
                "DELETE",
                "repos/Extra-Chill/homeboy/git/refs/heads/fix/example"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn token_is_redacted_from_command_failure() {
        use std::os::unix::process::ExitStatusExt;
        let client = GhClient::for_host("github.com").with_token("minted-secret");
        let error = client.command_failed(
            "gh pr create",
            std::process::Output {
                status: std::process::ExitStatus::from_raw(256),
                stdout: vec![],
                stderr: b"rejected minted-secret".to_vec(),
            },
        );
        let evidence = format!("{error:?}");
        assert!(!evidence.contains("minted-secret"));
        assert!(evidence.contains("[REDACTED]"));
    }

    /// A GitHub server or network fault is retried; a client error fails at once.
    #[test]
    fn transient_github_failures_retry_and_client_errors_do_not() {
        let fixture = tempfile::tempdir().unwrap();
        let calls = fixture.path().join("calls");
        homeboy_test_support::write_executable(
            &fixture.path().join("gh"),
            format!(
                "#!/bin/sh\necho x >> {calls}\nn=0\nwhile read -r _; do n=$((n+1)); done < {calls}\ncase \"$1:$n\" in\n  flaky:1) echo 'GraphQL: Something went wrong while executing your query on 2026-11-20T23:58:21Z.' >&2; exit 1;;\n  flaky:2) echo 'HTTP 502: Bad Gateway' >&2; exit 1;;\n  flaky:3) echo 'Post \"https://api.github.com/graphql\": net/http: TLS handshake timeout' >&2; exit 1;;\n  flaky:*) echo https://github.com/o/r/pull/7;;\n  *) echo 'GraphQL: Could not resolve to a Repository' >&2; exit 1;;\nesac\n",
                calls = homeboy_engine_primitives::shell::quote_path(&calls.to_string_lossy()),
            ),
            0o755,
        )
        .unwrap();
        let client = GhClient::for_host("github.com").with_test_path(fixture.path());
        let count = || std::fs::read_to_string(&calls).unwrap().lines().count();

        assert_eq!(
            client.run(&["flaky".into()]).unwrap().trim(),
            "https://github.com/o/r/pull/7"
        );
        assert_eq!(count(), 4);
        std::fs::remove_file(&calls).unwrap();
        let error = client.run(&["missing".into()]).unwrap_err();
        assert!(error.message.contains("Could not resolve"), "{error:?}");
        assert_eq!(count(), 1);
    }

    #[test]
    fn bounded_api_capture_drains_without_retaining_unbounded_stdout() {
        let captured = super::read_bounded(Cursor::new(b"abcdef"), 4).unwrap();

        assert_eq!(captured.bytes, b"abcd");
        assert!(captured.truncated);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bounded_gh_capture_reaps_setsid_descendant_after_leader_exit() {
        static PATH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _path_lock = PATH_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        let pid_file = fixture.path().join("descendant.pid");
        let gh = fixture.path().join("gh");
        homeboy_test_support::write_executable(&gh, format!(
                "#!/bin/sh\nsetsid sh -c 'echo $$ > \"$1\"; sleep 30' sh {} &\nwhile [ ! -s {} ]; do :; done\nexit 0\n",
                homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
                homeboy_engine_primitives::shell::quote_path(&pid_file.to_string_lossy()),
            ), 0o755)
        .unwrap();

        let previous_path = std::env::var_os("PATH");
        let mut paths = vec![fixture.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &previous_path.clone().unwrap_or_default(),
        ));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());

        let client = GhClient::for_host("github.com");
        let result = client.output_until(&[], Instant::now() + Duration::from_secs(1));
        if let Some(path) = previous_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        result.expect("contained gh capture completes after cleanup");
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            !crate::process::pid_is_running(descendant_pid),
            "setsid gh descendant {descendant_pid} survived capture cleanup"
        );
    }
}
