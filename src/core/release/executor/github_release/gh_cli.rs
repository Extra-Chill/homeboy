//! `gh` CLI probes, environment, command construction, and path/quote helpers.

use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;
use crate::core::engine::shell::quote_arg;
use crate::core::release::types::ReleaseState;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const GITHUB_RELEASE_UPLOAD_TIMEOUT_ENV: &str =
    "HOMEBOY_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS";
const DEFAULT_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GhCommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GitHubReleaseMetadata {
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(default)]
    pub assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GitHubReleaseAsset {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
}

pub(crate) fn gh_is_available() -> bool {
    crate::core::git::gh_probe_succeeds(&["--version"])
}

pub(crate) fn gh_is_authenticated(github: &GitHubRepo, config: &GithubConfig) -> bool {
    gh_probe_succeeds(
        github,
        config,
        &["auth", "status", "--hostname", &github.host],
    )
}

pub(crate) fn gh_release_exists(
    github: &GitHubRepo,
    config: &GithubConfig,
    tag: &str,
    repo_flag: &str,
) -> bool {
    gh_probe_succeeds(github, config, &["release", "view", tag, "-R", repo_flag])
}

pub(crate) fn github_release_artifact_paths(state: &ReleaseState) -> Vec<String> {
    state
        .artifacts
        .iter()
        .filter_map(|artifact| {
            artifact
                .durable_path
                .as_deref()
                .filter(|path| path_is_file(path))
                .or(Some(artifact.path.as_str()))
                .filter(|path| path_is_file(path))
                .map(str::to_string)
        })
        .collect()
}

pub(crate) fn github_release_upload_timeout() -> Duration {
    std::env::var(GITHUB_RELEASE_UPLOAD_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS))
}

pub(crate) fn gh_release_metadata(
    github: &GitHubRepo,
    config: &GithubConfig,
    tag: &str,
    repo_flag: &str,
) -> Result<GitHubReleaseMetadata, String> {
    let output = run_gh_command(
        gh_command(
            github,
            config,
            &[
                "release",
                "view",
                tag,
                "-R",
                repo_flag,
                "--json",
                "isDraft,assets",
            ],
        ),
        github_release_upload_timeout(),
    );
    if output.timed_out || output.exit_code != Some(0) {
        return Err(gh_failure_detail("gh release view", &output));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|error| format!("gh release view returned invalid metadata: {error}"))
}

pub(crate) fn verify_release_assets(
    artifact_paths: &[String],
    assets: &[GitHubReleaseAsset],
) -> Result<(), String> {
    for path in artifact_paths {
        let metadata = std::fs::metadata(path)
            .map_err(|error| format!("could not read release artifact '{path}': {error}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("release artifact '{path}' has no valid filename"))?;
        let asset = assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| format!("GitHub Release is missing uploaded asset '{name}'"))?;
        if asset.size != metadata.len() {
            return Err(format!(
                "GitHub Release asset '{name}' has size {}, expected {}",
                asset.size,
                metadata.len()
            ));
        }
        if let Some(digest) = asset
            .digest
            .as_deref()
            .and_then(|value| value.strip_prefix("sha256:"))
        {
            let mut file = std::fs::File::open(path)
                .map_err(|error| format!("could not hash release artifact '{path}': {error}"))?;
            let mut hasher = Sha256::new();
            let mut buffer = [0; 8192];
            loop {
                let read = file.read(&mut buffer).map_err(|error| {
                    format!("could not hash release artifact '{path}': {error}")
                })?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            if format!("{:x}", hasher.finalize()) != digest {
                return Err(format!(
                    "GitHub Release asset '{name}' digest does not match uploaded artifact"
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn gh_failure_detail(command: &str, output: &GhCommandOutput) -> String {
    if output.timed_out {
        return format!("{command} timed out");
    }
    match output.exit_code {
        Some(code) => format!("{command} exited with status {code}"),
        None => format!("{command} did not return an exit status"),
    }
}

pub(crate) fn run_gh_command(mut command: Command, timeout: Duration) -> GhCommandOutput {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return GhCommandOutput {
                stdout: String::new(),
                stderr: error.to_string(),
                exit_code: None,
                timed_out: false,
            }
        }
    };
    let started = Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                break (child.wait().ok(), true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break (None, false),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut stream) = child.stdout.take() {
        let _ = stream.read_to_end(&mut stdout);
    }
    if let Some(mut stream) = child.stderr.take() {
        let _ = stream.read_to_end(&mut stderr);
    }
    GhCommandOutput {
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        exit_code: status.and_then(|status| status.code()),
        timed_out,
    }
}

fn path_is_file(path: &str) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

pub(super) fn gh_env_prefix(env: &[(String, String)]) -> String {
    let parts = env
        .iter()
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .map(|(key, value)| format!("{}={}", key, quote_arg(value)))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

pub(super) fn gh_env_hint(github: &GitHubRepo, env: &[(String, String)]) -> Option<String> {
    if github.host == "github.com" && env.is_empty() {
        return None;
    }

    let mut hints = Vec::new();
    let proxy_keys = env
        .iter()
        .filter(|(key, value)| is_proxy_env_key(key) && !value.is_empty())
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();
    if github.host != "github.com" {
        hints.push(format!(
            "GitHub Enterprise host detected: repair commands include GH_HOST={}",
            github.host
        ));
    }
    if !proxy_keys.is_empty() {
        hints.push(format!(
            "Proxy environment is included in repair commands: {}.",
            proxy_keys.join(", ")
        ));
    } else if github.host != "github.com" {
        hints.push(
            "If this Enterprise host requires a proxy, prefix the commands with the needed HTTPS_PROXY/HTTP_PROXY/ALL_PROXY value.".to_string(),
        );
    }

    Some(hints.join(" "))
}

fn is_proxy_env_key(key: &str) -> bool {
    matches!(
        key,
        "HTTPS_PROXY" | "https_proxy" | "HTTP_PROXY" | "http_proxy" | "ALL_PROXY" | "all_proxy"
    )
}

pub(super) fn safe_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn gh_probe_succeeds(github: &GitHubRepo, config: &GithubConfig, args: &[&str]) -> bool {
    command_probe_succeeds(gh_command(github, config, args))
}

/// Run a prepared command swallowing stdout/stderr and report whether it exited
/// successfully. Centralizes the probe-style `null stdio + status + success`
/// pattern so probe call sites do not each reimplement it.
fn command_probe_succeeds(mut command: std::process::Command) -> bool {
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(super) fn gh_command(
    github: &GitHubRepo,
    config: &GithubConfig,
    args: &[&str],
) -> std::process::Command {
    let mut command = std::process::Command::new("gh");
    command.args(args);
    for (key, value) in github_cli_env(github, config) {
        command.env(key, value);
    }
    command
}

pub(crate) fn github_cli_env(github: &GitHubRepo, config: &GithubConfig) -> Vec<(String, String)> {
    crate::core::git::github_cli_env(&github.host, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_command_reports_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 1"]);
        let output = run_gh_command(command, Duration::from_millis(10));
        assert!(output.timed_out);
        assert_ne!(output.exit_code, Some(0));
    }

    #[test]
    fn bounded_command_preserves_nonzero_empty_stderr() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 7"]);
        let output = run_gh_command(command, Duration::from_secs(1));
        assert_eq!(output.exit_code, Some(7));
        assert!(output.stderr.is_empty());
        assert!(!output.timed_out);
    }

    #[test]
    fn verifies_asset_name_size_and_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("asset.zip");
        std::fs::write(&path, b"asset bytes").expect("write asset");
        let digest = format!("sha256:{:x}", Sha256::digest(b"asset bytes"));
        verify_release_assets(
            &[path.display().to_string()],
            &[GitHubReleaseAsset {
                name: "asset.zip".to_string(),
                size: 11,
                digest: Some(digest),
            }],
        )
        .expect("verified asset");
    }
}
