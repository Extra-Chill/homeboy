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

use crate::component::Component;
use crate::error::{Error, Result};

/// Parsed GitHub owner/repo from a remote URL.
#[derive(Debug, Clone)]
pub struct GitHubRepo {
    pub owner: String,
    pub repo: String,
}

impl GitHubRepo {
    /// Construct a release artifact download URL.
    pub(crate) fn release_artifact_url(&self, tag: &str, artifact_name: &str) -> String {
        format!(
            "https://github.com/{}/{}/releases/download/{}/{}",
            self.owner, self.repo, tag, artifact_name
        )
    }
}

/// Parse owner/repo from a GitHub URL.
///
/// Supports:
/// - `https://github.com/owner/repo`
/// - `https://github.com/owner/repo.git`
/// - `git@github.com:owner/repo.git`
pub fn parse_github_url(url: &str) -> Option<GitHubRepo> {
    // HTTPS format
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        let rest = rest.trim_end_matches(".git").trim_end_matches('/');
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Some(GitHubRepo {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
            });
        }
    }

    // SSH format
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let rest = rest.trim_end_matches(".git").trim_end_matches('/');
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Some(GitHubRepo {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
            });
        }
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
    let tmp_dir = crate::engine::temp::runtime_temp_dir("deploy-download")?;
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

    log_status!(
        "deploy",
        "Downloaded {} ({} bytes)",
        artifact_name,
        metadata.len()
    );

    Ok(dest_path)
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
    fn parse_github_url_ssh() {
        let repo = parse_github_url("git@github.com:Extra-Chill/homeboy.git").unwrap();
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
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
        assert!(parse_github_url("not a url").is_none());
        assert!(parse_github_url("").is_none());
    }

    #[test]
    fn release_artifact_url_format() {
        let repo = GitHubRepo {
            owner: "Extra-Chill".to_string(),
            repo: "data-machine".to_string(),
        };
        let url = repo.release_artifact_url("v0.36.1", "data-machine.zip");
        assert_eq!(
            url,
            "https://github.com/Extra-Chill/data-machine/releases/download/v0.36.1/data-machine.zip"
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
}
