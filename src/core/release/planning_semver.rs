use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;

use super::scope::ReleaseScope;
use super::types::{ReleaseSemverCommit, ReleaseSemverRecommendation};

pub(super) fn build_semver_recommendation(
    _component: &Component,
    requested_bump: &str,
    scope: &ReleaseScope,
) -> Result<Option<ReleaseSemverRecommendation>> {
    let (latest_tag, commits) = resolve_tag_and_commits(scope)?;

    if commits.is_empty() {
        return Ok(None);
    }

    // Explicit version strings (e.g. "2.0.0") skip semver keyword parsing.
    // The version is used verbatim: no underbump check, no rank comparison.
    let is_explicit_version =
        requested_bump.contains('.') && requested_bump.split('.').all(|p| p.parse::<u32>().is_ok());

    let recommended = git::recommended_bump_from_commits(&commits);

    if requested_bump == "none" && recommended.is_none() {
        return Ok(None);
    }

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
    scope: &ReleaseScope,
    current_version: &str,
) -> Result<Option<String>> {
    let tag_name = current_version_tag_name(scope, current_version);

    if !git::tag_exists_locally(&scope.git_root, &tag_name)? {
        return Ok(None);
    }

    let tag_commit = git::get_tag_commit(&scope.git_root, &tag_name)?;
    let output = git::execute_git_for_release(
        &scope.git_root,
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

pub(super) fn current_version_tag_name(scope: &ReleaseScope, current_version: &str) -> String {
    scope.tag_name(current_version)
}

/// Detect whether the release for `current_version` is already published at
/// HEAD: the expected tag exists locally and points at the same commit as HEAD.
///
/// Used by the planner to short-circuit forced re-runs after a prior release
/// already created the tag/release commit, so the operator sees a clear
/// "release already exists" message instead of a downstream changelog
/// contract error for the next version (issue #4316).
pub(super) fn current_version_tag_at_head(
    scope: &ReleaseScope,
    current_version: &str,
) -> Result<Option<String>> {
    let tag_name = current_version_tag_name(scope, current_version);

    if !git::tag_exists_locally(&scope.git_root, &tag_name)? {
        return Ok(None);
    }

    let tag_commit = git::get_tag_commit(&scope.git_root, &tag_name)?;
    let head_commit = git::get_head_commit(&scope.git_root)?;

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
    scope: &ReleaseScope,
) -> Result<(Option<String>, Vec<git::CommitInfo>)> {
    // Make release tags (and connecting history) available before the
    // reachability/changelog-range guard inspects them. A tagless or shallow
    // release checkout would otherwise report a genuinely-reachable tag as "not
    // reachable from HEAD" and refuse (issue #6916). Best-effort: offline
    // checkouts still fall through to the guard against local history, so a tag
    // that is truly not an ancestor of HEAD is still refused.
    let (latest_tag, commits) = scope.commits_since_latest_tag()?;
    validate_latest_release_tag_reachable(scope, latest_tag.as_deref())?;
    Ok((latest_tag, commits))
}

fn validate_latest_release_tag_reachable(
    scope: &ReleaseScope,
    latest_reachable_tag: Option<&str>,
) -> Result<()> {
    let Some(latest_any_tag) = scope.latest_tag_any()? else {
        return Ok(());
    };

    if latest_reachable_tag == Some(latest_any_tag.as_str()) {
        return Ok(());
    }

    if git::is_ancestor(&scope.git_root, &latest_any_tag, "HEAD")? {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release-range",
        format!(
            "Latest release tag {} is not reachable from HEAD. Refusing to plan changelog entries from {} because that would duplicate a prior release range.",
            latest_any_tag,
            latest_reachable_tag.unwrap_or("the initial commit")
        ),
        Some(format!("Repository: {}", scope.git_root)),
        Some(vec![
            format!(
                "Merge or recover the {} release commit onto the selected release base/default branch, then rerun the release.",
                latest_any_tag
            ),
            format!(
                "Inspect the boundary: git merge-base --is-ancestor {} HEAD",
                latest_any_tag
            ),
        ]),
    ))
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
    use crate::core::component::{CommandScopeConfig, Component, ScopeConfig, VersionTarget};
    use crate::core::release::scope::ReleaseScope;

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
        if let Some(parent) = dir.join(name).parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
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

        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let recommendation = build_semver_recommendation(&component, "patch", &release_scope)
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

        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let recommendation = build_semver_recommendation(&component, "2.0.0", &release_scope)
            .expect("recommendation should build")
            .expect("breaking commit should recommend a release");

        assert_eq!(recommendation.recommended_bump.as_deref(), Some("major"));
        assert_eq!(recommendation.requested_bump, "2.0.0");
        assert!(!recommendation.is_underbump);
        assert!(recommendation.reasons.is_empty());
    }

    #[test]
    fn none_request_with_only_non_releasable_commits_returns_no_recommendation() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v1.0.0"]);
        commit_file(
            dir,
            "baseline.txt",
            "baseline",
            "chore: refresh lint baseline",
        );
        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };

        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let recommendation = build_semver_recommendation(&component, "none", &release_scope)
            .expect("no-op recommendation should be valid");

        assert!(
            recommendation.is_none(),
            "internal no-op bump sentinel should let the planner build a skip plan"
        );
    }

    #[test]
    fn test_resolve_tag_and_commits() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v1.0.0"]);
        commit_file(dir, "fix.txt", "fix", "fix: patch bug");

        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let (latest_tag, commits) =
            resolve_tag_and_commits(&release_scope).expect("tag and commits should resolve");

        assert_eq!(latest_tag.as_deref(), Some("v1.0.0"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: patch bug");
    }

    #[test]
    fn repo_root_component_uses_release_scope_for_commits() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "package-a-v1.0.0"]);
        commit_file(
            dir,
            "packages/package-b/VERSION",
            "1.0.1",
            "fix: update sibling package",
        );
        commit_file(
            dir,
            "packages/package-a/VERSION",
            "1.0.1",
            "fix: update package a",
        );
        let mut component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        component.scopes = Some(ScopeConfig {
            release: Some(CommandScopeConfig {
                include: vec!["packages/package-a/**".to_string()],
                exclude: vec![],
            }),
            ..Default::default()
        });

        let release_scope =
            ReleaseScope::resolve(&component, "package-a").expect("release scope should resolve");
        let (latest_tag, commits) =
            resolve_tag_and_commits(&release_scope).expect("scoped commits should resolve");

        assert_eq!(latest_tag.as_deref(), Some("package-a-v1.0.0"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: update package a");
    }

    #[test]
    fn repo_root_component_infers_commit_scope_from_release_files() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "package-a-v1.0.0"]);
        commit_file(
            dir,
            "packages/package-b/VERSION",
            "1.0.1",
            "fix: update sibling package",
        );
        commit_file(
            dir,
            "packages/package-a/VERSION",
            "1.0.1",
            "fix: update package a",
        );
        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            changelog_target: Some("packages/package-a/docs/CHANGELOG.md".to_string()),
            version_targets: Some(vec![VersionTarget {
                file: "packages/package-a/VERSION".to_string(),
                pattern: None,
                artifact_path: None,
            }]),
            ..Default::default()
        };

        let release_scope = ReleaseScope::resolve(&component, "package-a")
            .expect("release files should create release scope");
        assert_eq!(release_scope.path_prefixes, vec!["packages/package-a"]);
        let (latest_tag, commits) =
            resolve_tag_and_commits(&release_scope).expect("scoped commits should resolve");

        assert_eq!(latest_tag.as_deref(), Some("package-a-v1.0.0"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: update package a");
    }

    #[test]
    fn resolve_tag_and_commits_fails_closed_when_latest_release_tag_is_off_branch() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["branch", "-M", "main"]);
        run_git(dir, &["tag", "v0.1.0"]);
        commit_file(
            dir,
            "feature.txt",
            "first release work",
            "feat: first release work",
        );
        run_git(dir, &["branch", "release-v0.2.0"]);
        run_git(dir, &["checkout", "release-v0.2.0"]);
        commit_file(dir, "VERSION", "0.2.0", "release: v0.2.0");
        run_git(dir, &["tag", "v0.2.0"]);
        run_git(dir, &["checkout", "main"]);
        commit_file(
            dir,
            "fix.txt",
            "second release work",
            "fix: second release work",
        );

        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let err = resolve_tag_and_commits(&release_scope)
            .expect_err("off-branch latest release tag should fail closed");

        assert!(err.message.contains("Latest release tag v0.2.0"));
        assert!(err.message.contains("not reachable from HEAD"));
        assert!(err.message.contains("duplicate a prior release range"));
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

        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let message = validate_current_version_tag_reachable(&release_scope, "0.1.1")
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

        let component = Component {
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let message = validate_current_version_tag_reachable(&release_scope, "0.1.1")
            .expect("validation should run");

        assert!(message.is_none());
    }

    /// Issue #6916: a release checkout that is missing the latest release tag
    /// locally, but where the tag is genuinely reachable on `origin`, must
    /// fetch the tag and proceed past the reachability/changelog-range guard
    /// rather than refusing.
    #[test]
    fn resolve_tag_and_commits_fetches_missing_tag_reachable_on_origin() {
        // Build a real upstream repo: initial commit, a release tag, then a
        // post-release fix commit on the same linear history.
        let origin = git_repo();
        let origin_dir = origin.path();
        commit_file(origin_dir, "README.md", "initial", "chore: initial");
        run_git(origin_dir, &["branch", "-M", "main"]);
        run_git(origin_dir, &["tag", "v0.1.18"]);
        commit_file(origin_dir, "fix.txt", "fix", "fix: patch bug");

        // Materialize a tagless working checkout from origin: the connecting
        // history is present (the tag's commit IS an ancestor of HEAD) but the
        // tag ref itself was never fetched, mirroring the materialization that
        // triggered the false "not reachable from HEAD" refusal.
        let work = tempfile::tempdir().expect("tempdir");
        let work_dir = work.path();
        run_git(
            work_dir,
            &[
                "clone",
                "--no-tags",
                "-q",
                &origin_dir.to_string_lossy(),
                ".",
            ],
        );
        run_git(work_dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(work_dir, &["config", "user.name", "Homeboy Test"]);

        // Precondition: the release tag is genuinely absent locally.
        let listed = std::process::Command::new("git")
            .args(["tag", "-l", "v0.1.18"])
            .current_dir(work_dir)
            .output()
            .expect("git tag -l");
        assert!(
            String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
            "tag should be missing locally before the fetch-before-guard step"
        );

        // resolve_tag_and_commits fetches tags from origin before the guard, so
        // the now-reachable tag is found and the call proceeds.
        let component = Component {
            local_path: work_dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let (latest_tag, commits) = resolve_tag_and_commits(&release_scope)
            .expect("tag reachable on origin should let the release proceed past the guard");

        assert_eq!(latest_tag.as_deref(), Some("v0.1.18"));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "fix: patch bug");
    }

    /// Guard correctness preserved: a tag that is genuinely NOT an ancestor of
    /// HEAD must still refuse, even after the fetch-before-guard step pulls all
    /// tags from origin.
    #[test]
    fn resolve_tag_and_commits_still_refuses_genuinely_unreachable_tag_after_fetch() {
        // Upstream has a tag on an off-branch commit that never merges into the
        // main line that the working checkout tracks.
        let origin = git_repo();
        let origin_dir = origin.path();
        commit_file(origin_dir, "README.md", "initial", "chore: initial");
        run_git(origin_dir, &["branch", "-M", "main"]);
        run_git(origin_dir, &["tag", "v0.1.0"]);
        commit_file(origin_dir, "feature.txt", "work", "feat: first work");
        run_git(origin_dir, &["branch", "release-v0.2.0"]);
        run_git(origin_dir, &["checkout", "-q", "release-v0.2.0"]);
        commit_file(origin_dir, "VERSION", "0.2.0", "release: v0.2.0");
        run_git(origin_dir, &["tag", "v0.2.0"]);
        run_git(origin_dir, &["checkout", "-q", "main"]);
        commit_file(origin_dir, "fix.txt", "more", "fix: second work");

        // Working checkout tracks main and, like the real failure, is missing
        // tags locally. The fetch step will pull v0.2.0, but it remains
        // off-branch — so the guard must still fail closed.
        let work = tempfile::tempdir().expect("tempdir");
        let work_dir = work.path();
        run_git(
            work_dir,
            &[
                "clone",
                "--no-tags",
                "-q",
                "--branch",
                "main",
                &origin_dir.to_string_lossy(),
                ".",
            ],
        );
        run_git(work_dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(work_dir, &["config", "user.name", "Homeboy Test"]);

        let component = Component {
            local_path: work_dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");
        let err = resolve_tag_and_commits(&release_scope)
            .expect_err("off-branch tag must still fail closed after fetching tags");

        assert!(err.message.contains("Latest release tag v0.2.0"));
        assert!(err.message.contains("not reachable from HEAD"));
    }
}
