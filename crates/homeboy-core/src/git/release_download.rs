//! Download release artifacts from GitHub for deployment.
//!
//! When a component has `remote_url` set (pointing to a GitHub repo), deploy can
//! skip local builds entirely and download the CI-built artifact from a GitHub release.
//!
//! Flow:
//! 1. Resolve the latest tag for the component
//! 2. Download the release artifact from `{remote_url}/releases/download/{tag}/{artifact}`
//! 3. Return the local path to the downloaded file for the existing upload pipeline
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/784

use std::collections::HashMap;
use std::io::{Read, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::component::{Component, GithubConfig};
use crate::error::{Error, Result};
use crate::git::github_cli_env;
use homeboy_engine_primitives::content_hash;
use serde_json::Value;

const PACKAGE_DEPENDENCY_FIELDS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

const MUTABLE_DEPENDENCY_PREFIXES: &[&str] = &[
    "file:",
    "link:",
    "git+",
    "github:",
    "git://",
    "git@",
    "ssh://git@",
    "https://github.com/",
    "http://github.com/",
];

const ZIP_MAGIC_PREFIX: &[u8] = b"PK";

/// Immutable release-asset identity verified before it is made available to deploy targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifact {
    pub path: PathBuf,
    pub tag: String,
    pub commit: Option<String>,
    pub url: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// Shared payload and runtime-temp pin retained by every lease clone.
///
/// Runtime download directories follow the existing retained-artifact policy and
/// are cleaned by the runtime-temp cleanup command, not when this lease drops.
#[derive(Debug)]
struct ReleaseArtifactLeaseInner {
    artifact: ReleaseArtifact,
    _runtime_temp_pin: crate::engine::temp::RuntimeTempPin,
}

/// Cloneable ownership of a downloaded release artifact within a deploy scope.
/// Consumers retain the verified bytes independently without invalidating another
/// consumer, and the final drop removes only the runtime cleanup pin.
#[derive(Debug, Clone)]
pub struct ReleaseArtifactLease(Arc<ReleaseArtifactLeaseInner>);

impl ReleaseArtifactLease {
    fn new(artifact: ReleaseArtifact) -> Result<Self> {
        let directory = artifact.path.parent().ok_or_else(|| {
            Error::internal_io(
                "Downloaded release artifact has no parent directory".to_string(),
                Some(artifact.path.display().to_string()),
            )
        })?;
        let runtime_temp_pin = crate::engine::temp::pin_runtime_temp_dir(directory)?;
        Ok(Self(Arc::new(ReleaseArtifactLeaseInner {
            artifact,
            _runtime_temp_pin: runtime_temp_pin,
        })))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_new(artifact: ReleaseArtifact) -> Result<Self> {
        Self::new(artifact)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_strong_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }
}

impl Deref for ReleaseArtifactLease {
    type Target = ReleaseArtifact;

    fn deref(&self) -> &Self::Target {
        &self.0.artifact
    }
}

/// Command-scoped immutable artifact cache. A multi-target deploy resolves and
/// verifies a release asset once, then fans out independent leases for the same
/// local bytes.
#[derive(Default)]
pub struct ReleaseArtifactStore {
    artifacts: HashMap<String, ReleaseArtifactLease>,
}

impl ReleaseArtifactStore {
    pub fn resolve(
        &mut self,
        github: &GitHubRepo,
        github_config: &GithubConfig,
        tag: &str,
        artifact_name: &str,
    ) -> Result<ReleaseArtifactLease> {
        self.get_or_insert_with(github, tag, artifact_name, || {
            resolve_and_download_release_artifact(github, github_config, tag, artifact_name)
        })
    }

    pub fn get(
        &self,
        github: &GitHubRepo,
        tag: &str,
        artifact_name: &str,
    ) -> Option<ReleaseArtifactLease> {
        self.artifacts
            .get(&format!(
                "{}/{}/{}:{tag}:{artifact_name}",
                github.host, github.owner, github.repo
            ))
            .cloned()
    }

    fn get_or_insert_with(
        &mut self,
        github: &GitHubRepo,
        tag: &str,
        artifact_name: &str,
        resolve: impl FnOnce() -> Result<ReleaseArtifact>,
    ) -> Result<ReleaseArtifactLease> {
        let key = format!(
            "{}/{}/{}:{tag}:{artifact_name}",
            github.host, github.owner, github.repo
        );
        if let Some(artifact) = self.artifacts.get(&key) {
            return Ok(artifact.clone());
        }
        let artifact = ReleaseArtifactLease::new(resolve()?)?;
        self.artifacts.insert(key, artifact.clone());
        Ok(artifact)
    }
}

#[derive(Debug, Clone)]
struct ReleaseAssetMetadata {
    url: String,
    name: String,
    size: u64,
    digest: Option<String>,
    tag: String,
    commit: Option<String>,
}

/// Parsed GitHub owner/repo from a remote URL.
#[derive(Debug, Clone)]
pub struct GitHubRepo {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl GitHubRepo {
    /// Construct a release artifact download URL.
    pub fn release_artifact_url(&self, tag: &str, artifact_name: &str) -> String {
        format!(
            "https://{}/{}/{}/releases/download/{}/{}",
            self.host, self.owner, self.repo, tag, artifact_name
        )
    }

    fn release_by_tag_api_url(&self, tag: &str) -> String {
        if self.host == "github.com" {
            format!(
                "https://api.github.com/repos/{}/{}/releases/tags/{}",
                self.owner, self.repo, tag
            )
        } else {
            format!(
                "https://{}/api/v3/repos/{}/{}/releases/tags/{}",
                self.host, self.owner, self.repo, tag
            )
        }
    }

    /// The GitHub Releases API URL for the repository's latest release.
    fn latest_release_api_url(&self) -> String {
        if self.host == "github.com" {
            format!(
                "https://api.github.com/repos/{}/{}/releases/latest",
                self.owner, self.repo
            )
        } else {
            format!(
                "https://{}/api/v3/repos/{}/{}/releases/latest",
                self.host, self.owner, self.repo
            )
        }
    }

    /// Raw-content URL for a repo-relative file at a tag. Used to resolve a
    /// component's portable config (`homeboy.json`) without a local checkout
    /// (#14782).
    fn raw_content_url(&self, tag: &str, path: &str) -> String {
        if self.host == "github.com" {
            format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                self.owner, self.repo, tag, path
            )
        } else {
            // GitHub Enterprise serves raw blobs from the web host, not the API host.
            format!(
                "https://{}/{}/{}/raw/{}/{}",
                self.host, self.owner, self.repo, tag, path
            )
        }
    }

    /// The GitHub API URL for a source zipball at a tag. Every tag/release has
    /// one regardless of uploaded release assets, so this is the fallback used
    /// to read `homeboy.json` when the raw-content fetch 404s (e.g. a private
    /// repository host that does not serve raw content the same way).
    fn zipball_api_url(&self, tag: &str) -> String {
        if self.host == "github.com" {
            format!(
                "https://api.github.com/repos/{}/{}/zipball/{}",
                self.owner, self.repo, tag
            )
        } else {
            format!(
                "https://{}/api/v3/repos/{}/{}/zipball/{}",
                self.host, self.owner, self.repo, tag
            )
        }
    }
}

/// Parse owner/repo from a GitHub URL.
///
/// Supports:
/// - `https://github.com/owner/repo`
/// - `https://github.com/owner/repo.git`
/// - `https://user:token@github.com/owner/repo.git`
/// - `git@github.com:owner/repo.git`
/// - GitHub Enterprise equivalents such as `git@github.example.com:owner/repo.git`
pub fn parse_github_url(url: &str) -> Option<GitHubRepo> {
    // HTTPS format
    if let Some(repo) = parse_github_http_url(url) {
        return Some(repo);
    }

    // SSH format
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        if is_github_host(host) {
            if let Some(repo) = parse_owner_repo(host, path) {
                return Some(repo);
            }
        }
    }

    None
}

fn is_github_host(host: &str) -> bool {
    let host = host.rsplit('@').next().unwrap_or(host).trim();

    host == "github.com" || (host.starts_with("github.") && !host.starts_with("github.com."))
}

fn parse_github_http_url(url: &str) -> Option<GitHubRepo> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit('@').next()?;

    // GitHub HTTPS remotes may include credentials before the host, e.g.
    // https://x-access-token:TOKEN@github.com/owner/repo.git.
    if !is_github_host(host) {
        return None;
    }

    parse_owner_repo(host, path)
}

fn parse_owner_repo(host: &str, path: &str) -> Option<GitHubRepo> {
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let parts: Vec<&str> = path.splitn(3, '/').collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return Some(GitHubRepo {
            host: host.to_string(),
            owner: parts[0].to_string(),
            repo: parts[1].to_string(),
        });
    }

    None
}

/// Resolve the artifact filename for a component.
///
/// Uses the component's `build_artifact` field. The artifact name is the
/// filename portion (no directory path) since it's downloaded from a flat
/// GitHub release.
pub fn resolve_artifact_name(component: &Component) -> Option<String> {
    let artifact = component.build_artifact.as_ref()?;
    let path = Path::new(artifact);
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

/// Download a release artifact from GitHub to a temporary directory.
///
/// Returns the local path to the downloaded file.
pub fn download_release_artifact(
    github: &GitHubRepo,
    github_config: &GithubConfig,
    tag: &str,
    artifact_name: &str,
) -> Result<PathBuf> {
    Ok(resolve_and_download_release_artifact(github, github_config, tag, artifact_name)?.path)
}

/// Resolve authenticated GitHub Release metadata and download one verified asset.
///
/// Metadata is mandatory: direct release-download URLs cannot prove which asset was
/// selected, its expected size, or the release/tag identity.
pub fn resolve_and_download_release_artifact(
    github: &GitHubRepo,
    github_config: &GithubConfig,
    tag: &str,
    artifact_name: &str,
) -> Result<ReleaseArtifact> {
    let auth_token = github_auth_token(github, github_config).ok_or_else(|| {
        Error::validation_invalid_argument(
            "github",
            format!(
                "No GitHub authentication token is available for {}",
                github.host
            ),
            None,
            Some(vec![
                "Authenticate gh for the configured GitHub host before deploying a release asset."
                    .to_string(),
            ]),
        )
    })?;
    let release_asset =
        resolve_release_asset_metadata(github, github_config, tag, artifact_name, &auth_token)?;
    let url = release_asset.url.clone();

    // Create temp directory for the download
    let tmp_dir = crate::engine::temp::runtime_temp_dir("deploy-download")?;
    let dest_path = tmp_dir.join(artifact_name);

    log_status!("deploy", "Downloading release artifact: {}", url);

    let curl_command = curl_release_artifact_command(
        &url,
        dest_path.to_str().unwrap_or("artifact"),
        Some(&auth_token),
        github_command_env(github, github_config),
    );

    // Use curl for the download (follows redirects, handles GitHub's CDN)
    let mut command = std::process::Command::new("curl");
    command.args(&curl_command.args);
    apply_command_env(&mut command, &curl_command.env);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    if curl_command.config_stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }

    let mut child = command.spawn().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("download release artifact".to_string()),
        )
    })?;

    if let Some(config_stdin) = curl_command.config_stdin {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            Error::internal_io(
                "Failed to open curl stdin".to_string(),
                Some("download release artifact".to_string()),
            )
        })?;

        stdin.write_all(config_stdin.as_bytes()).map_err(|e| {
            Error::internal_io(
                format!("Failed to write curl config: {}", e),
                Some("download release artifact".to_string()),
            )
        })?;
    }

    let output = child.wait_with_output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("download release artifact".to_string()),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_io(
            format!(
                "Failed to download release artifact from {}: {}",
                url,
                stderr.trim()
            ),
            Some("download release artifact".to_string()),
        ));
    }

    let sha256 = verify_downloaded_release_artifact(&dest_path, &release_asset, &url)?;

    log_status!(
        "deploy",
        "Downloaded {} ({} bytes)",
        release_asset.name,
        release_asset.size
    );

    Ok(ReleaseArtifact {
        path: dest_path,
        tag: release_asset.tag,
        commit: release_asset.commit,
        url,
        name: release_asset.name,
        size: release_asset.size,
        sha256,
    })
}

fn verify_downloaded_release_artifact(
    path: &Path,
    release_asset: &ReleaseAssetMetadata,
    url: &str,
) -> Result<String> {
    let file_metadata = std::fs::metadata(path).map_err(|error| {
        Error::internal_io(
            format!("Downloaded artifact not found: {error}"),
            Some(path.display().to_string()),
        )
    })?;
    if file_metadata.len() == 0 {
        return Err(Error::validation_invalid_argument(
            "releaseArtifact",
            format!(
                "Downloaded release artifact '{}' is empty",
                release_asset.name
            ),
            Some(url.to_string()),
            None,
        ));
    }
    if file_metadata.len() != release_asset.size {
        return Err(Error::validation_invalid_argument(
            "releaseArtifact",
            format!("Downloaded release artifact '{}' has size {}, expected {} from GitHub Release metadata", release_asset.name, file_metadata.len(), release_asset.size),
            Some(url.to_string()),
            None,
        ));
    }
    validate_downloaded_artifact(path, &release_asset.name, url)?;
    let sha256 = sha256_file(path)?;
    if let Some(expected) = release_asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
    {
        if !expected.eq_ignore_ascii_case(&sha256) {
            return Err(Error::validation_invalid_argument(
                "releaseArtifact",
                format!("Downloaded release artifact '{}' SHA-256 digest does not match GitHub Release metadata", release_asset.name),
                Some(url.to_string()),
                None,
            ));
        }
    }
    Ok(sha256)
}

fn resolve_release_asset_metadata(
    github: &GitHubRepo,
    github_config: &GithubConfig,
    tag: &str,
    artifact_name: &str,
    auth_token: &str,
) -> Result<ReleaseAssetMetadata> {
    let url = github.release_by_tag_api_url(tag);
    let curl_command =
        curl_release_asset_api_command(&url, auth_token, github_command_env(github, github_config));

    let mut command = std::process::Command::new("curl");
    command.args(&curl_command.args);
    apply_command_env(&mut command, &curl_command.env);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("resolve release asset".to_string()),
        )
    })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        Error::internal_io(
            "Failed to open curl stdin".to_string(),
            Some("resolve release asset".to_string()),
        )
    })?;
    stdin
        .write_all(curl_command.config_stdin.as_bytes())
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to write curl config: {}", e),
                Some("resolve release asset".to_string()),
            )
        })?;
    drop(stdin);

    let output = child.wait_with_output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("resolve release asset".to_string()),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_io(
            format!(
                "Failed to resolve release asset '{}' from {}: {}",
                artifact_name,
                url,
                stderr.trim()
            ),
            Some("resolve release asset".to_string()),
        ));
    }

    let release: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse release metadata from {}: {}", url, e),
            Some("resolve release asset".to_string()),
        )
    })?;

    let assets = release
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::internal_json(
                "GitHub Release metadata has no assets array".to_string(),
                Some(url.clone()),
            )
        })?;
    let asset = assets
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| release_asset_name_matches(name, artifact_name))
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "releaseArtifact",
                format!(
                    "GitHub Release tag '{}' is missing asset '{}'",
                    tag, artifact_name
                ),
                Some(url.clone()),
                None,
            )
        })?;
    let name = asset.get("name").and_then(Value::as_str).ok_or_else(|| {
        Error::internal_json(
            "GitHub Release asset has no name".to_string(),
            Some(url.clone()),
        )
    })?;
    let asset_url = asset.get("url").and_then(Value::as_str).ok_or_else(|| {
        Error::internal_json(
            "GitHub Release asset has no API URL".to_string(),
            Some(url.clone()),
        )
    })?;
    let size = asset.get("size").and_then(Value::as_u64).ok_or_else(|| {
        Error::internal_json(
            "GitHub Release asset has no size".to_string(),
            Some(url.clone()),
        )
    })?;
    let release_tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .unwrap_or(tag);
    if release_tag != tag {
        return Err(Error::validation_invalid_argument(
            "releaseTag",
            format!(
                "GitHub Release metadata returned tag '{}' for requested tag '{}'",
                release_tag, tag
            ),
            Some(url),
            None,
        ));
    }
    Ok(ReleaseAssetMetadata {
        url: asset_url.to_string(),
        name: name.to_string(),
        size,
        digest: asset
            .get("digest")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        tag: release_tag.to_string(),
        commit: release
            .get("target_commitish")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    })
}

// =============================================================================
// Checkout-less resolution (#14782)
// =============================================================================
//
// A project-attached component whose deploy source resolves to a release
// asset does not need a source checkout to be resolved or deployed: its
// portable config (`homeboy.json`) and its "current" version are both
// answerable from the component's GitHub repository alone. This section adds
// the two GitHub reads that make that possible:
//
//   - `fetch_portable_config_at_tag` — the component's `homeboy.json` at a
//     tag, read from the repository rather than a working tree.
//   - `latest_release_tag_for_repo` — the repository's latest GitHub Release
//     tag, used in place of a local `git tag` read when no checkout exists.
//
// Both results are cached per (repo, tag) / per repo for the process, since a
// project can attach the same component many times and a single `--outdated`
// pass resolves every attached component.

/// Outcome of a raw HTTP GET distinguishing "not found" from "found", so
/// callers can treat a 404 as a legitimate negative result (no release yet,
/// no `homeboy.json` at this path) rather than a transport failure.
enum FetchOutcome {
    Found(Vec<u8>),
    NotFound,
}

type PortableConfigCacheKey = (String, String, String, String);

fn portable_config_cache() -> &'static Mutex<HashMap<PortableConfigCacheKey, Component>> {
    static CACHE: OnceLock<Mutex<HashMap<PortableConfigCacheKey, Component>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn portable_config_cache_key(github: &GitHubRepo, tag: &str) -> PortableConfigCacheKey {
    (
        github.host.clone(),
        github.owner.clone(),
        github.repo.clone(),
        tag.to_string(),
    )
}

type LatestReleaseTagCacheKey = (String, String, String);

fn latest_release_tag_cache() -> &'static Mutex<HashMap<LatestReleaseTagCacheKey, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<LatestReleaseTagCacheKey, Option<String>>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn latest_release_tag_cache_key(github: &GitHubRepo) -> LatestReleaseTagCacheKey {
    (
        github.host.clone(),
        github.owner.clone(),
        github.repo.clone(),
    )
}

/// Resolve a component's portable config (`homeboy.json`) at a tag directly
/// from its GitHub repository, without requiring a local checkout.
///
/// Tries the raw-content URL first (cheap, no archive download). If that 404s
/// — some GitHub Enterprise hosts don't serve raw content the same way, or the
/// file genuinely is not at the repo root — falls back to the tag's source
/// zipball, which every tag has regardless of uploaded release assets, and
/// reads `homeboy.json` from its root or exactly one level of nesting (GitHub
/// zipballs root at a generated `<owner>-<repo>-<sha>/` directory).
///
/// Cached per (host, owner, repo, tag) for the process.
pub fn fetch_portable_config_at_tag(
    github: &GitHubRepo,
    github_config: &GithubConfig,
    tag: &str,
) -> Result<Component> {
    let key = portable_config_cache_key(github, tag);
    if let Some(cached) = portable_config_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
    {
        return Ok(cached.clone());
    }

    let component = fetch_portable_config_at_tag_uncached(github, github_config, tag)?;
    portable_config_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(key, component.clone());
    Ok(component)
}

fn fetch_portable_config_at_tag_uncached(
    github: &GitHubRepo,
    github_config: &GithubConfig,
    tag: &str,
) -> Result<Component> {
    let auth_token = github_auth_token(github, github_config);
    let context_label = format!("{}/{}/{} @ {}", github.host, github.owner, github.repo, tag);
    let context = Path::new(&context_label);

    let raw_url = github.raw_content_url(tag, "homeboy.json");
    match fetch_raw_content(
        &raw_url,
        auth_token.as_deref(),
        github_command_env(github, github_config),
    )? {
        FetchOutcome::Found(bytes) => {
            return crate::component::portable::component_from_portable_bytes(&bytes, context);
        }
        FetchOutcome::NotFound => {}
    }

    let zip_bytes = download_release_zipball(github, github_config, tag)?;
    let Some(json_bytes) = extract_homeboy_json_from_zip_bytes(&zip_bytes)? else {
        return Err(Error::validation_invalid_argument(
            "homeboy.json",
            format!(
                "GitHub repository '{}/{}' has no homeboy.json at tag '{}' (checked the repo root and its source archive)",
                github.owner, github.repo, tag
            ),
            Some(context_label),
            Some(vec![
                "Add homeboy.json to the repository root so it is resolvable without a local checkout".to_string(),
            ]),
        ));
    };
    crate::component::portable::component_from_portable_bytes(&json_bytes, context)
}

/// Resolve the latest GitHub Release tag for a repository, in place of a local
/// `git tag` read when no checkout exists.
///
/// Returns `Ok(None)` when the repository has no releases yet — a legitimate
/// negative result, not a failure. Cached per (host, owner, repo) for the
/// process.
pub fn latest_release_tag_for_repo(
    github: &GitHubRepo,
    github_config: &GithubConfig,
) -> Result<Option<String>> {
    let key = latest_release_tag_cache_key(github);
    if let Some(cached) = latest_release_tag_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
    {
        return Ok(cached.clone());
    }

    let auth_token = github_auth_token(github, github_config).ok_or_else(|| {
        Error::validation_invalid_argument(
            "github",
            format!(
                "No GitHub authentication token is available for {}",
                github.host
            ),
            None,
            Some(vec![
                "Authenticate gh for the configured GitHub host before resolving a release without a local checkout."
                    .to_string(),
            ]),
        )
    })?;
    let url = github.latest_release_api_url();
    let tag = match fetch_raw_content(
        &url,
        Some(&auth_token),
        github_command_env(github, github_config),
    )? {
        FetchOutcome::NotFound => None,
        FetchOutcome::Found(bytes) => parse_latest_release_tag(&bytes)?,
    };

    latest_release_tag_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(key, tag.clone());
    Ok(tag)
}

fn parse_latest_release_tag(bytes: &[u8]) -> Result<Option<String>> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        Error::internal_json(
            format!("Failed to parse latest release metadata: {error}"),
            None,
        )
    })?;
    Ok(value
        .get("tag_name")
        .and_then(Value::as_str)
        .map(ToString::to_string))
}

fn download_release_zipball(
    github: &GitHubRepo,
    github_config: &GithubConfig,
    tag: &str,
) -> Result<Vec<u8>> {
    let auth_token = github_auth_token(github, github_config).ok_or_else(|| {
        Error::validation_invalid_argument(
            "github",
            format!(
                "No GitHub authentication token is available for {}",
                github.host
            ),
            None,
            Some(vec![
                "Authenticate gh for the configured GitHub host before resolving a release without a local checkout."
                    .to_string(),
            ]),
        )
    })?;
    let url = github.zipball_api_url(tag);

    let tmp_dir = crate::engine::temp::runtime_temp_dir("deploy-portable-config")?;
    let dest_path = tmp_dir.join(format!("{}-{}.zip", github.repo, tag));

    let curl_command = curl_release_artifact_command(
        &url,
        dest_path.to_str().unwrap_or("archive.zip"),
        Some(&auth_token),
        github_command_env(github, github_config),
    );

    let mut command = std::process::Command::new("curl");
    command.args(&curl_command.args);
    apply_command_env(&mut command, &curl_command.env);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    if curl_command.config_stdin.is_some() {
        command.stdin(std::process::Stdio::piped());
    }

    let mut child = command.spawn().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("download release source archive".to_string()),
        )
    })?;
    if let Some(config_stdin) = curl_command.config_stdin {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            Error::internal_io(
                "Failed to open curl stdin".to_string(),
                Some("download release source archive".to_string()),
            )
        })?;
        stdin.write_all(config_stdin.as_bytes()).map_err(|e| {
            Error::internal_io(
                format!("Failed to write curl config: {}", e),
                Some("download release source archive".to_string()),
            )
        })?;
    }
    let output = child.wait_with_output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("download release source archive".to_string()),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_io(
            format!(
                "Failed to download release source archive from {}: {}",
                url,
                stderr.trim()
            ),
            Some("download release source archive".to_string()),
        ));
    }

    std::fs::read(&dest_path).map_err(|e| {
        Error::internal_io(
            format!("Failed to read downloaded source archive: {}", e),
            Some(dest_path.display().to_string()),
        )
    })
}

/// Read `homeboy.json` from a zip archive's root, or from exactly one level of
/// nesting (`<dir>/homeboy.json`) — the shape of a GitHub-generated source
/// zipball, which roots at `<owner>-<repo>-<sha>/`.
///
/// Returns `Ok(None)` when no unambiguous candidate exists: no match, or more
/// than one nested match (an archive this function has no basis to choose
/// between).
fn extract_homeboy_json_from_zip_bytes(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|error| {
        Error::internal_io(
            format!("Failed to read release source archive: {error}"),
            None,
        )
    })?;

    let mut root_index = None;
    let mut nested_indices = Vec::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|error| {
            Error::internal_io(
                format!("Failed to read release source archive entry: {error}"),
                None,
            )
        })?;
        let name = file.name();
        if name == "homeboy.json" {
            root_index = Some(i);
            continue;
        }
        if let Some(prefix) = name.strip_suffix("/homeboy.json") {
            if !prefix.is_empty() && !prefix.contains('/') {
                nested_indices.push(i);
            }
        }
    }

    let index = match root_index {
        Some(index) => index,
        None => match nested_indices.as_slice() {
            [single] => *single,
            _ => return Ok(None),
        },
    };

    let mut file = archive.by_index(index).map_err(|error| {
        Error::internal_io(
            format!("Failed to read release source archive entry: {error}"),
            None,
        )
    })?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|error| {
        Error::internal_io(
            format!("Failed to read homeboy.json from release source archive: {error}"),
            None,
        )
    })?;
    Ok(Some(buf))
}

/// Fetch a URL's body, distinguishing a 404 from any other outcome so callers
/// can treat "not found" as a legitimate negative result.
fn fetch_raw_content(
    url: &str,
    auth_token: Option<&str>,
    env: Vec<(String, String)>,
) -> Result<FetchOutcome> {
    let command = curl_body_with_status_command(url, auth_token, env);
    let (body, status) = run_curl_body_with_status(command)?;
    match status {
        Some(404) => Ok(FetchOutcome::NotFound),
        Some(code) if (200..300).contains(&code) => Ok(FetchOutcome::Found(body)),
        Some(code) => Err(Error::internal_io(
            format!("Request to {} failed with HTTP {}", url, code),
            Some(url.to_string()),
        )),
        None => Err(Error::internal_io(
            format!(
                "Request to {} returned a response with no readable HTTP status",
                url
            ),
            Some(url.to_string()),
        )),
    }
}

struct CurlBodyWithStatusCommand {
    args: Vec<String>,
    config_stdin: Option<String>,
    env: Vec<(String, String)>,
}

/// Build a curl invocation that writes the HTTP status code after the
/// response body, separated by a newline, so both are recoverable from one
/// process's stdout without a second request. Deliberately omits `-f`
/// (`--fail`): a non-2xx status must still be readable to distinguish "not
/// found" from every other failure.
fn curl_body_with_status_command(
    url: &str,
    auth_token: Option<&str>,
    env: Vec<(String, String)>,
) -> CurlBodyWithStatusCommand {
    let mut args = vec!["-sSL".to_string(), "--retry".to_string(), "3".to_string()];

    let config_stdin = auth_token.map(github_api_config);
    if config_stdin.is_some() {
        args.extend(["--config".to_string(), "-".to_string()]);
    }

    args.extend([
        "-w".to_string(),
        "\n%{http_code}".to_string(),
        url.to_string(),
    ]);

    CurlBodyWithStatusCommand {
        args,
        config_stdin,
        env,
    }
}

fn run_curl_body_with_status(command: CurlBodyWithStatusCommand) -> Result<(Vec<u8>, Option<u16>)> {
    let mut process = std::process::Command::new("curl");
    process.args(&command.args);
    apply_command_env(&mut process, &command.env);
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::piped());
    if command.config_stdin.is_some() {
        process.stdin(std::process::Stdio::piped());
    }

    let mut child = process.spawn().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("fetch release metadata".to_string()),
        )
    })?;
    if let Some(config_stdin) = command.config_stdin {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            Error::internal_io(
                "Failed to open curl stdin".to_string(),
                Some("fetch release metadata".to_string()),
            )
        })?;
        stdin.write_all(config_stdin.as_bytes()).map_err(|e| {
            Error::internal_io(
                format!("Failed to write curl config: {}", e),
                Some("fetch release metadata".to_string()),
            )
        })?;
    }
    let output = child.wait_with_output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run curl: {}", e),
            Some("fetch release metadata".to_string()),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_io(
            format!("curl failed: {}", stderr.trim()),
            Some("fetch release metadata".to_string()),
        ));
    }

    Ok(split_curl_body_and_status(&output.stdout))
}

/// Split a curl response built with `-w '\n%{http_code}'` into its body and
/// status code. The status is the text after the last newline when it parses
/// as a plain integer; anything else (no trailing status line at all, a body
/// whose own last line happens not to parse) yields `None` and the whole
/// output as the body, so a malformed response degrades to "status unknown"
/// rather than silently truncating real content.
fn split_curl_body_and_status(output: &[u8]) -> (Vec<u8>, Option<u16>) {
    let Some(newline_index) = output.iter().rposition(|&byte| byte == b'\n') else {
        return (output.to_vec(), None);
    };
    let status_str = String::from_utf8_lossy(&output[newline_index + 1..]);
    match status_str.trim().parse::<u16>() {
        Ok(status) => (output[..newline_index].to_vec(), Some(status)),
        Err(_) => (output.to_vec(), None),
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    content_hash::sha256_file(path)
}

fn release_asset_name_matches(asset_name: &str, artifact_name: &str) -> bool {
    if asset_name == artifact_name {
        return true;
    }

    let Some((ordinal, rest)) = asset_name.split_once('-') else {
        return false;
    };

    !ordinal.is_empty() && ordinal.chars().all(|c| c.is_ascii_digit()) && rest == artifact_name
}

fn github_auth_token(github: &GitHubRepo, github_config: &GithubConfig) -> Option<String> {
    let gh_command = gh_auth_token_command(github, github_config);
    let mut command = std::process::Command::new("gh");
    command.args(&gh_command.args);
    apply_command_env(&mut command, &gh_command.env);
    let output = command.output().ok()?;

    if !output.status.success() {
        return None;
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

struct GitHubCommand {
    args: Vec<String>,
    env: Vec<(String, String)>,
}

struct CurlReleaseArtifactCommand {
    args: Vec<String>,
    config_stdin: Option<String>,
    env: Vec<(String, String)>,
}

struct CurlReleaseAssetApiCommand {
    args: Vec<String>,
    config_stdin: String,
    env: Vec<(String, String)>,
}

fn gh_auth_token_command(github: &GitHubRepo, github_config: &GithubConfig) -> GitHubCommand {
    GitHubCommand {
        args: vec![
            "auth".to_string(),
            "token".to_string(),
            "--hostname".to_string(),
            github.host.clone(),
        ],
        env: github_command_env(github, github_config),
    }
}

fn curl_release_artifact_command(
    url: &str,
    dest_path: &str,
    auth_token: Option<&str>,
    env: Vec<(String, String)>,
) -> CurlReleaseArtifactCommand {
    let mut args = vec!["-fsSL".to_string(), "--retry".to_string(), "3".to_string()];

    let config_stdin = auth_token.map(|token| {
        format!(
            "{}header = \"Accept: application/octet-stream\"\n",
            github_api_config(token)
        )
    });

    if config_stdin.is_some() {
        args.extend(["--config".to_string(), "-".to_string()]);
    }

    args.extend(["-o".to_string(), dest_path.to_string(), url.to_string()]);

    CurlReleaseArtifactCommand {
        args,
        config_stdin,
        env,
    }
}

fn curl_release_asset_api_command(
    url: &str,
    auth_token: &str,
    env: Vec<(String, String)>,
) -> CurlReleaseAssetApiCommand {
    CurlReleaseAssetApiCommand {
        args: vec![
            "-fsSL".to_string(),
            "--retry".to_string(),
            "3".to_string(),
            "--config".to_string(),
            "-".to_string(),
            url.to_string(),
        ],
        config_stdin: github_api_config(auth_token),
        env,
    }
}

fn github_command_env(github: &GitHubRepo, github_config: &GithubConfig) -> Vec<(String, String)> {
    github_cli_env(&github.host, github_config)
}

fn apply_command_env(command: &mut std::process::Command, env: &[(String, String)]) {
    for (key, value) in env {
        command.env(key, value);
    }
}

fn github_api_config(token: &str) -> String {
    format!(
        "header = \"Authorization: Bearer {}\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nheader = \"User-Agent: homeboy\"\n",
        token
    )
}

fn validate_downloaded_artifact(path: &Path, artifact_name: &str, url: &str) -> Result<()> {
    if !artifact_name.ends_with(".zip") {
        return Ok(());
    }

    let bytes = std::fs::read(path).map_err(|e| {
        Error::internal_io(
            format!("Failed to read downloaded artifact for validation: {}", e),
            Some(path.display().to_string()),
        )
    })?;

    if bytes.starts_with(ZIP_MAGIC_PREFIX) {
        return Ok(());
    }

    let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let normalized_preview = preview.trim_start().to_ascii_lowercase();
    let hint = if normalized_preview.starts_with("<!doctype html")
        || normalized_preview.starts_with("<html")
        || normalized_preview.contains("class=\"html-auth\"")
    {
        "downloaded bytes look like an HTML authentication page; check GitHub Enterprise authentication/proxy configuration"
    } else {
        "downloaded bytes do not start with a ZIP file signature"
    };

    Err(Error::validation_invalid_argument(
        "releaseArtifact",
        format!(
            "Downloaded release artifact '{}' from {} is not a valid ZIP archive: {}",
            artifact_name, url, hint
        ),
        Some(path.display().to_string()),
        None,
    ))
}

/// Check if a component supports release-based deployment.
///
/// Requirements:
/// - `remote_url` is set (GitHub repo URL)
/// - `build_artifact` is set (to know what to download)
/// - The remote URL is a valid GitHub URL
pub fn supports_release_deploy(component: &Component) -> bool {
    let has_remote = component
        .remote_url
        .as_ref()
        .and_then(|url| parse_github_url(url))
        .is_some();
    let has_artifact = resolve_artifact_name(component).is_some();
    has_remote && has_artifact
}

/// Detect package dependency specs that are resolved from mutable sources.
///
/// Release artifacts are safe to reuse when dependencies resolve from immutable
/// package registry versions. Local paths and Git refs can change independently
/// of the component tag, so deploy should rebuild locally instead.
pub fn has_mutable_package_dependencies(component: &Component) -> bool {
    let package_json_path = Path::new(&component.local_path).join("package.json");
    let Ok(raw) = std::fs::read_to_string(package_json_path) else {
        return false;
    };
    let Ok(package_json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };

    PACKAGE_DEPENDENCY_FIELDS.iter().any(|field| {
        package_json
            .get(field)
            .and_then(serde_json::Value::as_object)
            .is_some_and(|dependencies| {
                dependencies
                    .values()
                    .any(|spec| spec.as_str().is_some_and(is_mutable_dependency_spec))
            })
    })
}

fn is_mutable_dependency_spec(spec: &str) -> bool {
    let spec = spec.trim().to_ascii_lowercase();
    MUTABLE_DEPENDENCY_PREFIXES
        .iter()
        .any(|prefix| spec.starts_with(prefix))
}

/// Auto-detect the git remote URL from a local repository.
///
/// Resolves the repository's remote (preferring `origin`, falling back to a sole
/// configured remote) and runs `git remote get-url <remote>` in the directory.
pub fn detect_remote_url(repo_path: &Path) -> Option<String> {
    detect_remote_url_within(repo_path, crate::git::DEFAULT_GIT_READ_PROBE_TIMEOUT).resolved()
}

/// Detect a repository remote within a fixed metadata-read budget.
///
/// The typed timeout outcome lets discovery callers degrade without leaving a
/// Cook admission blocked behind a stale checkout or credential helper.
pub fn detect_remote_url_within(
    repo_path: &Path,
    timeout: std::time::Duration,
) -> crate::git::BoundedGitRead {
    let started = std::time::Instant::now();
    let remotes = match crate::git::output_optional_within(repo_path, &["remote"], timeout) {
        crate::git::BoundedGitRead::Resolved(remotes) => remotes,
        outcome => return outcome,
    };
    let remotes = remotes
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .collect::<Vec<_>>();
    let remote = if remotes.iter().any(|remote| *remote == "origin") {
        "origin"
    } else if let [remote] = remotes.as_slice() {
        remote
    } else {
        "origin"
    };
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return crate::git::BoundedGitRead::TimedOut;
    }
    crate::git::output_optional_within(repo_path, &["remote", "get-url", remote], remaining)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // Hashed independently of the shared primitive so this stays a real
    // cross-check on the digest the downloader records.
    use sha2::{Digest, Sha256};

    use crate::component::GithubHostConfig;

    use super::*;

    #[test]
    fn remote_detection_enriches_a_live_portable_checkout() {
        let repo = tempfile::tempdir().expect("repo directory");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/example/portable.git",
        ]);

        assert_eq!(
            detect_remote_url_within(repo.path(), std::time::Duration::from_secs(1)),
            crate::git::BoundedGitRead::Resolved(
                "https://github.com/example/portable.git".to_string()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_detection_times_out_with_typed_evidence() {
        let _environment = crate::test_support::env_lock();
        let repo = tempfile::tempdir().expect("repo directory");
        let bin = tempfile::tempdir().expect("fake git bin");
        let git = bin.path().join("git");
        std::fs::write(&git, "#!/bin/sh\nsleep 30\n").expect("write fake git");
        let mut permissions = std::fs::metadata(&git).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("make fake git executable");
        let _path = crate::test_support::EnvVarGuard::set(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var_os("PATH")
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );

        let started = std::time::Instant::now();
        let result = detect_remote_url_within(repo.path(), std::time::Duration::from_millis(200));
        assert_eq!(result, crate::git::BoundedGitRead::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn parse_github_url_https() {
        let repo = parse_github_url("https://github.com/Extra-Chill/homeboy").unwrap();
        assert_eq!(repo.host, "github.com");
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_https_with_git_suffix() {
        let repo = parse_github_url("https://github.com/Extra-Chill/homeboy.git").unwrap();
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_authenticated_https() {
        let repo =
            parse_github_url("https://x-access-token:TOKEN@github.com/Extra-Chill/homeboy.git")
                .unwrap();
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_authenticated_https_user_token() {
        let repo =
            parse_github_url("https://user:token@github.com/Extra-Chill/homeboy.git").unwrap();
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_authenticated_http() {
        let repo =
            parse_github_url("http://user:token@github.com/Extra-Chill/homeboy.git").unwrap();
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_ssh() {
        let repo = parse_github_url("git@github.com:Extra-Chill/homeboy.git").unwrap();
        assert_eq!(repo.host, "github.com");
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_enterprise_ssh() {
        let repo = parse_github_url("git@github.example.com:example-org/intelligence.git").unwrap();
        assert_eq!(repo.host, "github.example.com");
        assert_eq!(repo.owner, "example-org");
        assert_eq!(repo.repo, "intelligence");
    }

    #[test]
    fn parse_github_url_enterprise_https() {
        let repo =
            parse_github_url("https://github.example.com/example-org/intelligence.git").unwrap();
        assert_eq!(repo.host, "github.example.com");
        assert_eq!(repo.owner, "example-org");
        assert_eq!(repo.repo, "intelligence");
    }

    #[test]
    fn parse_github_url_trailing_slash() {
        let repo = parse_github_url("https://github.com/Extra-Chill/homeboy/").unwrap();
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    }

    #[test]
    fn parse_github_url_invalid() {
        assert!(parse_github_url("https://gitlab.com/foo/bar").is_none());
        assert!(parse_github_url("https://github.com.evil/foo/bar").is_none());
        assert!(parse_github_url("https://token@github.com.evil/foo/bar").is_none());
        assert!(parse_github_url("not a url").is_none());
        assert!(parse_github_url("").is_none());
    }

    #[test]
    fn release_artifact_url_format() {
        let repo = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill".to_string(),
            repo: "sample-plugin".to_string(),
        };
        let url = repo.release_artifact_url("v0.36.1", "sample-plugin.zip");
        assert_eq!(
            url,
            "https://github.com/Extra-Chill/sample-plugin/releases/download/v0.36.1/sample-plugin.zip"
        );
    }

    #[test]
    fn enterprise_release_artifact_url_uses_remote_host() {
        let repo = GitHubRepo {
            host: "github.example.com".to_string(),
            owner: "example-org".to_string(),
            repo: "intelligence".to_string(),
        };
        let url = repo.release_artifact_url("v1.2.3", "intelligence.zip");
        assert_eq!(
            url,
            "https://github.example.com/example-org/intelligence/releases/download/v1.2.3/intelligence.zip"
        );
    }

    #[test]
    fn resolve_artifact_name_from_path() {
        let mut comp = Component::new(
            "test".to_string(),
            "/tmp".to_string(),
            "/remote".to_string(),
            Some("target/distrib/test-plugin.zip".to_string()),
        );
        assert_eq!(
            resolve_artifact_name(&comp),
            Some("test-plugin.zip".to_string())
        );

        comp.build_artifact = Some("simple.zip".to_string());
        assert_eq!(resolve_artifact_name(&comp), Some("simple.zip".to_string()));

        comp.build_artifact = None;
        assert_eq!(resolve_artifact_name(&comp), None);
    }

    #[test]
    fn supports_release_deploy_requires_both_fields() {
        let mut comp = Component::new(
            "test".to_string(),
            "/tmp".to_string(),
            "/remote".to_string(),
            Some("test.zip".to_string()),
        );

        // No remote_url → false
        assert!(!supports_release_deploy(&comp));

        // With remote_url → true
        comp.remote_url = Some("https://github.com/Extra-Chill/test".to_string());
        assert!(supports_release_deploy(&comp));

        // No build_artifact → false
        comp.build_artifact = None;
        assert!(!supports_release_deploy(&comp));
    }

    #[test]
    fn validate_downloaded_artifact_rejects_html_auth_page_as_zip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("theme.zip");
        std::fs::write(
            &artifact,
            br#"<!DOCTYPE html><html lang="en" class="html-auth"><body>Sign in</body></html>"#,
        )
        .expect("write html artifact");

        let error = validate_downloaded_artifact(
            &artifact,
            "theme.zip",
            "https://github.example.com/example-org/theme/releases/download/v1/theme.zip",
        )
        .expect_err("html auth page should be rejected");
        let message = error.to_string();

        assert!(message.contains("not a valid ZIP archive"));
        assert!(message.contains("HTML authentication page"));
        assert!(message.contains("GitHub Enterprise authentication/proxy"));
    }

    #[test]
    fn validate_downloaded_artifact_accepts_zip_signature() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("theme.zip");
        std::fs::write(&artifact, b"PK\x03\x04fixture").expect("write zip artifact");

        validate_downloaded_artifact(
            &artifact,
            "theme.zip",
            "https://github.com/Extra-Chill/theme/releases/download/v1/theme.zip",
        )
        .expect("zip signature should be accepted");
    }

    #[test]
    fn curl_release_artifact_command_sends_auth_header_via_stdin_config() {
        let command = curl_release_artifact_command(
            "https://github.example.com/example-org/theme/releases/download/v1/theme.zip",
            "/tmp/theme.zip",
            Some("secret-token"),
            Vec::new(),
        );

        assert!(command.args.contains(&"--config".to_string()));
        assert!(command.args.contains(&"-".to_string()));
        assert!(!command.args.iter().any(|arg| arg.contains("secret-token")));
        let config = command.config_stdin.as_deref().expect("curl config");
        assert!(config.contains("Authorization: Bearer secret-token"));
        assert!(config.contains("Accept: application/octet-stream"));
        assert!(config.contains("User-Agent: homeboy"));
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("https://github.example.com/example-org/theme/releases/download/v1/theme.zip")
        );
    }

    #[test]
    fn curl_release_artifact_command_omits_auth_header_without_token() {
        let command = curl_release_artifact_command(
            "https://github.com/example-org/theme/releases/download/v1/theme.zip",
            "/tmp/theme.zip",
            None,
            Vec::new(),
        );

        assert!(!command.args.contains(&"--config".to_string()));
        assert_eq!(command.config_stdin, None);
    }

    #[test]
    fn github_release_api_url_uses_enterprise_api_path() {
        let repo = GitHubRepo {
            host: "github.a8c.com".to_string(),
            owner: "chubes4".to_string(),
            repo: "studio-native".to_string(),
        };

        assert_eq!(
            repo.release_by_tag_api_url("v0.12.3"),
            "https://github.a8c.com/api/v3/repos/chubes4/studio-native/releases/tags/v0.12.3"
        );
    }

    #[test]
    fn github_release_api_url_uses_public_api_host() {
        let repo = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill".to_string(),
            repo: "homeboy".to_string(),
        };

        assert_eq!(
            repo.release_by_tag_api_url("v1.2.3"),
            "https://api.github.com/repos/Extra-Chill/homeboy/releases/tags/v1.2.3"
        );
    }

    #[test]
    fn release_asset_commands_include_enterprise_host_and_configured_proxy_env() {
        let github = GitHubRepo {
            host: "github.enterprise.test".to_string(),
            owner: "example-org".to_string(),
            repo: "theme".to_string(),
        };
        let config = GithubConfig {
            hosts: HashMap::from([(
                "github.enterprise.test".to_string(),
                GithubHostConfig {
                    proxy: Some("socks5://127.0.0.1:8080".to_string()),
                    env: HashMap::new(),
                },
            )]),
        };

        let gh_command = gh_auth_token_command(&github, &config);
        let api_command = curl_release_asset_api_command(
            &github.release_by_tag_api_url("v1.2.3"),
            "secret-token",
            github_command_env(&github, &config),
        );
        let artifact_command = curl_release_artifact_command(
            &github.release_artifact_url("v1.2.3", "theme.zip"),
            "/tmp/theme.zip",
            Some("secret-token"),
            github_command_env(&github, &config),
        );

        assert_eq!(
            gh_command.args,
            vec!["auth", "token", "--hostname", "github.enterprise.test"]
        );
        for env in [&gh_command.env, &api_command.env, &artifact_command.env] {
            assert!(env.contains(&("GH_HOST".to_string(), "github.enterprise.test".to_string())));
            assert!(env.contains(&(
                "HTTPS_PROXY".to_string(),
                "socks5://127.0.0.1:8080".to_string()
            )));
        }
        assert_eq!(
            api_command.args,
            vec![
                "-fsSL",
                "--retry",
                "3",
                "--config",
                "-",
                "https://github.enterprise.test/api/v3/repos/example-org/theme/releases/tags/v1.2.3"
            ]
        );
        assert_eq!(
            artifact_command.args.last().map(String::as_str),
            Some("https://github.enterprise.test/example-org/theme/releases/download/v1.2.3/theme.zip")
        );
    }

    #[test]
    fn release_asset_commands_do_not_add_enterprise_env_for_github_com() {
        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill".to_string(),
            repo: "theme".to_string(),
        };
        let config = GithubConfig {
            hosts: HashMap::from([(
                "github.enterprise.test".to_string(),
                GithubHostConfig {
                    proxy: Some("socks5://127.0.0.1:8080".to_string()),
                    env: HashMap::new(),
                },
            )]),
        };

        let gh_command = gh_auth_token_command(&github, &config);
        let artifact_command = curl_release_artifact_command(
            &github.release_artifact_url("v1.2.3", "theme.zip"),
            "/tmp/theme.zip",
            None,
            github_command_env(&github, &config),
        );

        assert_eq!(gh_command.env, Vec::<(String, String)>::new());
        assert_eq!(artifact_command.env, Vec::<(String, String)>::new());
        assert_eq!(
            gh_command.args,
            vec!["auth", "token", "--hostname", "github.com"]
        );
    }

    #[test]
    fn release_asset_name_matches_homeboy_ordinal_artifact_names() {
        assert!(release_asset_name_matches(
            "01-studio-native-theme.zip",
            "studio-native-theme.zip"
        ));
        assert!(release_asset_name_matches(
            "studio-native-theme.zip",
            "studio-native-theme.zip"
        ));
        assert!(!release_asset_name_matches(
            "studio-native.zip",
            "studio-native-theme.zip"
        ));
        assert!(!release_asset_name_matches(
            "theme-01-studio-native-theme.zip",
            "studio-native-theme.zip"
        ));
    }

    #[test]
    fn release_artifact_store_leases_deduplicate_and_retain_downloads() {
        let github = GitHubRepo {
            host: "github.example.test".to_string(),
            owner: "example".to_string(),
            repo: "plugin".to_string(),
        };
        let mut store = ReleaseArtifactStore::default();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin.zip");
        let calls = std::cell::Cell::new(0);
        let first = store
            .get_or_insert_with(&github, "v1.2.3", "plugin.zip", || {
                calls.set(calls.get() + 1);
                std::fs::write(&path, b"payload").expect("write downloaded artifact");
                Ok(ReleaseArtifact {
                    path: path.clone(),
                    tag: "v1.2.3".to_string(),
                    commit: Some("abc123".to_string()),
                    url: "https://github.example.test/api/v3/assets/1".to_string(),
                    name: "plugin.zip".to_string(),
                    size: 7,
                    sha256: "abc".to_string(),
                })
            })
            .expect("first target resolves asset");
        let second = store
            .get_or_insert_with(&github, "v1.2.3", "plugin.zip", || {
                calls.set(calls.get() + 1);
                panic!("second identical acquisition must reuse the verified download")
            })
            .expect("second target reuses asset");

        assert_eq!(calls.get(), 1, "one metadata lookup/download per run");
        assert_eq!(
            std::fs::read(&first.path).expect("first lease reads"),
            b"payload"
        );

        drop(first);
        assert_eq!(
            std::fs::read(&second.path).expect("second lease remains valid"),
            b"payload"
        );

        drop(second);
        assert!(
            path.exists(),
            "runtime download retention policy keeps the final artifact for cleanup"
        );
        drop(store);
        assert!(
            path.exists(),
            "dropping the final lease does not invent eager deletion"
        );
    }

    #[test]
    fn release_artifact_leases_pin_runtime_downloads_until_the_final_drop() {
        crate::test_support::with_isolated_home(|_| {
            let directory = crate::engine::temp::runtime_temp_dir("deploy-download")
                .expect("runtime download directory");
            let path = directory.join("plugin.zip");
            std::fs::write(&path, b"payload").expect("write downloaded artifact");
            let lease = ReleaseArtifactLease::new(ReleaseArtifact {
                path,
                tag: "v1.2.3".to_string(),
                commit: Some("abc123".to_string()),
                url: "https://github.example.test/api/v3/assets/1".to_string(),
                name: "plugin.zip".to_string(),
                size: 7,
                sha256: "abc".to_string(),
            })
            .expect("pin runtime download");
            let second = lease.clone();

            let pinned =
                crate::engine::temp::cleanup_runtime_tmp(true, 0, Some("deploy-download"), 10)
                    .expect("cleanup while pinned");
            assert_eq!(pinned.removed_count, 0);
            assert!(directory.exists());

            drop(lease);
            let still_pinned =
                crate::engine::temp::cleanup_runtime_tmp(true, 0, Some("deploy-download"), 10)
                    .expect("cleanup with second lease");
            assert_eq!(still_pinned.removed_count, 0);
            assert!(directory.exists());

            drop(second);
            assert!(
                !directory.join(".homeboy-runtime-temp-pin-v1").exists(),
                "the final lease removes its cleanup pin"
            );
            let reclaimed =
                crate::engine::temp::cleanup_runtime_tmp(true, 0, Some("deploy-download"), 10)
                    .expect("cleanup after final lease");
            assert_eq!(reclaimed.removed_count, 1);
            assert!(!directory.exists());
        });
    }

    #[test]
    fn sha256_file_reports_downloaded_content_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin.zip");
        std::fs::write(&path, b"verified bytes").expect("write artifact");
        assert_eq!(
            sha256_file(&path).expect("hash artifact"),
            format!("{:x}", Sha256::digest(b"verified bytes"))
        );
    }

    #[test]
    fn verification_failure_does_not_store_an_artifact_for_target_mutation() {
        let github = GitHubRepo {
            host: "github.example.test".to_string(),
            owner: "example".to_string(),
            repo: "plugin".to_string(),
        };
        let mut store = ReleaseArtifactStore::default();

        let error = store
            .get_or_insert_with(&github, "v1.2.3", "plugin.zip", || {
                Err(Error::validation_invalid_argument(
                    "releaseArtifact",
                    "Downloaded release artifact 'plugin.zip' SHA-256 digest does not match GitHub Release metadata".to_string(),
                    None,
                    None,
                ))
            })
            .expect_err("digest mismatch must fail release preflight");

        assert!(error.to_string().contains("SHA-256 digest does not match"));
        assert!(
            store.get(&github, "v1.2.3", "plugin.zip").is_none(),
            "a failed verification must not make bytes available to deploy targets"
        );
    }

    #[test]
    fn failed_acquisition_creates_no_live_entry_or_lease() {
        let github = GitHubRepo {
            host: "github.example.test".to_string(),
            owner: "example".to_string(),
            repo: "plugin".to_string(),
        };
        let mut store = ReleaseArtifactStore::default();
        let temp = tempfile::tempdir().expect("tempdir");
        let download_dir = temp.path().join("deploy-download");
        std::fs::create_dir(&download_dir).expect("download directory");
        let calls = std::cell::Cell::new(0);

        let error = store
            .get_or_insert_with(&github, "v1.2.3", "plugin.zip", || {
                calls.set(calls.get() + 1);
                std::fs::write(download_dir.join("plugin.zip"), b"invalid bytes")
                    .expect("failed download bytes");
                Err(Error::validation_invalid_argument(
                    "releaseArtifact",
                    "Downloaded release artifact 'plugin.zip' SHA-256 digest does not match GitHub Release metadata".to_string(),
                    None,
                    None,
                ))
            })
            .expect_err("failed verification must not yield a lease");

        assert_eq!(calls.get(), 1);
        assert!(error.to_string().contains("SHA-256 digest does not match"));
        assert!(store.get(&github, "v1.2.3", "plugin.zip").is_none());
        assert!(
            !download_dir.join(".homeboy-runtime-temp-pin-v1").exists(),
            "failed acquisition must not leave a cleanup pin"
        );
    }

    #[test]
    fn digest_mismatch_fails_before_an_artifact_can_be_fanned_out() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("plugin.zip");
        std::fs::write(&path, b"PK\x03\x04fixture").expect("write artifact");
        let metadata = ReleaseAssetMetadata {
            url: "https://github.example.test/api/v3/assets/1".to_string(),
            name: "plugin.zip".to_string(),
            size: 11,
            digest: Some(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            ),
            tag: "v1.2.3".to_string(),
            commit: Some("abc123".to_string()),
        };

        let error = verify_downloaded_release_artifact(&path, &metadata, &metadata.url)
            .expect_err("metadata digest mismatch must fail before target preparation");

        assert!(error.to_string().contains("SHA-256 digest does not match"));
    }

    #[test]
    fn mutable_package_dependencies_detects_git_and_file_specs() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{
                "dependencies": {
                    "registry-only": "^1.2.3",
                    "tokens": "github:Extra-Chill/extrachill-tokens#v0.7.2"
                },
                "devDependencies": {
                    "components": "file:../components"
                }
            }"#,
        )
        .expect("write package.json");
        let comp = Component::new(
            "test".to_string(),
            temp.path().to_string_lossy().to_string(),
            "/remote".to_string(),
            Some("test.zip".to_string()),
        );

        assert!(has_mutable_package_dependencies(&comp));
    }

    #[test]
    fn mutable_package_dependencies_allows_registry_specs() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("package.json"),
            r#"{
                "dependencies": {
                    "serde": "1.0.0",
                    "react": "^19.0.0",
                    "private-registry": "npm:@scope/pkg@1.2.3"
                },
                "optionalDependencies": {
                    "optional": "~2.0.0"
                }
            }"#,
        )
        .expect("write package.json");
        let comp = Component::new(
            "test".to_string(),
            temp.path().to_string_lossy().to_string(),
            "/remote".to_string(),
            Some("test.zip".to_string()),
        );

        assert!(!has_mutable_package_dependencies(&comp));
    }

    // =========================================================================
    // Checkout-less resolution (#14782)
    // =========================================================================

    fn github_repo() -> GitHubRepo {
        GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill".to_string(),
            repo: "data-machine".to_string(),
        }
    }

    fn enterprise_repo() -> GitHubRepo {
        GitHubRepo {
            host: "github.example.com".to_string(),
            owner: "example-org".to_string(),
            repo: "theme".to_string(),
        }
    }

    #[test]
    fn raw_content_url_uses_raw_githubusercontent_for_github_com() {
        assert_eq!(
            github_repo().raw_content_url("v1.2.3", "homeboy.json"),
            "https://raw.githubusercontent.com/Extra-Chill/data-machine/v1.2.3/homeboy.json"
        );
    }

    #[test]
    fn raw_content_url_uses_enterprise_web_host() {
        assert_eq!(
            enterprise_repo().raw_content_url("v1.2.3", "homeboy.json"),
            "https://github.example.com/example-org/theme/raw/v1.2.3/homeboy.json"
        );
    }

    #[test]
    fn latest_release_api_url_uses_public_api_host() {
        assert_eq!(
            github_repo().latest_release_api_url(),
            "https://api.github.com/repos/Extra-Chill/data-machine/releases/latest"
        );
    }

    #[test]
    fn latest_release_api_url_uses_enterprise_api_path() {
        assert_eq!(
            enterprise_repo().latest_release_api_url(),
            "https://github.example.com/api/v3/repos/example-org/theme/releases/latest"
        );
    }

    #[test]
    fn zipball_api_url_uses_public_api_host() {
        assert_eq!(
            github_repo().zipball_api_url("v1.2.3"),
            "https://api.github.com/repos/Extra-Chill/data-machine/zipball/v1.2.3"
        );
    }

    #[test]
    fn zipball_api_url_uses_enterprise_api_path() {
        assert_eq!(
            enterprise_repo().zipball_api_url("v1.2.3"),
            "https://github.example.com/api/v3/repos/example-org/theme/zipball/v1.2.3"
        );
    }

    #[test]
    fn split_curl_body_and_status_separates_body_from_trailing_status() {
        let output = b"{\"tag_name\":\"v1.2.3\"}\n200".to_vec();
        let (body, status) = split_curl_body_and_status(&output);
        assert_eq!(body, b"{\"tag_name\":\"v1.2.3\"}");
        assert_eq!(status, Some(200));
    }

    #[test]
    fn split_curl_body_and_status_reports_404() {
        let output = b"Not Found\n404".to_vec();
        let (body, status) = split_curl_body_and_status(&output);
        assert_eq!(body, b"Not Found");
        assert_eq!(status, Some(404));
    }

    #[test]
    fn split_curl_body_and_status_handles_a_multiline_body() {
        let output = b"line one\nline two\n204".to_vec();
        let (body, status) = split_curl_body_and_status(&output);
        assert_eq!(body, b"line one\nline two");
        assert_eq!(status, Some(204));
    }

    #[test]
    fn split_curl_body_and_status_degrades_when_no_status_line_is_present() {
        let output = b"just a body, no trailing status".to_vec();
        let (body, status) = split_curl_body_and_status(&output);
        assert_eq!(body, output);
        assert_eq!(status, None);
    }

    #[test]
    fn curl_body_with_status_command_sends_auth_header_and_no_fail_flag() {
        let command = curl_body_with_status_command(
            "https://api.github.com/repos/Extra-Chill/data-machine/releases/latest",
            Some("secret-token"),
            Vec::new(),
        );

        assert!(!command.args.contains(&"-f".to_string()));
        assert!(!command.args.contains(&"-fsSL".to_string()));
        assert!(command.args.contains(&"--config".to_string()));
        let config = command.config_stdin.as_deref().expect("curl config");
        assert!(config.contains("Authorization: Bearer secret-token"));
        assert!(command.args.contains(&"-w".to_string()));
        assert!(command.args.contains(&"\n%{http_code}".to_string()));
    }

    #[test]
    fn curl_body_with_status_command_omits_auth_without_a_token() {
        let command = curl_body_with_status_command(
            "https://api.github.com/repos/Extra-Chill/data-machine/releases/latest",
            None,
            Vec::new(),
        );

        assert!(!command.args.contains(&"--config".to_string()));
        assert_eq!(command.config_stdin, None);
    }

    #[test]
    fn parse_latest_release_tag_reads_tag_name() {
        let body = br#"{"tag_name": "v0.176.17", "id": 1}"#;
        assert_eq!(
            parse_latest_release_tag(body).expect("parses"),
            Some("v0.176.17".to_string())
        );
    }

    #[test]
    fn parse_latest_release_tag_rejects_invalid_json() {
        assert!(parse_latest_release_tag(b"not json").is_err());
    }

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::FileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).expect("start zip entry");
            writer.write_all(content).expect("write zip entry");
        }
        writer.finish().expect("finish zip").into_inner()
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_reads_the_root_entry() {
        let bytes = build_zip(&[
            ("homeboy.json", br#"{"id":"data-machine"}"#),
            ("README.md", b"hello"),
        ]);

        let extracted = extract_homeboy_json_from_zip_bytes(&bytes)
            .expect("extraction succeeds")
            .expect("homeboy.json found");
        assert_eq!(extracted, br#"{"id":"data-machine"}"#);
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_reads_one_level_of_nesting() {
        // GitHub source zipballs root at a generated `<owner>-<repo>-<sha>/` dir.
        let bytes = build_zip(&[
            (
                "Extra-Chill-data-machine-abc1234/homeboy.json",
                br#"{"id":"data-machine"}"#,
            ),
            ("Extra-Chill-data-machine-abc1234/README.md", b"hello"),
        ]);

        let extracted = extract_homeboy_json_from_zip_bytes(&bytes)
            .expect("extraction succeeds")
            .expect("nested homeboy.json found");
        assert_eq!(extracted, br#"{"id":"data-machine"}"#);
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_prefers_root_over_nested() {
        let bytes = build_zip(&[
            ("homeboy.json", br#"{"id":"root"}"#),
            ("nested-dir/homeboy.json", br#"{"id":"nested"}"#),
        ]);

        let extracted = extract_homeboy_json_from_zip_bytes(&bytes)
            .expect("extraction succeeds")
            .expect("root homeboy.json found");
        assert_eq!(extracted, br#"{"id":"root"}"#);
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_returns_none_when_absent() {
        let bytes = build_zip(&[("README.md", b"hello")]);

        assert!(extract_homeboy_json_from_zip_bytes(&bytes)
            .expect("extraction succeeds")
            .is_none());
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_returns_none_for_ambiguous_nesting() {
        // More than one nested candidate — this function has no basis to pick.
        let bytes = build_zip(&[
            ("dir-a/homeboy.json", br#"{"id":"a"}"#),
            ("dir-b/homeboy.json", br#"{"id":"b"}"#),
        ]);

        assert!(extract_homeboy_json_from_zip_bytes(&bytes)
            .expect("extraction succeeds")
            .is_none());
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_ignores_deeply_nested_entries() {
        // Two levels of nesting is not the GitHub zipball shape this function
        // targets, and must not be mistaken for it.
        let bytes = build_zip(&[("a/b/homeboy.json", br#"{"id":"deep"}"#)]);

        assert!(extract_homeboy_json_from_zip_bytes(&bytes)
            .expect("extraction succeeds")
            .is_none());
    }

    #[test]
    fn extract_homeboy_json_from_zip_bytes_rejects_a_non_zip_payload() {
        assert!(extract_homeboy_json_from_zip_bytes(b"not a zip").is_err());
    }

    // =========================================================================
    // End-to-end checkout-less resolution against a fake `curl`/`gh` on PATH.
    //
    // These functions shell out to `curl` and `gh` exactly like the rest of
    // this module (see `remote_detection_times_out_with_typed_evidence` above
    // for the same fake-binary-on-PATH technique already used in this file).
    // A real HTTP mock is not available here, so the fake binaries stand in
    // for GitHub: they read the request shape from argv and answer
    // deterministically, which is enough to exercise the real parsing,
    // caching, and fallback code paths without a network call.
    // =========================================================================

    #[cfg(unix)]
    fn write_executable(path: &Path, script: &str) {
        std::fs::write(path, script).expect("write fake binary");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make fake binary executable");
    }

    #[cfg(unix)]
    fn write_fake_gh_with_token(bin_dir: &Path) {
        write_executable(
            &bin_dir.join("gh"),
            "#!/bin/sh\necho fake-test-token\nexit 0\n",
        );
    }

    #[cfg(unix)]
    fn write_fake_gh_without_token(bin_dir: &Path) {
        write_executable(&bin_dir.join("gh"), "#!/bin/sh\nexit 1\n");
    }

    #[cfg(unix)]
    fn prefixed_path(bin_dir: &Path) -> std::ffi::OsString {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = bin_dir.as_os_str().to_os_string();
        new_path.push(":");
        new_path.push(existing);
        new_path
    }

    /// A fake `curl` that answers the body+status request shape
    /// (`curl_body_with_status_command`): drains any piped `--config` stdin so
    /// the parent's write cannot block, then matches the request URL (the
    /// final argument) against `matchers` in order and prints `body\nstatus`
    /// for the first match, or `\n404` when nothing matches.
    #[cfg(unix)]
    fn write_fake_curl_body_status(bin_dir: &Path, matchers: &[(&str, &str, u16)]) {
        let mut script = String::from("#!/bin/sh\ncat > /dev/null 2>&1\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n");
        for (pattern, body, status) in matchers {
            script.push_str(&format!(
                "  *{pattern}*) printf '%s' '{body}'; printf '\\n{status}' ;;\n"
            ));
        }
        script.push_str("  *) printf '\\n404' ;;\nesac\n");
        write_executable(&bin_dir.join("curl"), &script);
    }

    /// A fake `curl` that answers the body+status shape with `\n404` for
    /// everything, and the `-o <dest>` file-download shape (used by the
    /// zipball fallback) by copying `fixture_zip` to the requested
    /// destination.
    #[cfg(unix)]
    fn write_fake_curl_zip_fallback(bin_dir: &Path, fixture_zip: &Path) {
        let script = format!(
            "#!/bin/sh\n\
             has_o=0\n\
             dest=\"\"\n\
             prev=\"\"\n\
             for arg in \"$@\"; do\n\
             \x20\x20if [ \"$prev\" = \"-o\" ]; then dest=\"$arg\"; has_o=1; fi\n\
             \x20\x20prev=\"$arg\"\n\
             \x20\x20last=\"$arg\"\n\
             done\n\
             cat > /dev/null 2>&1\n\
             if [ \"$has_o\" = \"1\" ]; then cp '{fixture}' \"$dest\"; exit 0; fi\n\
             printf '\\n404'\n",
            fixture = fixture_zip.display(),
        );
        write_executable(&bin_dir.join("curl"), &script);
    }

    #[cfg(unix)]
    #[test]
    fn fetch_portable_config_at_tag_reads_raw_content_when_available() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_gh_with_token(bin_dir.path());
        write_fake_curl_body_status(
            bin_dir.path(),
            &[(
                "homeboy.json",
                r#"{"id":"data-machine","remote_path":"wp-content/plugins/data-machine","build_artifact":"dist/data-machine.zip"}"#,
                200,
            )],
        );
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "raw-content-fixture".to_string(),
        };
        let component =
            fetch_portable_config_at_tag(&github, &GithubConfig::default(), "v9.9.9-raw")
                .expect("resolves from raw content");

        assert_eq!(component.id, "data-machine");
        assert_eq!(component.remote_path, "wp-content/plugins/data-machine");
        assert_eq!(
            component.build_artifact.as_deref(),
            Some("dist/data-machine.zip")
        );
    }

    #[cfg(unix)]
    #[test]
    fn fetch_portable_config_at_tag_caches_per_repo_and_tag() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_gh_with_token(bin_dir.path());
        write_fake_curl_body_status(
            bin_dir.path(),
            &[("homeboy.json", r#"{"id":"cached-component"}"#, 200)],
        );
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "cache-fixture".to_string(),
        };
        let first = fetch_portable_config_at_tag(&github, &GithubConfig::default(), "v1.0.0-cache")
            .expect("first resolve reads from the fake curl");
        assert_eq!(first.id, "cached-component");

        // Replace curl with one that only knows how to fail, so a second call
        // that still succeeds proves it was answered from the cache rather
        // than a second network round trip.
        write_executable(
            &bin_dir.path().join("curl"),
            "#!/bin/sh\ncat > /dev/null 2>&1\nprintf '\\n500'\n",
        );
        let second =
            fetch_portable_config_at_tag(&github, &GithubConfig::default(), "v1.0.0-cache")
                .expect("second resolve is served from the process cache");
        assert_eq!(second.id, "cached-component");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_portable_config_at_tag_falls_back_to_zip_when_raw_content_404s() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        let zip_bytes = build_zip(&[(
            "Extra-Chill-Test-zip-fixture-abc1234/homeboy.json",
            br#"{"id":"data-machine","remote_path":"wp-content/plugins/data-machine"}"#,
        )]);
        let fixture_zip = bin_dir.path().join("fixture.zip");
        std::fs::write(&fixture_zip, &zip_bytes).expect("write fixture zip");

        write_fake_gh_with_token(bin_dir.path());
        write_fake_curl_zip_fallback(bin_dir.path(), &fixture_zip);
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "zip-fixture".to_string(),
        };
        let component =
            fetch_portable_config_at_tag(&github, &GithubConfig::default(), "v2.0.0-zip")
                .expect("resolves from the zip fallback when raw content 404s");

        assert_eq!(component.id, "data-machine");
        assert_eq!(component.remote_path, "wp-content/plugins/data-machine");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_portable_config_at_tag_errors_when_neither_source_has_homeboy_json() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        let zip_bytes = build_zip(&[("Extra-Chill-Test-empty-fixture-abc/README.md", b"hello")]);
        let fixture_zip = bin_dir.path().join("fixture.zip");
        std::fs::write(&fixture_zip, &zip_bytes).expect("write fixture zip");

        write_fake_gh_with_token(bin_dir.path());
        write_fake_curl_zip_fallback(bin_dir.path(), &fixture_zip);
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "empty-fixture".to_string(),
        };
        let error = fetch_portable_config_at_tag(&github, &GithubConfig::default(), "v3.0.0-empty")
            .expect_err("no homeboy.json anywhere must fail");

        assert!(error.message.contains("has no homeboy.json at tag"));
    }

    #[cfg(unix)]
    #[test]
    fn latest_release_tag_for_repo_parses_the_latest_release_tag() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_gh_with_token(bin_dir.path());
        write_fake_curl_body_status(
            bin_dir.path(),
            &[("releases/latest", r#"{"tag_name":"v0.176.17"}"#, 200)],
        );
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "latest-release-fixture".to_string(),
        };
        let tag = latest_release_tag_for_repo(&github, &GithubConfig::default())
            .expect("resolves the latest release tag");

        assert_eq!(tag, Some("v0.176.17".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn latest_release_tag_for_repo_returns_none_on_404() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_gh_with_token(bin_dir.path());
        write_fake_curl_body_status(bin_dir.path(), &[]);
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "no-releases-fixture".to_string(),
        };
        let tag = latest_release_tag_for_repo(&github, &GithubConfig::default())
            .expect("a repo with no releases is Ok(None), not an error");

        assert_eq!(tag, None);
    }

    #[cfg(unix)]
    #[test]
    fn latest_release_tag_for_repo_fails_closed_without_a_github_token() {
        let _lock = crate::test_support::env_lock();
        let bin_dir = tempfile::tempdir().expect("bin dir");
        write_fake_gh_without_token(bin_dir.path());
        let _path = crate::test_support::EnvVarGuard::set("PATH", prefixed_path(bin_dir.path()));

        let github = GitHubRepo {
            host: "github.com".to_string(),
            owner: "Extra-Chill-Test".to_string(),
            repo: "no-token-fixture".to_string(),
        };
        let error = latest_release_tag_for_repo(&github, &GithubConfig::default())
            .expect_err("no GitHub token must fail rather than silently resolve nothing");

        assert!(error
            .message
            .contains("No GitHub authentication token is available"));
    }
}
