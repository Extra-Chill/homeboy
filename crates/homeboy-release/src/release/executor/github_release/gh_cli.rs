//! `gh` CLI probes, environment, command construction, and path/quote helpers.

use crate::release::types::ReleaseState;
use homeboy_core::component::GithubConfig;
use homeboy_core::engine::shell::quote_arg;
use homeboy_core::git::release_download::GitHubRepo;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub(crate) const GITHUB_RELEASE_UPLOAD_TIMEOUT_ENV: &str =
    "HOMEBOY_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS";
const DEFAULT_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS: u64 = 30 * 60;
const GITHUB_RELEASE_DOWNLOAD_DIAGNOSTIC_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GhCommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GitHubReleaseMetadata {
    #[serde(rename = "draft", alias = "isDraft")]
    pub is_draft: bool,
    #[serde(default)]
    pub assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GitHubReleaseAsset {
    #[serde(default)]
    pub id: Option<u64>,
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
/// proven present remotely. When GitHub omits its digest, the caller must
/// independently retrieve and hash the asset; name and size alone are never
/// sufficient publication authority.
pub(crate) fn reconcile_release_publications(
    publications: &[ReleaseAssetPublication],
    remote_assets: &[GitHubReleaseAsset],
    github: &GitHubRepo,
    config: &GithubConfig,
    repo_flag: &str,
) -> Result<(Vec<ReleaseAssetPublication>, Vec<ReleaseAssetPublication>), String> {
    reconcile_release_publications_with(publications, remote_assets, &mut |asset, expected_size| {
        download_release_asset_identity(github, config, repo_flag, asset, expected_size)
    })
}

fn reconcile_release_publications_with(
    publications: &[ReleaseAssetPublication],
    remote_assets: &[GitHubReleaseAsset],
    verify_digestless: &mut impl FnMut(&GitHubReleaseAsset, u64) -> Result<(u64, String), String>,
) -> Result<(Vec<ReleaseAssetPublication>, Vec<ReleaseAssetPublication>), String> {
    let mut upload = Vec::new();
    let mut existing = Vec::new();
    for publication in publications {
        let matches = remote_assets
            .iter()
            .filter(|asset| asset.name == publication.target_name)
            .collect::<Vec<_>>();
        let Some(remote) = matches.first().copied() else {
            upload.push(publication.clone());
            continue;
        };
        if matches.len() != 1 {
            return Err(format!(
                "GitHub Release has multiple assets named '{}'; canonical publication ownership is ambiguous",
                publication.target_name
            ));
        }
        if verify_release_publication(publication, remote, verify_digestless)? {
            existing.push(publication.clone());
        } else {
            return Err(format!(
                "GitHub Release asset '{}' conflicts with the canonical publication bytes",
                publication.target_name
            ));
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
    // `gh release view --json` uses GraphQL, whose release asset shape omits
    // the REST digest field. Recovery authority requires that digest, including
    // for drafts, so read the tag endpoint directly.
    let endpoint = format!("repos/{repo_flag}/releases/tags/{tag}");
    let output = run_gh_command(
        gh_command(github, config, &["api", &endpoint]),
        github_release_upload_timeout(),
    );
    if output.timed_out || output.exit_code != Some(0) {
        return Err(gh_failure_detail("gh api release metadata", &output));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|error| format!("GitHub REST release metadata was invalid: {error}"))
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
    github: &GitHubRepo,
    config: &GithubConfig,
    repo_flag: &str,
) -> Result<(), String> {
    verify_release_publications_with(publications, assets, &mut |asset, expected_size| {
        download_release_asset_identity(github, config, repo_flag, asset, expected_size)
    })
}

fn verify_release_publications_with(
    publications: &[ReleaseAssetPublication],
    assets: &[GitHubReleaseAsset],
    verify_digestless: &mut impl FnMut(&GitHubReleaseAsset, u64) -> Result<(u64, String), String>,
) -> Result<(), String> {
    for publication in publications {
        let matches = assets
            .iter()
            .filter(|asset| asset.name == publication.target_name)
            .collect::<Vec<_>>();
        let asset = matches.first().copied().ok_or_else(|| {
            format!(
                "GitHub Release is missing uploaded asset '{}'",
                publication.target_name
            )
        })?;
        if matches.len() != 1 {
            return Err(format!(
                "GitHub Release has multiple assets named '{}'; canonical publication ownership is ambiguous",
                publication.target_name
            ));
        }
        if !verify_release_publication(publication, asset, verify_digestless)? {
            return Err(format!(
                "GitHub Release asset '{}' does not match the canonical publication bytes",
                publication.target_name
            ));
        }
    }
    Ok(())
}

fn verify_release_publication(
    publication: &ReleaseAssetPublication,
    asset: &GitHubReleaseAsset,
    verify_digestless: &mut impl FnMut(&GitHubReleaseAsset, u64) -> Result<(u64, String), String>,
) -> Result<bool, String> {
    if asset.size != publication.size {
        return Ok(false);
    }
    if let Some(digest) = canonical_remote_digest(asset)? {
        return Ok(digest == format!("sha256:{}", publication.sha256));
    }
    let (downloaded_size, downloaded_sha256) = verify_digestless(asset, publication.size)?;
    Ok(downloaded_size == publication.size && downloaded_sha256 == publication.sha256)
}

pub(crate) fn download_release_asset_identity(
    github: &GitHubRepo,
    config: &GithubConfig,
    repo_flag: &str,
    asset: &GitHubReleaseAsset,
    expected_size: u64,
) -> Result<(u64, String), String> {
    let asset_id = asset.id.ok_or_else(|| {
        format!(
            "GitHub Release asset '{}' has no digest or asset ID; cannot safely verify canonical publication ownership",
            asset.name
        )
    })?;
    if asset.size != expected_size {
        return Err(format!(
            "GitHub Release asset '{}' has size {}, expected {}",
            asset.name, asset.size, expected_size
        ));
    }
    let endpoint = format!("repos/{repo_flag}/releases/assets/{asset_id}");
    let mut command = gh_command(
        github,
        config,
        &["api", &endpoint, "-H", "Accept: application/octet-stream"],
    );
    run_bounded_asset_download(
        &mut command,
        &asset.name,
        expected_size,
        github_release_upload_timeout(),
        None,
    )
}

fn run_bounded_asset_download(
    command: &mut Command,
    asset_name: &str,
    expected_size: u64,
    timeout: Duration,
    temp_dir: Option<&std::path::Path>,
) -> Result<(u64, String), String> {
    let download = match temp_dir {
        Some(directory) => tempfile::NamedTempFile::new_in(directory),
        None => tempfile::NamedTempFile::new(),
    }
    .map_err(|error| {
        format!(
            "could not create temporary file for GitHub Release asset '{}': {error}",
            asset_name
        )
    })?;
    let output_file = download.reopen().map_err(|error| {
        format!(
            "could not open temporary file for GitHub Release asset '{}': {error}",
            asset_name
        )
    })?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        format!(
            "could not download GitHub Release asset '{}': {error}",
            asset_name
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        format!(
            "could not capture download stream for GitHub Release asset '{}'",
            asset_name
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        format!(
            "could not capture download diagnostics for GitHub Release asset '{}'",
            asset_name
        )
    })?;
    let (stream_tx, stream_rx) = mpsc::channel();
    let output_handle = std::thread::spawn(move || {
        let _ = stream_tx.send(stream_bounded_asset(stdout, output_file, expected_size));
    });
    let stderr_handle = std::thread::spawn(move || drain_bounded_diagnostics(stderr));
    let started = Instant::now();
    let mut stream_result = None;
    let status = loop {
        match stream_rx.try_recv() {
            Ok(Ok(identity)) => stream_result = Some(identity),
            Ok(Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_handle.join();
                let _ = stderr_handle.join();
                return Err(format!(
                    "could not download GitHub Release asset '{}': {error}",
                    asset_name
                ));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_handle.join();
                let _ = stderr_handle.join();
                return Err(format!(
                    "could not download GitHub Release asset '{}': download stream ended unexpectedly",
                    asset_name
                ));
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_handle.join();
                let _ = stderr_handle.join();
                return Err(format!(
                    "download of GitHub Release asset '{}' timed out",
                    asset_name
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_handle.join();
                let _ = stderr_handle.join();
                return Err(format!(
                    "could not monitor download of GitHub Release asset '{}': {error}",
                    asset_name
                ));
            }
        }
    };
    let stream_result = match stream_result {
        Some(identity) => Ok(identity),
        None => stream_rx
            .recv()
            .map_err(|_| "download stream ended without reporting an asset identity".to_string())?,
    };
    let _ = output_handle.join();
    let diagnostics = stderr_handle.join().unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "could not download GitHub Release asset '{}': {}",
            asset_name,
            diagnostics.trim()
        ));
    }
    stream_result.map_err(|error| {
        format!(
            "could not download GitHub Release asset '{}': {error}",
            asset_name
        )
    })
}

fn stream_bounded_asset(
    mut input: impl Read,
    mut output: impl Write,
    expected_size: u64,
) -> Result<(u64, String), String> {
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| format!("could not read asset bytes: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = expected_size.saturating_sub(size).min(buffer.len() as u64) as usize;
        let accepted = read.min(remaining);
        if accepted > 0 {
            output
                .write_all(&buffer[..accepted])
                .map_err(|error| format!("could not write temporary asset bytes: {error}"))?;
            hasher.update(&buffer[..accepted]);
            size += accepted as u64;
        }
        if accepted != read {
            return Err(format!(
                "download exceeded expected byte length {expected_size}"
            ));
        }
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn drain_bounded_diagnostics(mut input: impl Read) -> String {
    let mut diagnostics = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let Ok(read) = input.read(&mut buffer) else {
            break;
        };
        if read == 0 {
            break;
        }
        let remaining = GITHUB_RELEASE_DOWNLOAD_DIAGNOSTIC_BYTES.saturating_sub(diagnostics.len());
        diagnostics.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    let mut diagnostics = String::from_utf8_lossy(&diagnostics).to_string();
    if truncated {
        diagnostics.push_str("\n[diagnostics truncated]");
    }
    diagnostics
}

/// GitHub REST represents release asset checksums as `sha256:<hex>`. Preserve
/// that algorithm marker while canonicalizing hex case before authority checks.
fn canonical_remote_digest(asset: &GitHubReleaseAsset) -> Result<Option<String>, String> {
    let Some(digest) = asset.digest.as_deref() else {
        return Ok(None);
    };
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(format!(
            "GitHub Release asset '{}' has an invalid digest; cannot safely verify canonical publication ownership",
            asset.name
        ));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "GitHub Release asset '{}' has an invalid digest; cannot safely verify canonical publication ownership",
            asset.name
        ));
    }
    Ok(Some(format!("sha256:{}", hex.to_ascii_lowercase())))
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

    fn remote_asset(name: &str, size: u64, digest: Option<String>) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            id: Some(123),
            name: name.to_string(),
            size,
            digest,
        }
    }

    fn unexpected_download(_: &GitHubReleaseAsset, _: u64) -> Result<(u64, String), String> {
        panic!("digest-present asset must not be downloaded")
    }

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
    fn bounded_asset_download_drains_stderr_pressure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 20000 ]; do printf 'diagnostic pressure line\\n' >&2; i=$((i+1)); done; printf 'asset bytes'",
        ]);

        let identity = run_bounded_asset_download(
            &mut command,
            "asset.zip",
            11,
            Duration::from_secs(5),
            Some(temp.path()),
        )
        .expect("stderr pressure must not deadlock the asset stream");

        assert_eq!(identity.0, 11);
        assert_eq!(identity.1, format!("{:x}", Sha256::digest(b"asset bytes")));
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .count(),
            0,
            "successful download must clean up its tempfile"
        );
    }

    #[test]
    fn bounded_asset_download_kills_oversized_output_and_cleans_tempfile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'oversized'; while :; do :; done"]);

        let error = run_bounded_asset_download(
            &mut command,
            "asset.zip",
            4,
            Duration::from_secs(5),
            Some(temp.path()),
        )
        .expect_err("oversized output must fail closed");

        assert!(error.contains("exceeded expected byte length 4"));
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .count(),
            0,
            "oversized download must clean up its tempfile"
        );
    }

    #[test]
    fn bounded_asset_download_times_out_and_cleans_tempfile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut command = Command::new("sh");
        command.args(["-c", "while :; do :; done"]);

        let error = run_bounded_asset_download(
            &mut command,
            "asset.zip",
            4,
            Duration::from_millis(20),
            Some(temp.path()),
        )
        .expect_err("stalled output must time out");

        assert!(error.contains("timed out"));
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .count(),
            0,
            "timed-out download must clean up its tempfile"
        );
    }

    #[test]
    fn verifies_asset_name_size_and_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("asset.zip");
        std::fs::write(&path, b"asset bytes").expect("write asset");
        let digest = format!("sha256:{:x}", Sha256::digest(b"asset bytes"));
        verify_release_assets(
            &[path.display().to_string()],
            &[remote_asset("asset.zip", 11, Some(digest))],
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
        let remote = remote_asset(
            "component.zip",
            publications[0].size,
            Some(format!("sha256:{}", publications[0].sha256)),
        );

        let (upload, existing) =
            reconcile_release_publications_with(&publications, &[remote], &mut unexpected_download)
                .expect("reconcile exact asset");
        assert!(upload.is_empty());
        assert_eq!(existing, publications);
    }

    #[test]
    fn rest_release_metadata_preserves_digest_for_draft_and_published_releases() {
        for draft in [true, false] {
            let metadata: GitHubReleaseMetadata = serde_json::from_value(serde_json::json!({
                "draft": draft,
                "assets": [{
                    "name": "component.zip",
                    "size": 15,
                    "digest": "sha256:ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789"
                }]
            }))
            .expect("GitHub REST metadata");

            assert_eq!(metadata.is_draft, draft);
            assert_eq!(
                canonical_remote_digest(&metadata.assets[0]).expect("valid digest"),
                Some(
                    "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn partial_reupload_reuses_verified_asset_and_uploads_only_missing_asset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("first.zip");
        let second = dir.path().join("second.zip");
        std::fs::write(&first, b"first bytes").expect("write first");
        std::fs::write(&second, b"second bytes").expect("write second");
        let publications = github_release_publications(&release_state_with_artifacts(vec![
            artifact(&first, None),
            artifact(&second, None),
        ]))
        .expect("publication identities");

        let (uploads, existing) = reconcile_release_publications_with(
            &publications,
            &[remote_asset(
                "first.zip",
                publications[0].size,
                Some(format!("sha256:{}", publications[0].sha256)),
            )],
            &mut unexpected_download,
        )
        .expect("partial recovery");

        assert_eq!(existing, vec![publications[0].clone()]);
        assert_eq!(uploads, vec![publications[1].clone()]);
    }

    #[test]
    fn authority_rejects_malformed_and_mismatching_remote_digests() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 1,
            source_path: "component.zip".to_string(),
        };
        for digest in [Some("sha256:not-a-digest"), Some("sha512:abcd")] {
            let error = reconcile_release_publications_with(
                &[publication.clone()],
                &[remote_asset(
                    &publication.target_name,
                    publication.size,
                    digest.map(str::to_string),
                )],
                &mut unexpected_download,
            )
            .expect_err("unverifiable remote digest must fail closed");
            assert!(error.contains("cannot safely verify canonical publication ownership"));
        }

        let error = reconcile_release_publications_with(
            &[publication.clone()],
            &[remote_asset(
                &publication.target_name,
                publication.size,
                Some(format!("sha256:{}", "b".repeat(64))),
            )],
            &mut unexpected_download,
        )
        .expect_err("mismatching remote digest must fail closed");
        assert!(error.contains("conflicts with the canonical publication bytes"));
    }

    #[test]
    fn conflicting_preexisting_canonical_asset_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("component.zip");
        std::fs::write(&path, b"component bytes").expect("write zip");
        let publications =
            github_release_publications(&release_state_with_artifacts(vec![artifact(&path, None)]))
                .expect("publication identity");

        let error = reconcile_release_publications_with(
            &publications,
            &[remote_asset(
                "component.zip",
                publications[0].size,
                Some(format!("sha256:{}", "f".repeat(64))),
            )],
            &mut unexpected_download,
        )
        .expect_err("conflicting bytes must fail");
        assert!(error.contains("conflicts with the canonical publication bytes"));
    }

    #[test]
    fn digestless_asset_is_reused_when_downloaded_identity_matches() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let mut downloads = 0;
        let (uploads, existing) = reconcile_release_publications_with(
            &[publication.clone()],
            &[remote_asset("component.zip", 15, None)],
            &mut |_, _| {
                downloads += 1;
                Ok((15, "a".repeat(64)))
            },
        )
        .expect("downloaded identity establishes publication authority");

        assert!(uploads.is_empty());
        assert_eq!(existing, vec![publication]);
        assert_eq!(downloads, 1);
    }

    #[test]
    fn digestless_asset_rejects_downloaded_byte_mismatch() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let error = reconcile_release_publications_with(
            &[publication],
            &[remote_asset("component.zip", 15, None)],
            &mut |_, _| Ok((15, "b".repeat(64))),
        )
        .expect_err("different downloaded bytes must fail closed");

        assert!(error.contains("conflicts with the canonical publication bytes"));
    }

    #[test]
    fn digestless_asset_rejects_download_failure() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let error = reconcile_release_publications_with(
            &[publication],
            &[remote_asset("component.zip", 15, None)],
            &mut |_, _| Err("authenticated asset download failed".to_string()),
        )
        .expect_err("download failure must fail closed");

        assert_eq!(error, "authenticated asset download failed");
    }

    #[test]
    fn duplicate_remote_asset_name_is_ambiguous() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let error = reconcile_release_publications_with(
            &[publication],
            &[
                remote_asset("component.zip", 15, None),
                remote_asset("component.zip", 15, None),
            ],
            &mut |_, _| panic!("ambiguous assets must not be downloaded"),
        )
        .expect_err("duplicate canonical names must fail closed");

        assert!(error.contains("canonical publication ownership is ambiguous"));
    }

    #[test]
    fn release_continues_after_digestless_asset_verification() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let next_step = verify_release_publications_with(
            &[publication],
            &[remote_asset("component.zip", 15, None)],
            &mut |_, _| Ok((15, "a".repeat(64))),
        )
        .map(|()| "publish.nodejs")
        .expect("verified assets satisfy the publication gate");

        assert_eq!(next_step, "publish.nodejs");
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
