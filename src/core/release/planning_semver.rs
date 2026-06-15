use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;

use super::types::{ReleaseSemverCommit, ReleaseSemverRecommendation};

pub(super) fn build_semver_recommendation(
    component: &Component,
    requested_bump: &str,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<Option<ReleaseSemverRecommendation>> {
    let (latest_tag, commits) = resolve_tag_and_commits(&component.local_path, monorepo)?;

    if commits.is_empty() {
        return Ok(None);
    }

    // Explicit version strings (e.g. "2.0.0") skip semver keyword parsing.
    // The version is used verbatim: no underbump check, no rank comparison.
    let is_explicit_version =
        requested_bump.contains('.') && requested_bump.split('.').all(|p| p.parse::<u32>().is_ok());

    let recommended = git::recommended_bump_from_commits(&commits);

    if is_explicit_version {
        return Ok(Some(ReleaseSemverRecommendation {
            latest_tag: latest_tag.clone(),
            range: commit_range(latest_tag.as_deref()),
            commits: commit_rows(&commits),
            recommended_bump: recommended.map(|r| r.as_str().to_string()),
            requested_bump: requested_bump.to_string(),
            is_underbump: false,
            reasons: Vec::new(),
        }));
    }

    let requested = git::SemverBump::parse(requested_bump).ok_or_else(|| {
        Error::validation_invalid_argument(
            "bump_type",
            format!("Invalid bump type: {}", requested_bump),
            None,
            Some(vec![
                "Use one of: patch, minor, major, or an explicit version like 2.0.0".to_string(),
            ]),
        )
    })?;

    let is_underbump = recommended
        .map(|r| requested.rank() < r.rank())
        .unwrap_or(false);

    Ok(Some(ReleaseSemverRecommendation {
        latest_tag: latest_tag.clone(),
        range: commit_range(latest_tag.as_deref()),
        commits: commit_rows(&commits),
        recommended_bump: recommended.map(|r| r.as_str().to_string()),
        requested_bump: requested.as_str().to_string(),
        is_underbump,
        reasons: recommendation_reasons(&commits, recommended),
    }))
}

pub(super) fn validate_release_version_floor(
    latest_tag: Option<&str>,
    current_version: &str,
    next_version: &str,
) -> Option<String> {
    let latest_tag = latest_tag?;
    let tag_version = git::extract_version_from_tag(latest_tag)?;
    let tag_version = semver::Version::parse(&tag_version).ok()?;
    let current_version = semver::Version::parse(current_version).ok()?;
    let next_version = semver::Version::parse(next_version).ok()?;

    if tag_version > current_version {
        return Some(format!(
            "Latest release tag {} is ahead of source version {}. Refusing to release {} because this usually means a bad or misplaced tag needs cleanup.",
            latest_tag, current_version, next_version
        ));
    }

    if next_version <= tag_version {
        return Some(format!(
            "Next release version {} is not greater than latest release tag {}. Refusing to create a non-advancing release.",
            next_version, latest_tag
        ));
    }

    None
}

pub(super) fn validate_current_version_tag_reachable(
    local_path: &str,
    monorepo: Option<&git::MonorepoContext>,
    current_version: &str,
) -> Result<Option<String>> {
    let git_root = monorepo
        .map(|ctx| ctx.git_root.as_str())
        .unwrap_or(local_path);
    let tag_name = current_version_tag_name(monorepo, current_version);

    if !git::tag_exists_locally(git_root, &tag_name)? {
        return Ok(None);
    }

    let tag_commit = git::get_tag_commit(git_root, &tag_name)?;
    let output = git::execute_git_for_release(
        git_root,
        &["merge-base", "--is-ancestor", &tag_commit, "HEAD"],
    )
    .map_err(|err| Error::git_command_failed(format!("git merge-base failed: {}", err)))?;
    if output.status.success() {
        return Ok(None);
    }

    Ok(Some(format!(
        "Release tag {} exists for current source version {} but is not reachable from HEAD. Refusing to plan the next release until the orphaned tag is recovered or removed.",
        tag_name, current_version
    )))
}

pub(super) fn current_version_tag_name(
    monorepo: Option<&git::MonorepoContext>,
    current_version: &str,
) -> String {
    monorepo
        .map(|ctx| ctx.format_tag(current_version))
        .unwrap_or_else(|| format!("v{}", current_version))
}

/// Detect whether the release for `current_version` is already published at
/// HEAD: the expected tag exists locally and points at the same commit as HEAD.
///
/// Used by the planner to short-circuit forced re-runs after a prior release
/// already created the tag/release commit, so the operator sees a clear
/// "release already exists" message instead of a downstream changelog
/// contract error for the next version (issue #4316).
pub(super) fn current_version_tag_at_head(
    local_path: &str,
    monorepo: Option<&git::MonorepoContext>,
    current_version: &str,
) -> Result<Option<String>> {
    let git_root = monorepo
        .map(|ctx| ctx.git_root.as_str())
        .unwrap_or(local_path);
    let tag_name = current_version_tag_name(monorepo, current_version);

    if !git::tag_exists_locally(git_root, &tag_name)? {
        return Ok(None);
    }

    let tag_commit = git::get_tag_commit(git_root, &tag_name)?;
    let head_commit = git::get_head_commit(git_root)?;

    if tag_commit == head_commit {
        Ok(Some(tag_name))
    } else {
        Ok(None)
    }
}

/// Resolve the latest tag and commits since that tag for a component.
///
/// In a monorepo, uses component-prefixed tags and path-scoped commits.
/// In a single-repo, uses standard global tags and all commits.
pub(super) fn resolve_tag_and_commits(
    local_path: &str,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<(Option<String>, Vec<git::CommitInfo>)> {
    match monorepo {
        Some(ctx) => {
            let latest_tag = git::get_latest_tag_with_prefix(&ctx.git_root, Some(&ctx.tag_prefix))?;
            let commits = git::get_commits_since_tag_for_path(
                &ctx.git_root,
                latest_tag.as_deref(),
                Some(&ctx.path_prefix),
            )?;
            Ok((latest_tag, commits))
        }
        None => {
            let latest_tag = git::get_latest_tag(local_path)?;
            let commits = git::get_commits_since_tag(local_path, latest_tag.as_deref())?;
            Ok((latest_tag, commits))
        }
    }
}

fn commit_rows(commits: &[git::CommitInfo]) -> Vec<ReleaseSemverCommit> {
    commits
        .iter()
        .map(|c| ReleaseSemverCommit {
            sha: c.hash.clone(),
            subject: c.subject.clone(),
            commit_type: commit_type(&c.category).to_string(),
            breaking: c.category == git::CommitCategory::Breaking,
        })
        .collect()
}

fn commit_type(category: &git::CommitCategory) -> &'static str {
    match category {
        git::CommitCategory::Breaking => "breaking",
        git::CommitCategory::Feature => "feature",
        git::CommitCategory::Fix => "fix",
        git::CommitCategory::Docs => "docs",
        git::CommitCategory::Chore => "chore",
        git::CommitCategory::Merge => "merge",
        git::CommitCategory::Release => "release",
        git::CommitCategory::Other => "other",
    }
}

fn recommendation_reasons(
    commits: &[git::CommitInfo],
    recommended: Option<git::SemverBump>,
) -> Vec<String> {
    commits
        .iter()
        .filter(|c| {
            if let Some(rec) = recommended {
                match rec {
                    git::SemverBump::Major => c.category == git::CommitCategory::Breaking,
                    git::SemverBump::Minor => {
                        c.category == git::CommitCategory::Breaking
                            || c.category == git::CommitCategory::Feature
                    }
                    git::SemverBump::Patch => {
                        matches!(
                            c.category,
                            git::CommitCategory::Breaking
                                | git::CommitCategory::Feature
                                | git::CommitCategory::Fix
                                | git::CommitCategory::Other
                        )
                    }
                }
            } else {
                false
            }
        })
        .take(10)
        .map(|c| format!("{} {}", c.hash, c.subject))
        .collect()
}

fn commit_range(latest_tag: Option<&str>) -> String {
    latest_tag
        .map(|t| format!("{}..HEAD", t))
        .unwrap_or_else(|| "HEAD".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_semver_recommendation, resolve_tag_and_commits,
        validate_current_version_tag_reachable, validate_release_version_floor,
    };
    use crate::core::component::Component;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_file(dir: &std::path::Path, name: &str, content: &str, message: &str) {
        std::fs::write(dir.join(name), content).expect("write fixture file");
        run_git(dir, &["add", name]);
        run_git(dir, &["commit", "-q", "-m", message]);
    }

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Homeboy Test"]);
        temp
    }

    #[test]
    fn test_build_semver_recommendation() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v1.0.0"]);
        commit_file(dir, "feature.txt", "feature", "feat: add planning");
        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };

        let recommendation = build_semver_recommendation(&component, "patch", None)
            .expect("recommendation should build")
            .expect("feature commit should recommend a release");

        assert_eq!(recommendation.latest_tag.as_deref(), Some("v1.0.0"));
        assert_eq!(recommendation.range, "v1.0.0..HEAD");
        assert_eq!(recommendation.recommended_bump.as_deref(), Some("minor"));
        assert_eq!(recommendation.requested_bump, "patch");
        assert!(recommendation.is_underbump);
        assert_eq!(recommendation.commits.len(), 1);
        assert_eq!(recommendation.commits[0].commit_type, "feature");
        assert_eq!(recommendation.reasons.len(), 1);
    }

    #[test]
    fn explicit_version_request_does_not_underbump() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v1.0.0"]);
        commit_file(dir, "breaking.txt", "breaking", "feat!: break API");
        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };

        let recommendation = build_semver_recommendation(&component, "2.0.0", None)
            .expect("recommendation should build")
            .expect("breaking commit should recommend a release");

        assert_eq!(recommendation.recommended_bump.as_deref(), Some("major"));
        assert_eq!(recommendation.requested_bump, "2.0.0");
        assert!(!recommendation.is_underbump);
        assert!(recommendation.reasons.is_empty());
    }

    #[test]
    fn test_resolve_tag_and_commits() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v1.0.0"]);
        commit_file(dir, "fix.txt", "fix", "fix: patch bug");

        let (latest_tag, commits) = resolve_tag_and_commits(&dir.to_string_lossy(), None)
            .expect("tag and commits should resolve");

        assert_eq!(latest_tag.as_deref(), Some("v1.0.0"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: patch bug");
    }

    #[test]
    fn test_validate_release_version_floor() {
        let message = validate_release_version_floor(Some("v0.125.0"), "0.124.9", "0.124.10")
            .expect("ahead tag should block release");

        assert!(message.contains("Latest release tag v0.125.0 is ahead of source version 0.124.9"));
        assert!(message.contains("bad or misplaced tag"));
        assert!(validate_release_version_floor(Some("v0.124.9"), "0.124.9", "0.124.10").is_none());
    }

    #[test]
    fn release_version_floor_blocks_non_advancing_next_version() {
        let message = validate_release_version_floor(Some("v0.125.0"), "0.125.0", "0.125.0")
            .expect("same version should block release");

        assert!(message.contains(
            "Next release version 0.125.0 is not greater than latest release tag v0.125.0"
        ));
    }

    #[test]
    fn current_version_tag_reachability_blocks_orphaned_tag() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v0.1.0"]);
        commit_file(dir, "fix.txt", "fix", "fix: patch bug");
        commit_file(dir, "VERSION", "0.1.1", "release: v0.1.1");
        run_git(dir, &["branch", "-M", "main"]);

        run_git(dir, &["checkout", "--orphan", "orphan-release"]);
        run_git(dir, &["rm", "-qrf", "."]);
        commit_file(dir, "VERSION", "0.1.1", "release: v0.1.1");
        run_git(dir, &["tag", "v0.1.1"]);
        run_git(dir, &["checkout", "main"]);

        let message = validate_current_version_tag_reachable(&dir.to_string_lossy(), None, "0.1.1")
            .expect("validation should run")
            .expect("orphaned current-version tag should block release");

        assert!(message.contains("Release tag v0.1.1 exists"));
        assert!(message.contains("not reachable from HEAD"));
        assert!(message.contains("Refusing to plan the next release"));
    }

    #[test]
    fn current_version_tag_reachability_allows_reachable_tag() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        commit_file(dir, "VERSION", "0.1.1", "release: v0.1.1");
        run_git(dir, &["tag", "v0.1.1"]);

        let message = validate_current_version_tag_reachable(&dir.to_string_lossy(), None, "0.1.1")
            .expect("validation should run");

        assert!(message.is_none());
    }
}
