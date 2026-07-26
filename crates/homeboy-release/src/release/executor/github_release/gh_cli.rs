//! `gh` CLI probes, environment, command construction, and path/quote helpers.

use crate::release::types::ReleaseState;
use homeboy_core::component::GithubConfig;
use homeboy_core::engine::shell::quote_arg;
use homeboy_core::git::release_download::GitHubRepo;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::HashSet;
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

/// A content-addressed intent to publish bytes under one canonical remote name.
/// The durable path is an implementation detail; extension and core publishers
/// coordinate through this target-name plus digest identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReleaseAssetPublication {
    pub target_name: String,
    pub sha256: String,
    pub size: u64,
    pub source_path: String,
}

impl ReleaseAssetPublication {
    pub(crate) fn upload_spec(&self) -> String {
        self.source_path.clone()
    }
}

pub(crate) fn gh_is_available() -> bool {
    homeboy_core::git::gh_probe_succeeds(&["--version"])
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
    let authority_established = state
        .artifacts
        .iter()
        .any(|artifact| artifact.publication_authority);
    state
        .artifacts
        .iter()
        .filter(|artifact| !authority_established || artifact.publication_authority)
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

/// Resolve publication identities for all direct release artifacts.  The path
/// declared by the producer names the remote target while a durable copy may
/// provide the bytes after the source workspace has been cleaned up.
pub(crate) fn github_release_publications(
    state: &ReleaseState,
) -> Result<Vec<ReleaseAssetPublication>, String> {
    let mut publications: BTreeMap<String, ReleaseAssetPublication> = BTreeMap::new();
    let mut direct_sources = HashSet::new();
    let authority_established = state
        .artifacts
        .iter()
        .any(|artifact| artifact.publication_authority);
    for artifact in state
        .artifacts
        .iter()
        .filter(|artifact| !authority_established || artifact.publication_authority)
    {
        let source_path = artifact
            .durable_path
            .as_deref()
            .filter(|path| path_is_file(path))
            .or_else(|| path_is_file(&artifact.path).then_some(artifact.path.as_str()))
            .ok_or_else(|| format!("release artifact '{}' is missing", artifact.path))?;
        let target_name = release_asset_name(&artifact.path);
        if target_name.is_empty() {
            return Err(format!(
                "release artifact '{}' has no valid filename",
                artifact.path
            ));
        }
        let publication = publication_from_path(
            &stage_canonical_upload_path(source_path, &target_name)?,
            target_name,
        )?;
        direct_sources.insert(source_path.to_string());
        match publications.get(&publication.target_name) {
            Some(existing) if existing.sha256 == publication.sha256 => {}
            Some(_) => {
                return Err(format!(
                    "release assets targeting '{}' have conflicting bytes",
                    publication.target_name
                ))
            }
            None => {
                publications.insert(publication.target_name.clone(), publication);
            }
        }
    }
    // Distribution manifests can declare archives that were not explicit
    // extension outputs. They retain their own filenames as canonical targets.
    for source_path in github_release_asset_paths(state)? {
        if direct_sources.contains(&source_path) {
            continue;
        }
        let target_name = release_asset_name(&source_path);
        let publication = publication_from_path(
            &stage_canonical_upload_path(&source_path, &target_name)?,
            target_name,
        )?;
        match publications.get(&publication.target_name) {
            Some(existing) if existing.sha256 == publication.sha256 => {}
            Some(_) => {
                return Err(format!(
                    "release assets targeting '{}' have conflicting bytes",
                    publication.target_name
                ))
            }
            None => {
                publications.insert(publication.target_name.clone(), publication);
            }
        }
    }
    Ok(publications.into_values().collect())
}

/// GitHub names an upload from the local filename; `gh`'s `#label` suffix only
/// changes the display label. Keep a canonical durable filename beside numbered
/// durable copies so retries and repair commands upload the intended asset name.
fn stage_canonical_upload_path(source_path: &str, target_name: &str) -> Result<String, String> {
    let source = std::path::Path::new(source_path);
    if source.file_name().and_then(|name| name.to_str()) == Some(target_name) {
        return Ok(source_path.to_string());
    }
    let parent = source
        .parent()
        .ok_or_else(|| format!("release artifact '{source_path}' has no parent directory"))?;
    let target = parent.join(target_name);
    if target.is_file() {
        let source_digest = file_sha256(source_path)?;
        let target_digest = file_sha256(&target.display().to_string())?;
        if source_digest != target_digest {
            return Err(format!(
                "release assets targeting '{target_name}' have conflicting bytes"
            ));
        }
        return Ok(target.display().to_string());
    }
    std::fs::hard_link(source, &target)
        .or_else(|_| std::fs::copy(source, &target).map(|_| ()))
        .map_err(|error| {
            format!(
                "could not stage canonical release artifact '{source_path}' as '{}': {error}",
                target.display()
            )
        })?;
    Ok(target.display().to_string())
}

fn publication_from_path(
    source_path: &str,
    target_name: String,
) -> Result<ReleaseAssetPublication, String> {
    let metadata = std::fs::metadata(source_path)
        .map_err(|error| format!("could not read release artifact '{source_path}': {error}"))?;
    if metadata.len() == 0 {
        return Err(format!("release asset '{target_name}' is empty"));
    }
    let sha256 = file_sha256(source_path)?;
    Ok(ReleaseAssetPublication {
        target_name,
        sha256,
        size: metadata.len(),
        source_path: source_path.to_string(),
    })
}

fn file_sha256(path: &str) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("could not hash release artifact '{path}': {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not hash release artifact '{path}': {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Split publications into assets that must be uploaded and assets already
/// proven present remotely. A remote digest is required to make idempotence
/// safe: matching only a name or size could silently accept different bytes.
pub(crate) fn reconcile_release_publications(
    publications: &[ReleaseAssetPublication],
    remote_assets: &[GitHubReleaseAsset],
) -> Result<(Vec<ReleaseAssetPublication>, Vec<ReleaseAssetPublication>), String> {
    let mut upload = Vec::new();
    let mut existing = Vec::new();
    for publication in publications {
        let Some(remote) = remote_assets
            .iter()
            .find(|asset| asset.name == publication.target_name)
        else {
            upload.push(publication.clone());
            continue;
        };
        let digest = remote
            .digest
            .as_deref()
            .and_then(|value| value.strip_prefix("sha256:"));
        match digest {
            Some(digest) if digest == publication.sha256 && remote.size == publication.size => {
                existing.push(publication.clone());
            }
            Some(_) => return Err(format!(
                "GitHub Release asset '{}' conflicts with the canonical publication bytes",
                publication.target_name
            )),
            None => return Err(format!(
                "GitHub Release asset '{}' has no digest; cannot safely verify canonical publication ownership",
                publication.target_name
            )),
        }
    }
    Ok((upload, existing))
}

/// Resolve every local asset required to publish a release. A cargo-dist
/// manifest is itself an uploadable asset, but it also declares the archives
/// that must exist before the release can become public.
pub(crate) fn github_release_asset_paths(state: &ReleaseState) -> Result<Vec<String>, String> {
    let mut paths = github_release_artifact_paths(state);
    let manifests = paths
        .iter()
        .filter(|path| {
            std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                == Some("dist-manifest.json")
        })
        .cloned()
        .collect::<Vec<_>>();

    for manifest in manifests {
        let contents = std::fs::read_to_string(&manifest).map_err(|error| {
            format!("could not read distribution manifest '{manifest}': {error}")
        })?;
        let value: serde_json::Value = serde_json::from_str(&contents).map_err(|error| {
            format!("could not parse distribution manifest '{manifest}': {error}")
        })?;
        let manifest_dir = std::path::Path::new(&manifest)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        collect_manifest_assets(&value, manifest_dir, &mut paths);
    }

    paths.sort();
    paths.dedup();
    let missing = paths
        .iter()
        .filter(|path| !path_is_file(path))
        .map(|path| release_asset_name(path))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        Err(format!(
            "release assets declared by dist-manifest.json are missing: {}. Run the package step again, then resume with `homeboy release --head --from-artifacts <artifact-dir>`.",
            missing.join(", ")
        ))
    } else if let Some(path) = paths.iter().find(|path| {
        std::fs::metadata(path)
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(false)
    }) {
        Err(format!(
            "release asset '{}' is empty",
            release_asset_name(path)
        ))
    } else {
        Ok(paths)
    }
}

fn collect_manifest_assets(
    value: &serde_json::Value,
    manifest_dir: &std::path::Path,
    paths: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(object) => {
            // cargo-dist artifacts use a path/name pair. Require a distributable
            // filename so unrelated manifest metadata is never treated as an asset.
            if let Some(path) = object.get("path").and_then(serde_json::Value::as_str) {
                let candidate = std::path::Path::new(path);
                if is_release_archive(candidate) {
                    paths.push(manifest_dir.join(candidate).display().to_string());
                }
            }
            for value in object.values() {
                collect_manifest_assets(value, manifest_dir, paths);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_manifest_assets(value, manifest_dir, paths);
            }
        }
        _ => {}
    }
}

fn is_release_archive(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            [".tar.xz", ".tar.gz", ".tar.zst", ".zip", ".sha256"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
}

fn release_asset_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
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

pub(crate) fn verify_release_publications(
    publications: &[ReleaseAssetPublication],
    assets: &[GitHubReleaseAsset],
) -> Result<(), String> {
    for publication in publications {
        let asset = assets
            .iter()
            .find(|asset| asset.name == publication.target_name)
            .ok_or_else(|| {
                format!(
                    "GitHub Release is missing uploaded asset '{}'",
                    publication.target_name
                )
            })?;
        if asset.size != publication.size
            || asset.digest.as_deref() != Some(&format!("sha256:{}", publication.sha256))
        {
            return Err(format!(
                "GitHub Release asset '{}' does not match the canonical publication bytes",
                publication.target_name
            ));
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
    homeboy_core::git::github_cli_env(&github.host, config)
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

    #[test]
    fn publishing_uses_durable_release_artifact_after_source_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let durable = dir.path().join("durable.zip");
        std::fs::write(&durable, b"release bytes").expect("durable artifact");
        let state = ReleaseState {
            artifacts: vec![crate::release::types::ReleaseArtifact {
                path: dir.path().join("removed-source.zip").display().to_string(),
                durable_path: Some(durable.display().to_string()),
                artifact_type: None,
                platform: None,
                phase: "final".to_string(),
                producer: "test".to_string(),
                sha256: None,
                publication_authority: false,
            }],
            ..ReleaseState::default()
        };

        assert_eq!(
            github_release_artifact_paths(&state),
            vec![durable.display().to_string()]
        );
    }

    #[test]
    fn manifest_only_release_fails_with_the_missing_target_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = dir.path().join("dist-manifest.json");
        std::fs::write(
            &manifest,
            r#"{"artifacts":[{"path":"homeboy-x86_64-unknown-linux-gnu.tar.xz"}]}"#,
        )
        .expect("write manifest");
        let state = ReleaseState {
            artifacts: vec![crate::release::types::ReleaseArtifact {
                path: manifest.display().to_string(),
                durable_path: None,
                artifact_type: None,
                platform: None,
                phase: "final".to_string(),
                producer: "test".to_string(),
                sha256: None,
                publication_authority: false,
            }],
            ..ReleaseState::default()
        };

        let error = github_release_asset_paths(&state).expect_err("missing archive");
        assert!(error.contains("homeboy-x86_64-unknown-linux-gnu.tar.xz"));
        assert!(error.contains("homeboy release --head --from-artifacts"));
    }

    #[test]
    fn manifest_declared_archives_are_uploaded_with_the_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = dir.path().join("dist-manifest.json");
        let archive = dir.path().join("homeboy-x86_64-unknown-linux-gnu.tar.xz");
        std::fs::write(&archive, b"archive").expect("write archive");
        std::fs::write(
            &manifest,
            r#"{"artifacts":[{"path":"homeboy-x86_64-unknown-linux-gnu.tar.xz"}]}"#,
        )
        .expect("write manifest");
        let state = ReleaseState {
            artifacts: vec![crate::release::types::ReleaseArtifact {
                path: manifest.display().to_string(),
                durable_path: None,
                artifact_type: None,
                platform: None,
                phase: "final".to_string(),
                producer: "test".to_string(),
                sha256: None,
                publication_authority: false,
            }],
            ..ReleaseState::default()
        };

        assert_eq!(
            github_release_asset_paths(&state).expect("assets"),
            vec![
                manifest.display().to_string(),
                archive.display().to_string()
            ]
        );
    }

    #[test]
    fn release_assets_reject_zero_byte_external_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("homeboy-x86_64-unknown-linux-gnu.tar.xz");
        std::fs::write(&archive, []).expect("write empty archive");
        let state = ReleaseState {
            artifacts: vec![crate::release::types::ReleaseArtifact {
                path: archive.display().to_string(),
                durable_path: None,
                artifact_type: None,
                platform: None,
                phase: "final".to_string(),
                producer: "test".to_string(),
                sha256: None,
                publication_authority: false,
            }],
            ..ReleaseState::default()
        };

        let error = github_release_asset_paths(&state).expect_err("empty archive");
        assert_eq!(
            error,
            "release asset 'homeboy-x86_64-unknown-linux-gnu.tar.xz' is empty"
        );
    }

    #[test]
    fn verification_names_the_missing_uploaded_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("homeboy-x86_64-unknown-linux-gnu.tar.xz");
        std::fs::write(&path, b"archive").expect("write archive");

        let error = verify_release_assets(&[path.display().to_string()], &[])
            .expect_err("remote archive is absent");
        assert_eq!(
            error,
            "GitHub Release is missing uploaded asset 'homeboy-x86_64-unknown-linux-gnu.tar.xz'"
        );
    }

    fn release_state_with_artifacts(
        artifacts: Vec<crate::release::types::ReleaseArtifact>,
    ) -> ReleaseState {
        ReleaseState {
            artifacts,
            ..ReleaseState::default()
        }
    }

    fn artifact(
        path: &std::path::Path,
        durable_path: Option<&std::path::Path>,
    ) -> crate::release::types::ReleaseArtifact {
        crate::release::types::ReleaseArtifact {
            path: path.display().to_string(),
            durable_path: durable_path.map(|path| path.display().to_string()),
            artifact_type: None,
            platform: None,
            phase: "final".to_string(),
            producer: "test".to_string(),
            sha256: None,
            publication_authority: false,
        }
    }

    #[test]
    fn numbered_durable_zip_publishes_to_the_declared_canonical_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().join("component.zip");
        let durable = dir.path().join("01-component.zip");
        std::fs::write(&durable, b"component bytes").expect("write durable zip");

        let publications =
            github_release_publications(&release_state_with_artifacts(vec![artifact(
                &canonical,
                Some(&durable),
            )]))
            .expect("publication identity");

        assert_eq!(publications.len(), 1);
        assert_eq!(publications[0].target_name, "component.zip");
        assert_eq!(
            publications[0].upload_spec(),
            canonical.display().to_string()
        );
        assert_eq!(
            std::fs::read(&canonical).expect("staged canonical zip"),
            b"component bytes"
        );
    }

    #[test]
    fn canonical_durable_name_rejects_conflicting_bytes_without_replacing_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().join("component.zip");
        let durable = dir.path().join("01-component.zip");
        std::fs::write(&canonical, b"existing bytes").expect("write canonical zip");
        std::fs::write(&durable, b"component bytes").expect("write durable zip");

        let error = github_release_publications(&release_state_with_artifacts(vec![artifact(
            &canonical,
            Some(&durable),
        )]))
        .expect_err("conflicting durable canonical name must fail");

        assert!(error.contains("targeting 'component.zip' have conflicting bytes"));
        assert_eq!(
            std::fs::read(&canonical).expect("canonical zip retained"),
            b"existing bytes"
        );
    }

    #[test]
    fn exact_remote_asset_is_reused_on_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("component.zip");
        std::fs::write(&path, b"component bytes").expect("write zip");
        let publications =
            github_release_publications(&release_state_with_artifacts(vec![artifact(&path, None)]))
                .expect("publication identity");
        let remote = GitHubReleaseAsset {
            name: "component.zip".to_string(),
            size: publications[0].size,
            digest: Some(format!("sha256:{}", publications[0].sha256)),
        };

        let (upload, existing) = reconcile_release_publications(&publications, &[remote])
            .expect("reconcile exact asset");
        assert!(upload.is_empty());
        assert_eq!(existing, publications);
    }

    #[test]
    fn conflicting_preexisting_canonical_asset_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("component.zip");
        std::fs::write(&path, b"component bytes").expect("write zip");
        let publications =
            github_release_publications(&release_state_with_artifacts(vec![artifact(&path, None)]))
                .expect("publication identity");

        let error = reconcile_release_publications(
            &publications,
            &[GitHubReleaseAsset {
                name: "component.zip".to_string(),
                size: publications[0].size,
                digest: Some("sha256:conflicting".to_string()),
            }],
        )
        .expect_err("conflicting bytes must fail");
        assert!(error.contains("conflicts with the canonical publication bytes"));
    }

    #[test]
    fn distinct_component_assets_are_preserved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first.zip");
        let second = dir.path().join("second.zip");
        std::fs::write(&first, b"same bytes").expect("write first zip");
        std::fs::write(&second, b"same bytes").expect("write second zip");

        let publications = github_release_publications(&release_state_with_artifacts(vec![
            artifact(&first, None),
            artifact(&second, None),
        ]))
        .expect("publication identities");

        assert_eq!(publications.len(), 2);
        assert_eq!(
            publications
                .iter()
                .map(|asset| &asset.target_name)
                .collect::<Vec<_>>(),
            vec!["first.zip", "second.zip"]
        );
    }

    #[test]
    fn mixed_extension_versions_keep_legacy_artifacts_and_add_publication_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().join("component.zip");
        let durable = dir.path().join("01-component.zip");
        std::fs::write(&durable, b"component bytes").expect("write durable zip");
        let state = release_state_with_artifacts(vec![artifact(&canonical, Some(&durable))]);
        let payload = crate::release::executor::package::build_release_payload(
            &state,
            "component",
            ".",
            None,
            None,
        );

        assert_eq!(
            payload["release"]["artifacts"][0]["path"],
            canonical.display().to_string()
        );
        assert_eq!(
            payload["release"]["asset_publications"][0]["target_name"],
            "component.zip"
        );
    }
}
