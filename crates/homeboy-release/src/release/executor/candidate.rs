use super::artifacts::source_authority_artifacts;
use super::github_release::{gh_command, github_release_upload_timeout, run_gh_command};
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct CandidatePublication {
    pub source_sha: String,
    pub tag: String,
    pub assets: Vec<CandidateAsset>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateAsset {
    pub name: String,
    pub sha256: String,
    pub url: String,
}

pub fn publish_candidate(
    component: &Component,
    component_id: &str,
    sha: &str,
    artifact_dir: &Path,
    version: &str,
    apply: bool,
) -> Result<CandidatePublication> {
    let sha = resolve_sha(&component.local_path, sha)?;
    let tag = format!("candidate-{sha}");
    let artifacts = source_authority_artifacts(artifact_dir, component_id, &tag, version, &sha)?;
    let remote = component.remote_url.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "candidate",
            "component has no configured GitHub remote",
            None,
            None,
        )
    })?;
    let github =
        homeboy_core::git::release_download::parse_github_url(remote).ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate",
                "candidate publication requires a GitHub remote",
                None,
                None,
            )
        })?;
    let repo = format!("{}/{}", github.owner, github.repo);
    let assets = artifacts
        .iter()
        .map(|artifact| CandidateAsset {
            name: Path::new(&artifact.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            sha256: artifact.sha256.clone(),
            url: format!(
                "https://{}/{repo}/releases/download/{tag}/{}",
                github.host,
                Path::new(&artifact.path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
            ),
        })
        .collect::<Vec<_>>();
    if apply {
        verify_tag_is_absent(component, remote, &tag)?;
        let lookup = run_gh_command(
            gh_command(
                &github,
                &component.github,
                &["api", &format!("repos/{repo}/releases/tags/{tag}")],
            ),
            github_release_upload_timeout(),
        );
        if lookup.exit_code == Some(0) {
            return Err(Error::validation_invalid_argument(
                "candidate",
                "immutable candidate release already exists",
                Some(tag),
                None,
            ));
        }
        if lookup.timed_out || !lookup.stderr.to_ascii_lowercase().contains("404") {
            return Err(Error::validation_invalid_argument(
                "candidate",
                format!(
                    "could not establish candidate release absence: {}",
                    lookup.stderr.trim()
                ),
                None,
                None,
            ));
        }
        let mut args = vec![
            "release".to_string(),
            "create".to_string(),
            tag.clone(),
            "--repo".to_string(),
            repo.clone(),
            "--target".to_string(),
            sha.clone(),
            "--prerelease".to_string(),
            "--latest=false".to_string(),
        ];
        args.extend(artifacts.iter().map(|artifact| artifact.path.clone()));
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let created = run_gh_command(
            gh_command(&github, &component.github, &refs),
            github_release_upload_timeout(),
        );
        if created.timed_out || created.exit_code != Some(0) {
            return Err(Error::validation_invalid_argument(
                "candidate",
                format!(
                    "candidate release creation failed: {}",
                    created.stderr.trim()
                ),
                None,
                None,
            ));
        }
        verify_tag_matches(component, remote, &tag, &sha)?;
        let readback = run_gh_command(
            gh_command(
                &github,
                &component.github,
                &["api", &format!("repos/{repo}/releases/tags/{tag}")],
            ),
            github_release_upload_timeout(),
        );
        if readback.timed_out || readback.exit_code != Some(0) {
            return Err(Error::validation_invalid_argument(
                "candidate",
                format!(
                    "candidate release readback failed: {}",
                    readback.stderr.trim()
                ),
                None,
                None,
            ));
        }
        verify_readback(&readback.stdout, &tag, &assets)?;
    }
    Ok(CandidatePublication {
        source_sha: sha,
        tag,
        assets,
    })
}

fn resolve_sha(path: &str, sha: &str) -> Result<String> {
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::validation_invalid_argument(
            "sha",
            "candidate publication requires a full 40-character commit SHA",
            Some(sha.to_string()),
            None,
        ));
    }
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{sha}^{{commit}}")])
        .current_dir(path)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("resolve candidate SHA".to_string()))
        })?;
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || !resolved.eq_ignore_ascii_case(sha) {
        return Err(Error::validation_invalid_argument(
            "sha",
            "candidate SHA does not resolve in the component checkout",
            Some(sha.to_string()),
            None,
        ));
    }
    Ok(resolved)
}

fn verify_tag_is_absent(component: &Component, remote: &str, tag: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["ls-remote", "--tags", remote, &format!("refs/tags/{tag}")])
        .current_dir(&component.local_path)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("check candidate tag".to_string()))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "could not query remote candidate tag",
            None,
            None,
        ));
    }
    if !output.stdout.is_empty() {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "immutable candidate tag already exists",
            Some(tag.to_string()),
            None,
        ));
    }
    Ok(())
}

fn verify_tag_matches(component: &Component, remote: &str, tag: &str, sha: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["ls-remote", "--tags", remote, &format!("refs/tags/{tag}")])
        .current_dir(&component.local_path)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("verify candidate tag".to_string()))
        })?;
    let actual = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    if !output.status.success() || !actual.eq_ignore_ascii_case(sha) {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "candidate tag does not resolve to the requested source SHA",
            Some(tag.to_string()),
            None,
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct ReleaseReadback {
    tag_name: String,
    prerelease: bool,
    assets: Vec<ReadbackAsset>,
}
#[derive(Deserialize)]
struct ReadbackAsset {
    name: String,
    digest: Option<String>,
}

fn verify_readback(json: &str, tag: &str, assets: &[CandidateAsset]) -> Result<()> {
    let release: ReleaseReadback = serde_json::from_str(json).map_err(|error| {
        Error::validation_invalid_argument(
            "candidate",
            format!("candidate release readback was not valid JSON: {error}"),
            None,
            None,
        )
    })?;
    if release.tag_name != tag || !release.prerelease {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "candidate release readback is not the expected prerelease tag",
            None,
            None,
        ));
    }
    for expected in assets {
        let digest = format!("sha256:{}", expected.sha256);
        if !release.assets.iter().any(|asset| {
            asset.name == expected.name && asset.digest.as_deref() == Some(digest.as_str())
        }) {
            return Err(Error::validation_invalid_argument(
                "candidate",
                format!(
                    "candidate release readback did not verify asset {}",
                    expected.name
                ),
                None,
                None,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn asset(name: &str, digest: &str) -> CandidateAsset {
        CandidateAsset {
            name: name.to_string(),
            sha256: digest.to_string(),
            url: String::new(),
        }
    }
    #[test]
    fn readback_rejects_crossed_asset_digests() {
        assert!(verify_readback(r#"{"tag_name":"candidate-a","prerelease":true,"assets":[{"name":"one","digest":"sha256:b"},{"name":"two","digest":"sha256:a"}]}"#, "candidate-a", &[asset("one", "a"), asset("two", "b")]).is_err());
    }
}
