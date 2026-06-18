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

use std::path::{Path, PathBuf};

use crate::core::component::Component;
use crate::core::error::{Error, Result};

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

/// Parsed GitHub owner/repo from a remote URL.
#[derive(Debug, Clone)]
pub struct GitHubRepo {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl GitHubRepo {
    /// Construct a release artifact download URL.
    pub(crate) fn release_artifact_url(&self, tag: &str, artifact_name: &str) -> String {
        format!(
            "https://{}/{}/{}/releases/download/{}/{}",
            self.host, self.owner, self.repo, tag, artifact_name
        )
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
    tag: &str,
    artifact_name: &str,
) -> Result<PathBuf> {
    let url = github.release_artifact_url(tag, artifact_name);

    // Create temp directory for the download
    let tmp_dir = crate::core::engine::temp::runtime_temp_dir("deploy-download")?;
    let dest_path = tmp_dir.join(artifact_name);

    log_status!("deploy", "Downloading release artifact: {}", url);

    // Use curl for the download (follows redirects, handles GitHub's CDN)
    let output = std::process::Command::new("curl")
        .args([
            "-fsSL", // fail silently, show errors, follow redirects
            "--retry",
            "3", // retry on transient failures
            "-o",
            dest_path.to_str().unwrap_or("artifact"),
            &url,
        ])
        .output()
        .map_err(|e| {
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

    // Verify the file exists and has content
    let metadata = std::fs::metadata(&dest_path).map_err(|e| {
        Error::internal_io(
            format!("Downloaded artifact not found: {}", e),
            Some(dest_path.display().to_string()),
        )
    })?;

    if metadata.len() == 0 {
        return Err(Error::internal_io(
            format!(
                "Downloaded artifact is empty — check that tag '{}' has a release with artifact '{}'",
                tag, artifact_name
            ),
            Some(url),
        ));
    }

    validate_downloaded_artifact(&dest_path, artifact_name, &url)?;

    log_status!(
        "deploy",
        "Downloaded {} ({} bytes)",
        artifact_name,
        metadata.len()
    );

    Ok(dest_path)
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
    let package_json_path = Path::new(&component.local_path).join(concat!("package", ".json"));
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
/// Runs `git remote get-url origin` in the given directory.
pub fn detect_remote_url(repo_path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output()
        .ok()?;

    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn mutable_package_dependencies_detects_git_and_file_specs() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join(concat!("package", ".json")),
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
        .expect(concat!("write package", ".json"));
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
            temp.path().join(concat!("package", ".json")),
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
        .expect(concat!("write package", ".json"));
        let comp = Component::new(
            "test".to_string(),
            temp.path().to_string_lossy().to_string(),
            "/remote".to_string(),
            Some("test.zip".to_string()),
        );

        assert!(!has_mutable_package_dependencies(&comp));
    }
}
