//! `gh` CLI probes, environment, command construction, and path/quote helpers.

use crate::release::types::ReleaseState;
use homeboy_core::component::GithubConfig;
use homeboy_core::engine::shell::quote_arg;
use homeboy_core::git::release_download::GitHubRepo;
use homeboy_core::redaction::RedactionPolicy;
use homeboy_engine_primitives::command::{
    isolate_process_tree, wait_with_bounded_output_supervised, SupervisedCommandTermination,
};
use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const GITHUB_RELEASE_UPLOAD_TIMEOUT_ENV: &str =
    "HOMEBOY_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS";
const DEFAULT_GITHUB_RELEASE_UPLOAD_TIMEOUT_SECS: u64 = 30 * 60;
const GITHUB_RELEASE_DOWNLOAD_DIAGNOSTIC_BYTES: usize = 8 * 1024;
const GITHUB_RELEASE_DOWNLOAD_READER_CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);
const GITHUB_RELEASE_DOWNLOAD_PIPE_CHUNKS_PER_TURN: usize = 4;
const GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT: usize = 4096;
const GITHUB_COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GhCommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

/// Safe, bounded evidence from a failed `gh` invocation for persisted release
/// step data. Command arguments are intentionally not retained because release
/// notes and upload paths can contain credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GitHubCommandFailureDiagnostic {
    pub operation: String,
    pub endpoint: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub http_status: Option<u16>,
    pub github_request_id: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub(crate) struct GitHubReleaseMetadataError {
    pub message: String,
    pub diagnostics: Vec<GitHubCommandFailureDiagnostic>,
}

impl std::fmt::Display for GitHubReleaseMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitHubReleaseMetadataError {}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GitHubReleaseMetadata {
    #[serde(default)]
    pub tag_name: String,
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
    pub state: String,
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
    content_hash::sha256_file(std::path::Path::new(path))
        .map_err(|error| format!("could not hash release artifact '{path}': {error}"))
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
            let observed = canonical_remote_digest(remote)?
                .unwrap_or_else(|| "downloaded bytes with no reported digest".to_string());
            return Err(format!(
                "GitHub Release asset '{}' conflicts with the canonical publication bytes (expected sha256:{}, found {})",
                publication.target_name, publication.sha256, observed
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
) -> Result<GitHubReleaseMetadata, GitHubReleaseMetadataError> {
    // `gh release view --json` uses GraphQL, whose release asset shape omits
    // the REST digest field. Recovery authority requires that digest, including
    // for drafts, so read the REST API directly.
    let endpoint = format!("repos/{repo_flag}/releases/tags/{tag}");
    let output = run_gh_command(
        gh_command(github, config, &["api", &endpoint]),
        github_release_upload_timeout(),
    );
    if !output.timed_out && output.exit_code == Some(0) {
        return serde_json::from_str(&output.stdout).map_err(|error| GitHubReleaseMetadataError {
            message: format!("GitHub REST release metadata was invalid: {error}"),
            diagnostics: vec![gh_failure_diagnostic(
                "gh api release metadata",
                &endpoint,
                &output,
            )],
        });
    }
    if output.timed_out {
        return Err(GitHubReleaseMetadataError {
            message: gh_failure_detail("gh api release metadata", &output),
            diagnostics: vec![gh_failure_diagnostic(
                "gh api release metadata",
                &endpoint,
                &output,
            )],
        });
    }

    // `releases/tags/{tag}` resolves published releases only -- GitHub returns
    // 404 for a draft, because a draft has no tag association on that endpoint.
    // A failed publish leaves exactly that state, so looking the release up by
    // tag made recovery impossible for the one case recovery exists to handle:
    // every retry 404'd, left the release a draft, and 404'd again forever.
    // Drafts are visible on the list endpoint, so fall back to resolving by id.
    //
    // Report BOTH failures (issue #10441). The fallback is the path that
    // matters for a stranded draft, so collapsing its failure into the
    // primary call's generic "exited with status 1" makes the one failure
    // that strands a release undiagnosable -- which is exactly what run
    // 30313665269 left behind for v0.321.1: a step error naming only the
    // expected 404, with no trace of why the draft lookup did not recover it.
    gh_draft_release_metadata(github, config, tag, repo_flag)
        .map_err(|draft_error| metadata_fallback_error(&endpoint, &output, draft_error))
}

fn metadata_fallback_error(
    endpoint: &str,
    output: &GhCommandOutput,
    draft_error: GitHubReleaseMetadataError,
) -> GitHubReleaseMetadataError {
    let mut diagnostics = vec![gh_failure_diagnostic(
        "gh api release metadata",
        endpoint,
        output,
    )];
    diagnostics.extend(draft_error.diagnostics);
    GitHubReleaseMetadataError {
        message: format!(
            "{}; the draft fallback also failed: {}",
            gh_failure_detail("gh api release metadata", output),
            draft_error.message
        ),
        diagnostics,
    }
}

/// Resolve a draft release by scanning the list endpoint, which -- unlike
/// `releases/tags/{tag}` -- includes drafts. Returns the REST shape, so the
/// asset digests recovery depends on are preserved.
///
/// The error distinguishes the three ways this can fail -- the `gh` invocation
/// itself failed, no release matched the tag, or the matched record was not
/// parseable release metadata -- because they demand different responses and
/// were previously indistinguishable (all collapsed into `None`).
fn gh_draft_release_metadata(
    github: &GitHubRepo,
    config: &GithubConfig,
    tag: &str,
    repo_flag: &str,
) -> Result<GitHubReleaseMetadata, GitHubReleaseMetadataError> {
    let endpoint = format!("repos/{repo_flag}/releases");
    let filter = format!(".[] | select(.tag_name == \"{tag}\")");
    let output = run_gh_command(
        gh_command(
            github,
            config,
            &["api", "--paginate", &endpoint, "--jq", &filter],
        ),
        github_release_upload_timeout(),
    );
    if output.timed_out || output.exit_code != Some(0) {
        return Err(GitHubReleaseMetadataError {
            message: gh_failure_detail("gh api releases list", &output),
            diagnostics: vec![gh_failure_diagnostic(
                "gh api releases list",
                &endpoint,
                &output,
            )],
        });
    }
    parse_listed_release_metadata(&output.stdout).ok_or_else(|| {
        let malformed = !output.stdout.trim().is_empty();
        GitHubReleaseMetadataError {
            message: if malformed {
                format!(
                    "the release list entry for {tag} on {repo_flag} was not valid release metadata"
                )
            } else {
                format!("no GitHub Release (published or draft) on {repo_flag} matched tag {tag}")
            },
            diagnostics: malformed
                .then(|| gh_failure_diagnostic("gh api releases list", &endpoint, &output))
                .into_iter()
                .collect(),
        }
    })
}

/// `gh api --jq` streams one JSON object per match rather than an array, and
/// `--paginate` concatenates pages. A tag resolves to at most one release, so
/// take the first non-empty record.
pub(crate) fn parse_listed_release_metadata(stdout: &str) -> Option<GitHubReleaseMetadata> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| serde_json::from_str(line).ok())
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
            if file_sha256(path)? != digest {
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

pub(crate) fn download_small_release_asset(
    github: &GitHubRepo,
    config: &GithubConfig,
    repo_flag: &str,
    asset: &GitHubReleaseAsset,
) -> Result<String, String> {
    const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
    if asset.size == 0 || asset.size > MAX_CHECKSUM_BYTES {
        return Err(format!(
            "checksum asset '{}' exceeds the 64 KiB adoption limit",
            asset.name
        ));
    }
    let id = asset
        .id
        .ok_or_else(|| format!("checksum asset '{}' has no GitHub asset ID", asset.name))?;
    let endpoint = format!("repos/{repo_flag}/releases/assets/{id}");
    let output = run_gh_command(
        gh_command(
            github,
            config,
            &["api", &endpoint, "-H", "Accept: application/octet-stream"],
        ),
        github_release_upload_timeout(),
    );
    if output.timed_out
        || output.exit_code != Some(0)
        || output.stdout.len() as u64 != asset.size
        || output.stdout.len() as u64 > MAX_CHECKSUM_BYTES
    {
        return Err(format!(
            "could not download bounded checksum asset '{}'",
            asset.name
        ));
    }
    Ok(output.stdout)
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
    let mut output_file = download.reopen().map_err(|error| {
        format!(
            "could not open temporary file for GitHub Release asset '{}': {error}",
            asset_name
        )
    })?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut containment =
        homeboy_core::process::ProcessContainment::prepare(command).map_err(|error| {
            format!(
                "could not contain GitHub Release asset '{}' download: {error}",
                asset_name
            )
        })?;
    let mut child = command.spawn().map_err(|error| {
        format!(
            "could not download GitHub Release asset '{}': {error}",
            asset_name
        )
    })?;
    if let Err(error) = containment.attach(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "could not contain GitHub Release asset '{}' download: {error}",
            asset_name
        ));
    }
    let mut stdout = child.stdout.take().ok_or_else(|| {
        format!(
            "could not capture download stream for GitHub Release asset '{}'",
            asset_name
        )
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        format!(
            "could not capture download diagnostics for GitHub Release asset '{}'",
            asset_name
        )
    })?;
    let started = Instant::now();
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut diagnostics = Vec::new();
    let mut diagnostics_truncated = false;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;
    let mut exited_at = None;
    let outcome = loop {
        if stdout_open {
            stdout_open = match drain_available_pipe(
                &mut stdout,
                GITHUB_RELEASE_DOWNLOAD_PIPE_CHUNKS_PER_TURN,
                |bytes| {
                    let remaining =
                        expected_size.saturating_sub(size).min(bytes.len() as u64) as usize;
                    let accepted = bytes.len().min(remaining);
                    if accepted > 0 {
                        output_file.write_all(&bytes[..accepted]).map_err(|error| {
                            format!("could not write temporary asset bytes: {error}")
                        })?;
                        hasher.update(&bytes[..accepted]);
                        size += accepted as u64;
                    }
                    if accepted != bytes.len() {
                        return Err(format!(
                            "download exceeded expected byte length {expected_size}"
                        ));
                    }
                    Ok(())
                },
            ) {
                Ok(open) => open,
                Err(error) => {
                    break Err(format!(
                        "could not download GitHub Release asset '{}': {error}",
                        asset_name
                    ))
                }
            };
        }
        if stderr_open {
            stderr_open = match drain_available_pipe(
                &mut stderr,
                GITHUB_RELEASE_DOWNLOAD_PIPE_CHUNKS_PER_TURN,
                |bytes| {
                    let remaining =
                        GITHUB_RELEASE_DOWNLOAD_DIAGNOSTIC_BYTES.saturating_sub(diagnostics.len());
                    diagnostics.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
                    diagnostics_truncated |= bytes.len() > remaining;
                    Ok(())
                },
            ) {
                Ok(open) => open,
                Err(error) => {
                    break Err(format!(
                        "could not read GitHub Release asset '{}' diagnostics: {error}",
                        asset_name
                    ))
                }
            };
        }
        if !stdout_open && !stderr_open {
            if let Some(status) = status {
                break Ok(status);
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    status = Some(exit_status);
                    exited_at = Some(Instant::now());
                }
                Ok(None) if started.elapsed() >= timeout => {
                    break Err(format!(
                        "download of GitHub Release asset '{}' timed out",
                        asset_name
                    ));
                }
                Ok(None) => {}
                Err(error) => {
                    break Err(format!(
                        "could not monitor download of GitHub Release asset '{}': {error}",
                        asset_name
                    ));
                }
            }
        } else if exited_at
            .is_some_and(|exit| exit.elapsed() >= GITHUB_RELEASE_DOWNLOAD_READER_CLEANUP_TIMEOUT)
        {
            break Err(format!(
                "GitHub Release asset '{}' pipes remained open after the download process exited",
                asset_name
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = match outcome {
        Ok(status) => status,
        Err(error) => {
            let cleanup =
                terminate_asset_download_child(&mut child, &mut containment, status.is_some())
                    .err()
                    .map(|cleanup| format!("; {cleanup}"))
                    .unwrap_or_default();
            return Err(format!("{error}{cleanup}"));
        }
    };
    containment
        .cleanup_after_leader_exit_bounded(GITHUB_RELEASE_DOWNLOAD_READER_CLEANUP_TIMEOUT)
        .map_err(|error| {
            format!(
                "could not clean up GitHub Release asset '{}' download containment: {error}",
                asset_name
            )
        })?;
    let mut diagnostics = String::from_utf8_lossy(&diagnostics).to_string();
    if diagnostics_truncated {
        diagnostics.push_str("\n[diagnostics truncated]");
    }
    if !status.success() {
        return Err(format!(
            "could not download GitHub Release asset '{}': {}",
            asset_name,
            diagnostics.trim()
        ));
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn terminate_asset_download_child(
    child: &mut std::process::Child,
    containment: &mut homeboy_core::process::ProcessContainment,
    leader_has_exited: bool,
) -> Result<(), String> {
    let pid = child.id();
    let started = Instant::now();
    let tree_result = containment.terminate_on_failure_bounded(
        GITHUB_RELEASE_DOWNLOAD_READER_CLEANUP_TIMEOUT,
        leader_has_exited,
    );
    let _ = child.kill();
    let deadline = started + GITHUB_RELEASE_DOWNLOAD_READER_CLEANUP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                return Err(format!(
                    "download process {pid} did not exit within {} ms",
                    GITHUB_RELEASE_DOWNLOAD_READER_CLEANUP_TIMEOUT.as_millis()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(format!("could not reap download process {pid}: {error}")),
        }
    }
    tree_result.map_err(|error| error.to_string())
}

#[cfg(unix)]
fn drain_available_pipe<R: Read + std::os::fd::AsRawFd>(
    input: &mut R,
    max_chunks: usize,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<bool, String> {
    let mut buffer = [0_u8; 8192];
    for _ in 0..max_chunks {
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("could not poll download pipe: {error}"));
        }
        if ready == 0 {
            return Ok(true);
        }
        match input.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(read) => consume(&buffer[..read])?,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("could not read download pipe: {error}")),
        }
    }
    Ok(true)
}

#[cfg(windows)]
fn drain_available_pipe<R: Read + std::os::windows::io::AsRawHandle>(
    input: &mut R,
    max_chunks: usize,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<bool, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let handle = input.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    if unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
        return Err("download output handle is not a pipe".to_string());
    }
    let mut buffer = [0_u8; 8192];
    for _ in 0..max_chunks {
        let mut available = 0;
        let peeked = unsafe {
            PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            let error = std::io::Error::last_os_error();
            if available == 0 && matches!(error.raw_os_error(), Some(109 | 233)) {
                return Ok(false);
            }
            return Err(format!("could not inspect download pipe: {error}"));
        }
        if available == 0 {
            return Ok(true);
        }
        let read_size = buffer.len().min(available as usize);
        match input.read(&mut buffer[..read_size]) {
            Ok(0) => return Ok(false),
            Ok(read) => consume(&buffer[..read])?,
            Err(error) => return Err(format!("could not read download pipe: {error}")),
        }
    }
    Ok(true)
}

#[cfg(all(not(unix), not(windows)))]
fn drain_available_pipe<R: Read>(
    _input: &mut R,
    _max_chunks: usize,
    _consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<bool, String> {
    Err("nonblocking download pipes are unsupported on this platform".to_string())
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

pub(crate) fn validate_draft_adoption(
    tag: &str,
    expected_names: &[String],
    metadata: &GitHubReleaseMetadata,
    sidecars: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !metadata.is_draft || metadata.tag_name != tag {
        return Err("draft adoption requires the matching unpublished GitHub Release".to_string());
    }
    let expected = expected_names
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let actual = metadata
        .assets
        .iter()
        .map(|asset| asset.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if expected != actual || actual.len() != metadata.assets.len() {
        // Name the difference. Recovery is a ~40 minute rebuild cycle, so an
        // inventory mismatch reported only as "does not match" costs a full
        // release cycle per diagnostic guess — that is exactly how #10547
        // (cargo-dist's unlisted `dist-manifest.json`) stayed hidden across
        // repeated recovery attempts of v0.321.1.
        let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
        let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
        let mut seen = std::collections::BTreeSet::new();
        let duplicated = metadata
            .assets
            .iter()
            .filter(|asset| !seen.insert(asset.name.as_str()))
            .map(|asset| asset.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut detail = Vec::new();
        if !missing.is_empty() {
            detail.push(format!("expected but absent: {}", missing.join(", ")));
        }
        if !unexpected.is_empty() {
            detail.push(format!("present but unexpected: {}", unexpected.join(", ")));
        }
        if !duplicated.is_empty() {
            detail.push(format!(
                "duplicated on the release: {}",
                duplicated.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if detail.is_empty() {
            detail.push("inventory differs in multiplicity only".to_string());
        }
        return Err(format!(
            "draft adoption asset inventory does not exactly match the manifest ({} expected, {} on the release): {}",
            expected.len(),
            metadata.assets.len(),
            detail.join("; ")
        ));
    }
    for asset in &metadata.assets {
        if asset.state != "uploaded" || asset.size == 0 || canonical_remote_digest(asset)?.is_none()
        {
            return Err(format!(
                "draft adoption asset '{}' is not an uploaded non-empty SHA-256 GitHub asset",
                asset.name
            ));
        }
    }
    let payloads = expected
        .iter()
        .filter(|name| !name.ends_with(".sha256") && *name != "sha256.sum")
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if payloads.is_empty() || sidecars.is_empty() {
        return Err(
            "draft adoption requires at least one checksum contract and payload".to_string(),
        );
    }
    let mut references = BTreeMap::new();
    for (sidecar, contents) in sidecars {
        if !expected.contains(sidecar) {
            return Err(format!(
                "checksum sidecar '{sidecar}' is not an expected asset"
            ));
        }
        let mut sidecar_references = 0;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.split_whitespace();
            let digest = fields
                .next()
                .ok_or_else(|| format!("malformed checksum in {sidecar}"))?;
            let name = fields
                .next()
                .ok_or_else(|| format!("malformed checksum in {sidecar}"))?
                .trim_start_matches('*');
            let digest = digest.to_ascii_lowercase();
            if fields.next().is_some()
                || digest.len() != 64
                || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !payloads.contains(name)
                || (sidecar.ends_with(".sha256") && name != sidecar.trim_end_matches(".sha256"))
            {
                return Err(format!("invalid checksum contract in {sidecar}"));
            }
            if references
                .insert(name.to_string(), digest.clone())
                .is_some_and(|existing| existing != digest)
            {
                return Err(format!("inconsistent checksum contract in {sidecar}"));
            }
            sidecar_references += 1;
        }
        if sidecar_references == 0 || (sidecar.ends_with(".sha256") && sidecar_references != 1) {
            return Err(format!("incomplete checksum contract in {sidecar}"));
        }
    }
    for asset in &metadata.assets {
        if let Some(expected_digest) = references.get(&asset.name) {
            if canonical_remote_digest(asset)?.as_deref()
                != Some(&format!("sha256:{expected_digest}"))
            {
                return Err(format!(
                    "checksum contract does not match GitHub digest for '{}'",
                    asset.name
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

pub(crate) fn gh_failure_diagnostic(
    operation: &str,
    endpoint: &str,
    output: &GhCommandOutput,
) -> GitHubCommandFailureDiagnostic {
    let stdout = gh_diagnostic_text(&output.stdout);
    let stderr = gh_diagnostic_text(&output.stderr);
    let http_status = parse_http_status(&stderr).or_else(|| parse_http_status(&stdout));
    let github_request_id =
        parse_github_request_id(&stderr).or_else(|| parse_github_request_id(&stdout));
    GitHubCommandFailureDiagnostic {
        operation: gh_diagnostic_text(operation),
        endpoint: gh_diagnostic_text(endpoint),
        exit_code: output.exit_code,
        timed_out: output.timed_out,
        summary: gh_failure_summary(operation, output, http_status, github_request_id.as_deref()),
        http_status,
        github_request_id,
        stdout,
        stderr,
    }
}

pub(crate) fn gh_diagnostic_text(value: &str) -> String {
    let policy = RedactionPolicy::default()
        .with_sensitive_key("signature")
        .with_sensitive_key("sig");
    let redacted = serde_json::from_str(value)
        .map(|json| policy.redact_json(&json).to_string())
        .unwrap_or_else(|_| policy.redact_env_value(value));
    bound_diagnostic_text(&redacted)
}

fn gh_failure_summary(
    operation: &str,
    output: &GhCommandOutput,
    http_status: Option<u16>,
    github_request_id: Option<&str>,
) -> String {
    let mut evidence = Vec::new();
    if let Some(status) = http_status {
        evidence.push(format!("HTTP {status}"));
    }
    if let Some(request_id) = github_request_id {
        evidence.push(format!("GitHub request ID {request_id}"));
    }
    let detail = gh_failure_detail(operation, output);
    if evidence.is_empty() {
        bound_diagnostic_text(&detail)
    } else {
        bound_diagnostic_text(&format!("{detail} ({})", evidence.join(", ")))
    }
}

fn bound_diagnostic_text(value: &str) -> String {
    bound_text(value, GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT)
}

fn bound_command_output(value: &str) -> String {
    bound_text(value, GITHUB_COMMAND_OUTPUT_LIMIT)
}

fn bound_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &value[..end])
}

fn parse_http_status(value: &str) -> Option<u16> {
    for marker in ["HTTP ", "HTTP/1.1 ", "HTTP/2 ", "\"status\":"] {
        let mut start = 0;
        while let Some(offset) = value[start..].find(marker) {
            let code_start = start + offset + marker.len();
            let code_start = code_start
                + value[code_start..]
                    .chars()
                    .take_while(|character| character.is_whitespace())
                    .map(char::len_utf8)
                    .sum::<usize>();
            if let Some(code) = value.get(code_start..code_start + 3) {
                if let Ok(code) = code.parse() {
                    return Some(code);
                }
            }
            start = code_start;
        }
    }
    None
}

fn parse_github_request_id(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    for marker in [
        "x-github-request-id:",
        "x-github-request-id=",
        "\"request_id\":\"",
    ] {
        if let Some(offset) = lower.find(marker) {
            let start = offset + marker.len();
            let start = start
                + value[start..]
                    .chars()
                    .take_while(|character| character.is_whitespace())
                    .map(char::len_utf8)
                    .sum::<usize>();
            let end = value[start..]
                .find(|character: char| matches!(character, '\n' | '\r' | ' ' | '"' | ','))
                .map(|offset| start + offset)
                .unwrap_or(value.len());
            if start < end {
                return Some(bound_diagnostic_text(&value[start..end]));
            }
        }
    }
    None
}

pub(crate) fn run_gh_command(mut command: Command, timeout: Duration) -> GhCommandOutput {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_tree(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return GhCommandOutput {
                stdout: String::new(),
                stderr: gh_diagnostic_text(&error.to_string()),
                exit_code: None,
                timed_out: false,
            }
        }
    };
    match wait_with_bounded_output_supervised(
        &mut child,
        GITHUB_COMMAND_OUTPUT_LIMIT,
        timeout,
        timeout,
        || false,
        |_, _| Ok(()),
    ) {
        Ok(supervised) => GhCommandOutput {
            stdout: bound_command_output(&String::from_utf8_lossy(&supervised.output.stdout)),
            stderr: bound_command_output(&String::from_utf8_lossy(&supervised.output.stderr)),
            exit_code: supervised.output.status.code(),
            timed_out: supervised.termination == SupervisedCommandTermination::TimedOut,
        },
        Err(error) => GhCommandOutput {
            stdout: String::new(),
            stderr: gh_diagnostic_text(&format!("could not supervise gh command: {error}")),
            exit_code: None,
            timed_out: false,
        },
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
    use std::sync::mpsc;

    /// The list endpoint is the only REST surface that exposes a draft release:
    /// `releases/tags/{tag}` returns 404 for drafts, which deadlocked recovery
    /// (a failed publish leaves a draft, every retry 404'd, it stayed a draft).
    /// This is a real `gh api --paginate --jq` record for a stranded release.
    #[test]
    fn draft_release_metadata_parses_from_the_list_endpoint() {
        let stdout = r#"{"id":360531007,"tag_name":"v0.320.0","draft":true,"assets":[{"id":1,"name":"homeboy-x86_64-unknown-linux-gnu.tar.xz","size":9876543,"digest":"sha256:abc123"}]}"#;
        let metadata = parse_listed_release_metadata(stdout).expect("draft metadata parses");
        assert!(metadata.is_draft);
        assert_eq!(metadata.assets.len(), 1);
        // Recovery reconciles by digest, so losing it would silently re-upload.
        assert_eq!(metadata.assets[0].digest.as_deref(), Some("sha256:abc123"));
    }

    #[test]
    fn listed_release_metadata_ignores_blank_and_paginated_noise() {
        let stdout = "\n\n{\"tag_name\":\"v1.2.3\",\"draft\":false,\"assets\":[]}\n";
        let metadata = parse_listed_release_metadata(stdout).expect("metadata parses");
        assert!(!metadata.is_draft);
        assert!(parse_listed_release_metadata("").is_none());
        assert!(parse_listed_release_metadata("not json").is_none());
    }

    fn failed_output(
        stdout: &str,
        stderr: &str,
        exit_code: Option<i32>,
        timed_out: bool,
    ) -> GhCommandOutput {
        GhCommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code,
            timed_out,
        }
    }

    #[test]
    fn metadata_fallback_retains_primary_404_and_draft_list_failure() {
        let primary = failed_output("", "HTTP 404: Not Found", Some(1), false);
        let draft = GitHubReleaseMetadataError {
            message: "gh api releases list exited with status 1".to_string(),
            diagnostics: vec![gh_failure_diagnostic(
                "gh api releases list",
                "repos/example/repo/releases",
                &failed_output("", "HTTP 403: forbidden", Some(1), false),
            )],
        };

        let error = metadata_fallback_error("repos/example/repo/releases/tags/v1", &primary, draft);

        assert!(error.message.contains("draft fallback also failed"));
        assert_eq!(error.diagnostics.len(), 2);
        assert_eq!(error.diagnostics[0].http_status, Some(404));
        assert_eq!(error.diagnostics[1].http_status, Some(403));
    }

    #[test]
    fn failure_diagnostic_extracts_403_request_id_and_timeout_without_dangling_detail() {
        let output = failed_output(
            "",
            "HTTP 403: forbidden\nX-GitHub-Request-Id: AB12:CD34\nAuthorization: Bearer secret",
            Some(1),
            false,
        );
        let diagnostic = gh_failure_diagnostic(
            "gh api releases list",
            "repos/example/repo/releases",
            &output,
        );
        assert_eq!(diagnostic.http_status, Some(403));
        assert_eq!(diagnostic.github_request_id.as_deref(), Some("AB12:CD34"));
        assert_eq!(
            diagnostic.summary,
            "gh api releases list exited with status 1 (HTTP 403, GitHub request ID AB12:CD34)"
        );
        assert!(!diagnostic.stderr.contains("secret"));

        let timeout = gh_failure_diagnostic(
            "gh release upload",
            "repos/example/repo/releases/v1/assets",
            &failed_output("", "", Some(124), true),
        );
        assert_eq!(timeout.summary, "gh release upload timed out");
        assert!(timeout.stderr.is_empty());
    }

    #[test]
    fn failure_diagnostic_parses_whitespace_json_status_and_redacts_url_userinfo() {
        let diagnostic = gh_failure_diagnostic(
            "gh api",
            "https://user:password@example.test/repos/example/repo",
            &failed_output("{\"status\": 403}", "", Some(1), false),
        );
        assert_eq!(diagnostic.http_status, Some(403));
        assert!(diagnostic.summary.contains("HTTP 403"));
        assert!(!diagnostic.endpoint.contains("user:password"));
        assert!(!diagnostic.endpoint.contains("password"));
    }

    #[test]
    fn failure_diagnostic_redacts_signed_url_secrets_and_bounds_output() {
        let output = failed_output(
            &format!(
                "{} https://example.test/file?X-Amz-Signature=secret&token=also-secret",
                "x".repeat(5000)
            ),
            "",
            Some(1),
            false,
        );
        let diagnostic = gh_failure_diagnostic(
            "gh api",
            "repos/example/repo/releases?access_token=secret",
            &output,
        );
        assert!(!diagnostic.stdout.contains("secret"));
        assert!(!diagnostic.stdout.contains("also-secret"));
        assert!(!diagnostic.endpoint.contains("secret"));
        assert!(diagnostic.stdout.len() <= GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT + 16);
        assert!(diagnostic.stdout.ends_with("...[truncated]"));
    }

    #[test]
    fn diagnostic_text_redacts_json_secret_values() {
        let redacted = gh_diagnostic_text(
            r#"{"access_token":"secret","token": "escaped\\u0073ecret","secret":"another-secret"}"#,
        );
        assert!(redacted.contains(r#""access_token":"[REDACTED]""#));
        assert!(redacted.contains(r#""token":"[REDACTED]""#));
        assert!(redacted.contains(r#""secret":"[REDACTED]""#));
        assert!(!redacted.contains("another-secret"));
        assert!(!redacted.contains("escaped"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn diagnostic_text_redacts_sensitive_headers_and_signed_urls() {
        let redacted = gh_diagnostic_text(
            "Cookie: session=secret\nSet-Cookie: sid=secret\nProxy-Authorization: Basic secret\nAuthorization: token ghp_secret\nX-Api-Key: secret\nhttps://user:password@example.test/path?X-Amz-Signature=secret&token=secret",
        );
        for secret in [
            "session=secret",
            "sid=secret",
            "Basic secret",
            "ghp_secret",
            "password",
            "token=secret",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn gh_spawn_failure_is_bounded_and_diagnostic_ready() {
        let output = run_gh_command(
            Command::new("homeboy-gh-does-not-exist"),
            Duration::from_secs(1),
        );
        let diagnostic = gh_failure_diagnostic(
            "gh api releases list",
            "repos/example/repo/releases",
            &output,
        );
        assert_eq!(diagnostic.exit_code, None);
        assert_eq!(diagnostic.operation, "gh api releases list");
        assert_eq!(diagnostic.endpoint, "repos/example/repo/releases");
        assert!(diagnostic.stderr.len() <= GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT);
    }

    fn remote_asset(name: &str, size: u64, digest: Option<String>) -> GitHubReleaseAsset {
        GitHubReleaseAsset {
            id: Some(123),
            name: name.to_string(),
            size,
            state: "uploaded".to_string(),
            digest,
        }
    }

    #[test]
    fn draft_adoption_requires_exact_uploaded_digest_checked_inventory_and_complete_checksums() {
        let digest = "a".repeat(64);
        let metadata = GitHubReleaseMetadata {
            tag_name: "v1.2.3".to_string(),
            is_draft: true,
            assets: vec![
                remote_asset("app.zip", 1, Some(format!("sha256:{digest}"))),
                remote_asset(
                    "app.zip.sha256",
                    70,
                    Some(format!("sha256:{}", "b".repeat(64))),
                ),
            ],
        };
        let expected = vec!["app.zip".to_string(), "app.zip.sha256".to_string()];
        let mut sidecars = BTreeMap::new();
        sidecars.insert("app.zip.sha256".to_string(), format!("{digest}  app.zip\n"));
        validate_draft_adoption("v1.2.3", &expected, &metadata, &sidecars).expect("valid adoption");

        let mut extra = metadata.clone();
        extra
            .assets
            .push(remote_asset("extra", 1, Some(format!("sha256:{digest}"))));
        assert!(validate_draft_adoption("v1.2.3", &expected, &extra, &sidecars).is_err());
        let mut missing = metadata.clone();
        missing.assets.pop();
        assert!(validate_draft_adoption("v1.2.3", &expected, &missing, &sidecars).is_err());
        let mut duplicate = metadata.clone();
        duplicate
            .assets
            .push(remote_asset("app.zip", 1, Some(format!("sha256:{digest}"))));
        assert!(validate_draft_adoption("v1.2.3", &expected, &duplicate, &sidecars).is_err());
        assert!(validate_draft_adoption("v9.9.9", &expected, &metadata, &sidecars).is_err());
        assert!(validate_draft_adoption("v1.2.3", &expected, &metadata, &BTreeMap::new()).is_err());
        sidecars.insert(
            "app.zip.sha256".to_string(),
            format!("{}  unknown.zip\n", "c".repeat(64)),
        );
        assert!(validate_draft_adoption("v1.2.3", &expected, &metadata, &sidecars).is_err());
    }

    /// Regression for #10519/#10547: an inventory mismatch must name the
    /// differing assets. Recovery is a full rebuild cycle, so a bare "does not
    /// match" forces operators to spend one ~40 minute release run per guess.
    #[test]
    fn draft_adoption_inventory_mismatch_names_the_differing_assets() {
        let digest = "a".repeat(64);
        let metadata = GitHubReleaseMetadata {
            tag_name: "v1.2.3".to_string(),
            is_draft: true,
            assets: vec![
                remote_asset("app.zip", 1, Some(format!("sha256:{digest}"))),
                remote_asset(
                    "app.zip.sha256",
                    70,
                    Some(format!("sha256:{}", "b".repeat(64))),
                ),
                // cargo-dist publishes this but never lists it in the plan
                // manifest's `.releases[].artifacts[]` (#10547).
                remote_asset(
                    "dist-manifest.json",
                    12,
                    Some(format!("sha256:{}", "c".repeat(64))),
                ),
            ],
        };
        let expected = vec!["app.zip".to_string(), "app.zip.sha256".to_string()];
        let mut sidecars = BTreeMap::new();
        sidecars.insert("app.zip.sha256".to_string(), format!("{digest}  app.zip\n"));

        let error = validate_draft_adoption("v1.2.3", &expected, &metadata, &sidecars)
            .expect_err("unexpected remote asset must fail adoption");
        assert!(
            error.contains("dist-manifest.json"),
            "mismatch must name the unexpected asset, got: {error}"
        );
        assert!(
            error.contains("present but unexpected"),
            "mismatch must say which side the asset is on, got: {error}"
        );
        assert!(
            error.contains("2 expected") && error.contains("3 on the release"),
            "mismatch must report both counts, got: {error}"
        );

        // The inverse direction: an asset the manifest expects but the draft
        // never received.
        let mut short = metadata.clone();
        short.assets.retain(|asset| asset.name != "app.zip.sha256");
        let error = validate_draft_adoption("v1.2.3", &expected, &short, &sidecars)
            .expect_err("missing remote asset must fail adoption");
        assert!(
            error.contains("expected but absent") && error.contains("app.zip.sha256"),
            "mismatch must name the absent asset, got: {error}"
        );

        // Duplicate names collapse in a set comparison, so multiplicity needs
        // its own report or the error would be empty of detail.
        let mut duplicate = GitHubReleaseMetadata {
            tag_name: "v1.2.3".to_string(),
            is_draft: true,
            assets: metadata.assets[..2].to_vec(),
        };
        duplicate
            .assets
            .push(remote_asset("app.zip", 1, Some(format!("sha256:{digest}"))));
        let error = validate_draft_adoption("v1.2.3", &expected, &duplicate, &sidecars)
            .expect_err("duplicated remote asset must fail adoption");
        assert!(
            error.contains("duplicated on the release") && error.contains("app.zip"),
            "mismatch must report duplicates, got: {error}"
        );
    }

    #[test]
    fn draft_adoption_accepts_consistent_individual_and_aggregate_checksums() {
        let app_digest = "a".repeat(64);
        let source_digest = "b".repeat(64);
        let metadata = GitHubReleaseMetadata {
            tag_name: "v1.2.3".to_string(),
            is_draft: true,
            assets: vec![
                remote_asset("app.zip", 1, Some(format!("sha256:{app_digest}"))),
                remote_asset("source.tar.gz", 1, Some(format!("sha256:{source_digest}"))),
                remote_asset(
                    "dist-manifest.json",
                    1,
                    Some(format!("sha256:{}", "c".repeat(64))),
                ),
                remote_asset(
                    "app.zip.sha256",
                    1,
                    Some(format!("sha256:{}", "d".repeat(64))),
                ),
                remote_asset(
                    "source.tar.gz.sha256",
                    1,
                    Some(format!("sha256:{}", "e".repeat(64))),
                ),
                remote_asset("sha256.sum", 1, Some(format!("sha256:{}", "f".repeat(64)))),
            ],
        };
        let expected = metadata
            .assets
            .iter()
            .map(|asset| asset.name.clone())
            .collect::<Vec<_>>();
        let mut sidecars = BTreeMap::from([
            (
                "app.zip.sha256".to_string(),
                format!("{app_digest}  app.zip\n"),
            ),
            (
                "source.tar.gz.sha256".to_string(),
                format!("{source_digest}  source.tar.gz\n"),
            ),
            (
                "sha256.sum".to_string(),
                format!("{app_digest}  app.zip\n{source_digest}  source.tar.gz\n"),
            ),
        ]);

        validate_draft_adoption("v1.2.3", &expected, &metadata, &sidecars)
            .expect("matching duplicate references are valid");

        sidecars.insert(
            "sha256.sum".to_string(),
            format!(
                "{}  app.zip\n{source_digest}  source.tar.gz\n",
                "0".repeat(64)
            ),
        );
        assert!(validate_draft_adoption("v1.2.3", &expected, &metadata, &sidecars).is_err());
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
    fn bounded_command_retains_large_release_metadata_but_diagnostics_stay_compact() {
        let assets = (0..200)
            .map(|index| {
                serde_json::json!({
                    "id": index,
                    "name": format!("release-asset-{index:03}.tar.xz"),
                    "size": 1,
                    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                })
            })
            .collect::<Vec<_>>();
        let metadata = serde_json::json!({
            "tag_name": "v1.2.3",
            "draft": true,
            "assets": assets,
        })
        .to_string();
        assert!(metadata.len() > GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT);

        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s' \"$1\"", "sh", &metadata]);
        let output = run_gh_command(command, Duration::from_secs(1));

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.len() > GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT);
        assert!(output.stdout.len() <= GITHUB_COMMAND_OUTPUT_LIMIT);
        assert_eq!(
            serde_json::from_str::<GitHubReleaseMetadata>(&output.stdout)
                .expect("large metadata remains parseable")
                .assets
                .len(),
            200
        );
        assert_eq!(
            parse_listed_release_metadata(&output.stdout)
                .expect("large draft-list record remains parseable")
                .assets
                .len(),
            200
        );

        let diagnostic =
            gh_failure_diagnostic("gh api release metadata", "repos/example/repo", &output);
        assert!(diagnostic.stdout.len() <= GITHUB_COMMAND_DIAGNOSTIC_TEXT_LIMIT + 16);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_times_out_with_continuous_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "(while :; do printf x; done) & wait"]);
        let output = run_gh_command(command, Duration::from_millis(30));
        assert!(output.timed_out);
        assert!(output.stdout.len() <= GITHUB_COMMAND_OUTPUT_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_closes_inherited_descendant_pipes() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & printf done"]);
        let started = Instant::now();
        let output = run_gh_command(command, Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(output.stdout, "done");
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
        assert!(
            !error.contains("owned process tree"),
            "reaping the direct child must not be reported as failed tree cleanup: {error}"
        );
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("read tempdir")
                .count(),
            0,
            "timed-out download must clean up its tempfile"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_asset_download_continuous_output_observes_timeout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let downloads = temp.path().join("downloads");
        std::fs::create_dir(&downloads).expect("download tempdir");
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut command = Command::new("sh");
            command.args([
                "-c",
                "(while :; do printf x; done) & (while :; do printf diagnostic >&2; done) & wait",
            ]);
            let started = Instant::now();
            let result = run_bounded_asset_download(
                &mut command,
                "asset.zip",
                u64::MAX,
                Duration::from_millis(30),
                Some(&downloads),
            );
            let _ = result_tx.send((result, started.elapsed()));
        });

        let (result, elapsed) = result_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("continuous output exceeded the external bound");
        worker.join().expect("download worker");
        let error = result.expect_err("continuous output must time out");
        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert!(
            elapsed < Duration::from_secs(2),
            "continuous output cleanup took {elapsed:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bounded_asset_download_kills_setsid_descendant_after_leader_exit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let downloads = temp.path().join("downloads");
        std::fs::create_dir(&downloads).expect("download tempdir");
        let descendant_pid = temp.path().join("descendant.pid");
        let script = format!(
            "setsid sleep 30 & echo $! > {}; exit 0",
            quote_arg(&descendant_pid.display().to_string())
        );
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut command = Command::new("sh");
            command.args(["-c", &script]);
            let result = run_bounded_asset_download(
                &mut command,
                "asset.zip",
                4,
                Duration::from_secs(5),
                Some(&downloads),
            );
            let _ = result_tx.send(result);
        });

        let error = result_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("leader-exit cleanup exceeded the external bound")
            .expect_err("inherited descendant pipes must fail cleanup");
        worker.join().expect("download worker");
        assert!(
            error.contains("pipes remained open"),
            "unexpected error: {error}"
        );
        let pid = std::fs::read_to_string(&descendant_pid)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant pid");
        assert!(
            !homeboy_core::process::pid_is_running(pid),
            "setsid descendant {pid} survived cleanup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_asset_download_timeout_closes_descendant_inherited_pipes() {
        assert_inherited_pipe_cleanup(
            "sleep 30",
            "wait",
            Duration::from_millis(50),
            "timed out",
            "timeout",
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_asset_download_oversize_closes_descendant_inherited_pipes() {
        assert_inherited_pipe_cleanup(
            "sleep 30",
            "printf 'oversized'; wait",
            Duration::from_secs(5),
            "exceeded expected byte length 4",
            "oversize",
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bounded_asset_download_timeout_kills_escaped_inherited_pipes() {
        assert_inherited_pipe_cleanup(
            "setsid sleep 30",
            "wait",
            Duration::from_millis(50),
            "timed out",
            "escaped timeout",
        );
    }

    #[cfg(unix)]
    fn assert_inherited_pipe_cleanup(
        descendant_command: &str,
        command_tail: &str,
        timeout: Duration,
        expected_error: &str,
        case: &str,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let downloads = temp.path().join("downloads");
        std::fs::create_dir(&downloads).expect("download tempdir");
        let leader_pid = temp.path().join("leader.pid");
        let descendant_pid = temp.path().join("descendant.pid");
        let script = format!(
            "echo $$ > {}; {descendant_command} & echo $! > {}; {command_tail}",
            quote_arg(&leader_pid.display().to_string()),
            quote_arg(&descendant_pid.display().to_string())
        );
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut command = Command::new("sh");
            command.args(["-c", &script]);
            let started = Instant::now();
            let result =
                run_bounded_asset_download(&mut command, "asset.zip", 4, timeout, Some(&downloads));
            let _ = result_tx.send((result, started.elapsed()));
        });

        let (result, elapsed) = result_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap_or_else(|_| panic!("{case} cleanup exceeded the external bound"));
        worker.join().expect("download worker");
        let error = result.unwrap_err();
        assert!(error.contains(expected_error), "unexpected error: {error}");
        assert!(
            elapsed < Duration::from_secs(2),
            "{case} cleanup took {elapsed:?}"
        );
        assert_fixture_processes_stopped(&leader_pid, &descendant_pid);
        assert_eq!(
            std::fs::read_dir(temp.path().join("downloads"))
                .expect("read download tempdir")
                .count(),
            0,
            "{case} inherited pipes must not retain the tempfile"
        );
    }

    #[cfg(unix)]
    fn assert_fixture_processes_stopped(
        leader_pid_path: &std::path::Path,
        descendant_pid_path: &std::path::Path,
    ) {
        let leader_pid = std::fs::read_to_string(leader_pid_path)
            .expect("leader pid")
            .trim()
            .parse::<u32>()
            .expect("numeric leader pid");
        let descendant_pid = std::fs::read_to_string(descendant_pid_path)
            .expect("descendant pid")
            .trim()
            .parse::<u32>()
            .expect("numeric descendant pid");
        for _ in 0..40 {
            if !homeboy_core::process::process_group_is_running(leader_pid as i32)
                && !homeboy_core::process::pid_is_running(descendant_pid)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("download process group {leader_pid} or descendant {descendant_pid} remained alive");
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
    fn canonical_recovery_with_ordinal_durable_path_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().join("component.zip");
        let ordinal = dir.path().join("01-component.zip");
        std::fs::write(&ordinal, b"component bytes").expect("write durable zip");
        let publications =
            github_release_publications(&release_state_with_artifacts(vec![artifact(
                &canonical,
                Some(&ordinal),
            )]))
            .expect("publication identity");
        let remote = remote_asset(
            "component.zip",
            publications[0].size,
            Some(format!("sha256:{}", publications[0].sha256)),
        );

        for _ in 0..2 {
            let (uploads, existing) = reconcile_release_publications_with(
                &publications,
                std::slice::from_ref(&remote),
                &mut unexpected_download,
            )
            .expect("existing canonical asset is reusable");
            assert!(uploads.is_empty());
            assert_eq!(existing, publications);
        }
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
    fn draft_published_during_build_reuses_verified_asset_and_uploads_only_missing_asset() {
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
        let planned = GitHubReleaseMetadata {
            tag_name: "v1.2.3".to_string(),
            is_draft: true,
            assets: Vec::new(),
        };
        assert!(planned.is_draft, "planning observed the matching draft");
        let finalizing = GitHubReleaseMetadata {
            tag_name: planned.tag_name.clone(),
            is_draft: false,
            assets: vec![remote_asset(
                "first.zip",
                publications[0].size,
                Some(format!("sha256:{}", publications[0].sha256)),
            )],
        };

        let (uploads, existing) = reconcile_release_publications_with(
            &publications,
            &finalizing.assets,
            &mut unexpected_download,
        )
        .expect("partial recovery");

        assert_eq!(finalizing.tag_name, planned.tag_name);
        assert!(!finalizing.is_draft, "finalization observed publication");
        assert_eq!(existing, vec![publications[0].clone()]);
        assert_eq!(uploads, vec![publications[1].clone()]);

        let mut completed_assets = finalizing.assets;
        completed_assets.push(remote_asset(
            "second.zip",
            publications[1].size,
            Some(format!("sha256:{}", publications[1].sha256)),
        ));
        verify_release_publications_with(
            &publications,
            &completed_assets,
            &mut unexpected_download,
        )
        .expect("the missing authoritative upload completes the published release");
    }

    #[test]
    fn draft_published_during_build_rejects_conflicting_existing_bytes() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let planned = GitHubReleaseMetadata {
            tag_name: "v1.2.3".to_string(),
            is_draft: true,
            assets: Vec::new(),
        };
        let finalizing = GitHubReleaseMetadata {
            tag_name: planned.tag_name.clone(),
            is_draft: false,
            assets: vec![remote_asset(
                "component.zip",
                publication.size,
                Some(format!("sha256:{}", "b".repeat(64))),
            )],
        };

        let error = reconcile_release_publications_with(
            &[publication],
            &finalizing.assets,
            &mut unexpected_download,
        )
        .expect_err("published conflicting bytes must fail closed");

        assert_eq!(finalizing.tag_name, planned.tag_name);
        assert!(!finalizing.is_draft);
        assert_eq!(
            error,
            format!(
                "GitHub Release asset 'component.zip' conflicts with the canonical publication bytes (expected sha256:{}, found sha256:{})",
                "a".repeat(64),
                "b".repeat(64)
            )
        );
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
    fn missing_or_mismatched_remote_digest_cannot_adopt_canonical_bytes() {
        let publication = ReleaseAssetPublication {
            target_name: "component.zip".to_string(),
            sha256: "a".repeat(64),
            size: 15,
            source_path: "component.zip".to_string(),
        };
        let missing_digest = reconcile_release_publications_with(
            &[publication.clone()],
            &[remote_asset("component.zip", 15, None)],
            &mut |_, _| Ok((15, "b".repeat(64))),
        )
        .expect_err("digestless assets require matching downloaded bytes");
        assert!(missing_digest.contains("conflicts with the canonical publication bytes"));

        let mismatched_digest = reconcile_release_publications_with(
            &[publication],
            &[remote_asset(
                "component.zip",
                15,
                Some(format!("sha256:{}", "b".repeat(64))),
            )],
            &mut unexpected_download,
        )
        .expect_err("mismatched GitHub digest must fail closed");
        assert!(mismatched_digest.contains("conflicts with the canonical publication bytes"));
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
