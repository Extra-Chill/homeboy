use clap::Args;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

use homeboy::core::component;
use homeboy::core::error::{Error, Result};

use super::CmdResult;

/// Publish immutable assets for a specific source commit without creating a
/// stable release or moving GitHub's latest release alias.
#[derive(Args)]
pub struct CandidateArgs {
    /// Component whose GitHub remote receives the candidate release
    component_id: String,

    /// Full 40-character commit SHA to bind into the candidate release
    #[arg(long, value_name = "SHA")]
    sha: String,

    /// Source-authority manifest directory created by Homeboy packaging or CI.
    #[arg(long, value_name = "DIR")]
    from_artifacts: std::path::PathBuf,

    /// Version recorded in the source-authority manifest.
    #[arg(long)]
    version: String,

    /// Create the GitHub prerelease. Without this flag, print the immutable publication plan.
    #[arg(long)]
    apply: bool,
}

#[derive(Serialize)]
pub struct CandidatePublication {
    pub component_id: String,
    pub source_sha: String,
    pub tag: String,
    pub prerelease: bool,
    pub latest: bool,
    pub assets: Vec<CandidateAsset>,
    pub applied: bool,
}

#[derive(Serialize)]
pub struct CandidateAsset {
    pub name: String,
    pub url: String,
    pub sha256: String,
}

struct CandidateRepository {
    host: String,
    slug: String,
}

pub(super) fn run(args: CandidateArgs) -> CmdResult<CandidatePublication> {
    let component = component::load(&args.component_id)?;
    let publication = homeboy_release::release::publish_candidate(
        &component,
        &args.component_id,
        &args.sha,
        &args.from_artifacts,
        &args.version,
        args.apply,
    )?;
    return Ok((
        CandidatePublication {
            component_id: args.component_id,
            source_sha: publication.source_sha,
            tag: publication.tag,
            prerelease: true,
            latest: false,
            assets: publication
                .assets
                .into_iter()
                .map(|asset| CandidateAsset {
                    name: asset.name,
                    url: asset.url,
                    sha256: asset.sha256,
                })
                .collect(),
            applied: args.apply,
        },
        0,
    ));
    #[allow(unreachable_code)]
    let sha = resolve_sha(Path::new(&component.local_path), &args.sha)?;
    let repo = github_repo(&component)?;
    let tag = format!("candidate-{sha}");
    let authority = homeboy_release::release::source_authority_artifacts(
        &args.from_artifacts,
        &args.component_id,
        &tag,
        &args.version,
        &sha,
    )?;
    let assets = candidate_assets(&repo, &tag, &authority)?;

    if args.apply {
        ensure_release_is_new(&repo.slug, &tag)?;
        let mut command = Command::new("gh");
        command.args([
            "release",
            "create",
            &tag,
            "--repo",
            &repo.slug,
            "--target",
            &sha,
            "--prerelease",
            "--latest=false",
            "--title",
            &format!("Candidate {sha}"),
            "--notes",
            &format!("Immutable candidate publication for source commit `{sha}`."),
        ]);
        command.args(authority.iter().map(|asset| asset.path.as_str()));
        let output = command.output().map_err(|error| {
            Error::validation_invalid_argument(
                "candidate",
                format!("failed to run gh release create: {error}"),
                None,
                Some(vec![
                    "Install and authenticate the GitHub CLI, then retry.".to_string()
                ]),
            )
        })?;
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "candidate",
                format!(
                    "GitHub candidate release creation failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                None,
                None,
            ));
        }
    }

    Ok((
        CandidatePublication {
            component_id: args.component_id,
            source_sha: sha,
            tag,
            prerelease: true,
            latest: false,
            assets,
            applied: args.apply,
        },
        0,
    ))
}

fn resolve_sha(path: &Path, sha: &str) -> Result<String> {
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
            "candidate SHA does not resolve to the requested commit in the component checkout",
            Some(sha.to_string()),
            Some(vec![
                "Fetch the PR head SHA before publishing its candidate assets.".to_string(),
            ]),
        ));
    }
    Ok(resolved)
}

fn github_repo(component: &homeboy::core::component::Component) -> Result<CandidateRepository> {
    let remote = component.remote_url.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "candidate",
            "component has no configured remote URL",
            None,
            None,
        )
    })?;
    let github =
        homeboy::core::git::release_download::parse_github_url(remote).ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate",
                "candidate publication requires a GitHub remote",
                None,
                None,
            )
        })?;
    Ok(CandidateRepository {
        host: github.host,
        slug: format!("{}/{}", github.owner, github.repo),
    })
}

fn candidate_assets(
    repository: &CandidateRepository,
    tag: &str,
    paths: &[homeboy_release::release::SourceAuthorityArtifact],
) -> Result<Vec<CandidateAsset>> {
    let mut names = std::collections::BTreeSet::new();
    paths
        .iter()
        .map(|artifact| {
            let path = Path::new(&artifact.path);
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "asset",
                        "candidate asset has no UTF-8 file name",
                        Some(path.display().to_string()),
                        None,
                    )
                })?;
            if !names.insert(name.to_string()) {
                return Err(Error::validation_invalid_argument(
                    "asset",
                    format!("candidate assets have duplicate filename '{name}'"),
                    Some(path.display().to_string()),
                    None,
                ));
            }
            Ok(CandidateAsset {
                name: name.to_string(),
                sha256: artifact.sha256.clone(),
                url: format!(
                    "https://{}/{}/releases/download/{tag}/{name}",
                    repository.host, repository.slug
                ),
            })
        })
        .collect()
}

fn ensure_release_is_new(repo: &str, tag: &str) -> Result<()> {
    let output = Command::new("gh")
        .args(["release", "view", tag, "--repo", repo])
        .output()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("check candidate release".to_string()),
            )
        })?;
    if output.status.success() {
        return Err(Error::validation_invalid_argument(
            "candidate",
            format!("immutable candidate release '{tag}' already exists"),
            None,
            Some(vec![
                "Use its existing asset URLs; candidate releases are never overwritten."
                    .to_string(),
            ]),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_tag_is_deterministic_and_not_a_stable_tag() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            format!("candidate-{sha}"),
            "candidate-0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn malformed_sha_is_rejected_before_git_access() {
        let error = resolve_sha(Path::new("."), "abc").expect_err("short SHA must fail");
        assert!(error.to_string().contains("full 40-character commit SHA"));
    }

    #[test]
    fn candidate_assets_use_the_immutable_prerelease_url() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let asset = directory.path().join("site.zip");
        std::fs::write(&asset, "candidate bytes").expect("write asset");
        let repository = CandidateRepository {
            host: "github.com".to_string(),
            slug: "example/site".to_string(),
        };
        let assets = candidate_assets(
            &repository,
            "candidate-deadbeef",
            &[homeboy_release::release::SourceAuthorityArtifact {
                path: asset.display().to_string(),
                sha256: "a".repeat(64),
            }],
        )
        .expect("candidate assets");

        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "site.zip");
        assert_eq!(assets[0].sha256, "a".repeat(64));
        assert_eq!(
            assets[0].url,
            "https://github.com/example/site/releases/download/candidate-deadbeef/site.zip"
        );
    }
}
