use crate::core::component::{resolve_component_scope, Component, ScopeCommand};
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

pub(super) fn release_monorepo_context(
    component: &Component,
    component_id: &str,
) -> Option<git::MonorepoContext> {
    let mut context = git::MonorepoContext::detect(&component.local_path, component_id);
    let extra_prefixes = release_scope_prefixes(component);

    if let Some(ctx) = context.as_mut() {
        for prefix in extra_prefixes {
            let component_prefix = ctx.path_prefix.trim_end_matches('/');
            let scoped = if prefix == component_prefix
                || prefix.starts_with(&format!("{}/", component_prefix))
            {
                prefix
            } else {
                format!("{}/{}", component_prefix, prefix)
            };
            if !ctx.path_prefixes.contains(&scoped) {
                ctx.path_prefixes.push(scoped);
            }
        }
        return context;
    }

    if extra_prefixes.is_empty() {
        return None;
    }

    let git_root = git::get_git_root(&component.local_path).ok()?;
    Some(git::MonorepoContext {
        git_root,
        path_prefix: extra_prefixes[0].clone(),
        path_prefixes: extra_prefixes,
        tag_prefix: component_id.to_string(),
    })
}

fn release_scope_prefixes(component: &Component) -> Vec<String> {
    let scope = resolve_component_scope(component, ScopeCommand::Release);
    let mut prefixes: Vec<String> = scope
        .include
        .iter()
        .filter_map(|path| normalize_release_scope_path(path))
        .collect();

    if prefixes.is_empty() {
        if let Some(prefix) = infer_common_release_prefix(component) {
            prefixes.push(prefix);
        }
    }

    prefixes.sort();
    prefixes.dedup();
    prefixes
}

fn infer_common_release_prefix(component: &Component) -> Option<String> {
    let mut paths = Vec::new();

    if let Some(targets) = component.version_targets.as_ref() {
        paths.extend(
            targets
                .iter()
                .filter_map(|target| normalize_release_scope_path(&target.file)),
        );
    }

    if let Some(target) = component.changelog_target.as_ref() {
        if let Some(path) = normalize_release_scope_path(target) {
            paths.push(path);
        }
    }

    common_directory_prefix(&paths)
}

fn normalize_release_scope_path(path: &str) -> Option<String> {
    let mut value = path.trim().trim_start_matches("./").trim_matches('/');
    if value.is_empty() || value == "." {
        return None;
    }

    if let Some(wildcard) = value.find('*') {
        value = value[..wildcard].trim_end_matches('/');
    }

    if value.is_empty() || value == "." {
        return None;
    }

    Some(value.to_string())
}

fn common_directory_prefix(paths: &[String]) -> Option<String> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut prefix: Vec<&str> = first.split('/').collect();
    if prefix.len() <= 1 {
        return None;
    }
    prefix.pop();

    for path in iter {
        let mut dirs: Vec<&str> = path.split('/').collect();
        if dirs.len() <= 1 {
            return None;
        }
        dirs.pop();

        let keep = prefix
            .iter()
            .zip(dirs.iter())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(keep);
        if prefix.is_empty() {
            return None;
        }
    }

    Some(prefix.join("/"))
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
    // Make release tags (and connecting history) available before the
    // reachability/changelog-range guard inspects them. A tagless or shallow
    // release checkout would otherwise report a genuinely-reachable tag as "not
    // reachable from HEAD" and refuse (issue #6916). Best-effort: offline
    // checkouts still fall through to the guard against local history, so a tag
    // that is truly not an ancestor of HEAD is still refused.
    let guard_root = monorepo
        .map(|ctx| ctx.git_root.as_str())
        .unwrap_or(local_path);
    git::fetch_tags(guard_root)?;

    match monorepo {
        Some(ctx) => {
            let latest_tag = git::get_latest_tag_with_prefix(&ctx.git_root, Some(&ctx.tag_prefix))?;
            validate_latest_release_tag_reachable(
                &ctx.git_root,
                latest_tag.as_deref(),
                Some(&ctx.tag_prefix),
            )?;
            let path_prefixes: Vec<&str> = ctx.path_prefixes.iter().map(String::as_str).collect();
            let commits = git::get_commits_since_tag_for_paths(
                &ctx.git_root,
                latest_tag.as_deref(),
                &path_prefixes,
            )?;
            Ok((latest_tag, commits))
        }
        None => {
            let latest_tag = git::get_latest_tag(local_path)?;
            validate_latest_release_tag_reachable(local_path, latest_tag.as_deref(), None)?;
            let commits = git::get_commits_since_tag(local_path, latest_tag.as_deref())?;
            Ok((latest_tag, commits))
        }
    }
}

fn validate_latest_release_tag_reachable(
    git_root: &str,
    latest_reachable_tag: Option<&str>,
    tag_prefix: Option<&str>,
) -> Result<()> {
    let Some(latest_any_tag) = git::get_latest_tag_any_with_prefix(git_root, tag_prefix)? else {
        return Ok(());
    };

    if latest_reachable_tag == Some(latest_any_tag.as_str()) {
        return Ok(());
    }

    if git::is_ancestor(git_root, &latest_any_tag, "HEAD")? {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release-range",
        format!(
            "Latest release tag {} is not reachable from HEAD. Refusing to plan changelog entries from {} because that would duplicate a prior release range.",
            latest_any_tag,
            latest_reachable_tag.unwrap_or("the initial commit")
        ),
        Some(format!("Repository: {}", git_root)),
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
        build_semver_recommendation, release_monorepo_context, resolve_tag_and_commits,
        validate_current_version_tag_reachable, validate_release_version_floor,
    };
    use crate::core::component::{CommandScopeConfig, Component, ScopeConfig, VersionTarget};

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

        let recommendation = build_semver_recommendation(&component, "none", None)
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

        let (latest_tag, commits) = resolve_tag_and_commits(&dir.to_string_lossy(), None)
            .expect("tag and commits should resolve");

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

        let monorepo = release_monorepo_context(&component, "package-a")
            .expect("release scope should create monorepo context");
        let (latest_tag, commits) = resolve_tag_and_commits(&component.local_path, Some(&monorepo))
            .expect("scoped commits should resolve");

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

        let monorepo = release_monorepo_context(&component, "package-a")
            .expect("release files should create monorepo context");
        assert_eq!(monorepo.path_prefixes, vec!["packages/package-a"]);
        let (latest_tag, commits) = resolve_tag_and_commits(&component.local_path, Some(&monorepo))
            .expect("scoped commits should resolve");

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

        let err = resolve_tag_and_commits(&dir.to_string_lossy(), None)
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
        let (latest_tag, commits) = resolve_tag_and_commits(&work_dir.to_string_lossy(), None)
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

        let err = resolve_tag_and_commits(&work_dir.to_string_lossy(), None)
            .expect_err("off-branch tag must still fail closed after fetching tags");

        assert!(err.message.contains("Latest release tag v0.2.0"));
        assert!(err.message.contains("not reachable from HEAD"));
    }
}
