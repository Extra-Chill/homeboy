use crate::release::changelog;
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

use super::planning_semver::resolve_tag_and_commits;
use super::scope::ReleaseScope;
use super::types::{ReleaseChangelogPlan, ReleaseOptions};

pub(super) fn build_changelog_plan(
    component: &Component,
    options: &ReleaseOptions,
    entries: std::collections::HashMap<String, Vec<String>>,
) -> Result<ReleaseChangelogPlan> {
    let path = changelog::resolve_changelog_path(component)?;
    let entry_count = entries.values().map(Vec::len).sum();

    Ok(ReleaseChangelogPlan {
        policy: "generated".to_string(),
        path: path.to_string_lossy().to_string(),
        dry_run: options.dry_run,
        entries,
        entry_count,
    })
}

/// Generate changelog entries from the commits since the last tag.
///
/// Returns an empty map when the changelog is already ahead of the latest tag
/// or when an empty release is explicitly forced.
pub(super) fn generate_changelog_entries(
    component: &Component,
    component_id: &str,
    options: &ReleaseOptions,
    release_scope: &ReleaseScope,
) -> Result<std::collections::HashMap<String, Vec<String>>> {
    let (latest_tag, commits) = resolve_tag_and_commits(release_scope)?;

    if commits.is_empty() {
        if options.bump_policy.force_empty_release {
            return Ok(std::collections::HashMap::new());
        }

        let tag_desc = latest_tag
            .as_deref()
            .map(|t| format!("tag '{}'", t))
            .unwrap_or_else(|| "the initial commit".to_string());
        return Err(Error::validation_invalid_argument(
            "commits",
            format!("No commits since {} — nothing to release", tag_desc),
            Some(format!("Component: {}", component_id)),
            Some(vec![
                "Homeboy releases are driven by commits. Commit a change, then re-run.".to_string(),
                format!(
                    "Check status: git log {}..HEAD --oneline",
                    latest_tag.as_deref().unwrap_or("")
                )
                .trim_end_matches(' ')
                .to_string(),
            ]),
        ));
    }

    // If the changelog is already finalized ahead of the latest tag, the
    // release commit was produced in a prior run that got interrupted before
    // tagging. No new entries to generate; let the rest of the pipeline finish.
    let changelog_path = changelog::resolve_changelog_path(component)?;
    let changelog_content =
        read_changelog_for_release(component, &changelog_path, options.dry_run)?;
    let latest_changelog_version = changelog::get_latest_finalized_version(&changelog_content);
    if let (Some(latest_tag), Some(changelog_ver_str)) = (&latest_tag, latest_changelog_version) {
        let tag_version = latest_tag.trim_start_matches('v');
        if let (Ok(tag_ver), Ok(cl_ver)) = (
            semver::Version::parse(tag_version),
            semver::Version::parse(&changelog_ver_str),
        ) {
            if cl_ver > tag_ver {
                homeboy_core::log_status!(
                    "release",
                    "Changelog already finalized at {} (ahead of tag {})",
                    changelog_ver_str,
                    latest_tag
                );
                return Ok(std::collections::HashMap::new());
            }
        }
    }

    let releasable: Vec<git::CommitInfo> = commits
        .into_iter()
        .filter(|c| c.category.to_changelog_entry_type().is_some())
        .collect();

    let entries = group_commits_for_changelog(component, &releasable);
    let count: usize = entries.values().map(|v| v.len()).sum();

    homeboy_core::log_status!(
        "release",
        "{} auto-generate {} changelog entries from commits",
        if options.dry_run { "Would" } else { "Will" },
        count,
    );

    Ok(entries)
}

fn read_changelog_for_release(
    component: &Component,
    changelog_path: &std::path::Path,
    dry_run: bool,
) -> Result<String> {
    match homeboy_core::engine::local_files::local().read(changelog_path) {
        Ok(content) => Ok(content),
        Err(err) if dry_run && is_file_not_found_error(&err) => {
            homeboy_core::log_status!(
                "release",
                "Would initialize changelog at {} (first release for {})",
                changelog_path.display(),
                component.id
            );
            Ok(changelog::INITIAL_CHANGELOG_CONTENT.to_string())
        }
        Err(err) => Err(err),
    }
}

fn is_file_not_found_error(err: &Error) -> bool {
    let detail = err
        .details
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or_default();

    err.message.contains("File not found")
        || err.message.contains("No such file")
        || detail.contains("File not found")
        || detail.contains("No such file")
}

/// First-release bootstrap: if the component's configured `changelog_target`
/// doesn't exist on disk (and no fallback candidate exists), create a minimal
/// changelog scaffold so downstream finalization has a file to update.
pub(super) fn ensure_changelog_initialized(component: &Component) -> Result<()> {
    let Some(ref target) = component.changelog_target else {
        return Ok(());
    };

    let configured_path = homeboy_core::paths::resolve_path(&component.local_path, target);
    if configured_path.exists() {
        return Ok(());
    }

    let repo_root = std::path::Path::new(&component.local_path);
    if changelog::discover_changelog_relative_path(repo_root).is_some() {
        return Ok(());
    }

    if let Some(parent) = configured_path.parent() {
        homeboy_core::engine::local_files::local().ensure_dir(parent)?;
    }

    homeboy_core::engine::local_files::local()
        .write(&configured_path, changelog::INITIAL_CHANGELOG_CONTENT)?;

    homeboy_core::log_status!(
        "release",
        "Initialized changelog at {} (first release for {})",
        configured_path.display(),
        component.id
    );

    Ok(())
}

/// Group component-scoped commits into reviewer-facing changelog sections.
fn group_commits_for_changelog(
    component: &Component,
    commits: &[git::CommitInfo],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut entries_by_type: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for commit in commits {
        if let Some(entry_type) = commit.category.to_changelog_entry_type() {
            let message = format_changelog_entry(component, commit);
            entries_by_type
                .entry(entry_type.to_string())
                .or_default()
                .push(message);
        }
    }

    if entries_by_type.is_empty() {
        let fallback = commits
            .iter()
            .find(|c| {
                !matches!(
                    c.category,
                    git::CommitCategory::Docs
                        | git::CommitCategory::Chore
                        | git::CommitCategory::Merge
                        | git::CommitCategory::Release
                )
            })
            .map(|c| format_changelog_entry(component, c))
            .unwrap_or_else(|| "Internal improvements".to_string());

        entries_by_type.insert("changed".to_string(), vec![fallback]);
    }

    entries_by_type
}

/// Render the authoritative component change set for reviewer-facing release
/// notes. Commit subjects retain their GitHub references as clickable links and
/// are attributed to the commit author. A mixed commit is emitted once because
/// the scoped git path query returns each matching commit only once.
fn format_changelog_entry(component: &Component, commit: &git::CommitInfo) -> String {
    let subject = git::strip_conventional_prefix(&commit.subject);
    let subject = github_reference_links(component, &subject);
    match commit_author(component, &commit.hash) {
        Some(author) => format!("{} (by {})", subject, author),
        None => subject,
    }
}

fn commit_author(component: &Component, hash: &str) -> Option<String> {
    let output =
        git::execute_git_for_release(&component.local_path, &["show", "-s", "--format=%an", hash])
            .ok()?;
    if !output.status.success() {
        return None;
    }
    let author = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!author.is_empty()).then_some(author)
}

fn github_reference_links(component: &Component, subject: &str) -> String {
    let remote = component.remote_url.clone().or_else(|| {
        git::release_download::detect_remote_url(std::path::Path::new(&component.local_path))
    });
    let Some(remote) = remote else {
        return subject.to_string();
    };
    let Some(repo) = git::release_download::parse_github_url(&remote) else {
        return subject.to_string();
    };
    let reference = regex::Regex::new(r"#(\d+)").expect("valid GitHub reference regex");
    reference
        .replace_all(subject, |captures: &regex::Captures<'_>| {
            format!(
                "[#{}](https://{}/{}/{}/pull/{})",
                &captures[1], repo.host, repo.owner, repo.repo, &captures[1]
            )
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        build_changelog_plan, ensure_changelog_initialized, generate_changelog_entries,
        group_commits_for_changelog, read_changelog_for_release,
    };
    use crate::release::scope::ReleaseScope;
    use crate::release::types::ReleaseOptions;
    use homeboy_core::component::{CommandScopeConfig, Component, ScopeConfig};
    use homeboy_core::git::{CommitCategory, CommitInfo};

    fn commit(subject: &str, category: CommitCategory) -> CommitInfo {
        CommitInfo {
            hash: "abc1234".to_string(),
            subject: subject.to_string(),
            category,
        }
    }

    fn component_with_changelog_target(
        temp_dir: &tempfile::TempDir,
        target: Option<&str>,
    ) -> Component {
        Component {
            id: "test-component".to_string(),
            local_path: temp_dir.path().to_string_lossy().to_string(),
            remote_path: String::new(),
            changelog_target: target.map(|s| s.to_string()),
            ..Default::default()
        }
    }

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
    fn test_build_changelog_plan() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));
        let changelog_path = temp.path().join("CHANGELOG.md");
        std::fs::write(&changelog_path, "# Changelog\n").unwrap();
        let entries = std::collections::HashMap::from([(
            "added".to_string(),
            vec!["new release planning".to_string()],
        )]);

        let plan = build_changelog_plan(&component, &ReleaseOptions::default(), entries)
            .expect("changelog plan should build");

        assert_eq!(plan.policy, "generated");
        assert_eq!(plan.entry_count, 1);
        assert!(!plan.dry_run);
        assert!(plan.path.ends_with("CHANGELOG.md"));
    }

    #[test]
    fn ensure_changelog_initialized_creates_missing_file() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));

        let changelog_path = temp.path().join("CHANGELOG.md");
        assert!(!changelog_path.exists(), "precondition: no changelog yet");

        ensure_changelog_initialized(&component).expect("preflight should bootstrap");

        let content = std::fs::read_to_string(&changelog_path).expect("file created");
        assert_eq!(content, super::changelog::INITIAL_CHANGELOG_CONTENT);
        assert!(
            !content.contains("## Unreleased"),
            "should NOT pre-create Unreleased section (legacy): {}",
            content
        );
    }

    #[test]
    fn ensure_changelog_initialized_creates_parent_dir_for_nested_target() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("docs/CHANGELOG.md"));

        let docs_dir = temp.path().join("docs");
        assert!(!docs_dir.exists(), "precondition: no docs/ yet");

        ensure_changelog_initialized(&component).expect("preflight should bootstrap");

        assert!(docs_dir.is_dir(), "docs/ parent should be created");
        assert!(
            temp.path().join("docs/CHANGELOG.md").exists(),
            "changelog should land at docs/CHANGELOG.md"
        );
    }

    #[test]
    fn ensure_changelog_initialized_leaves_existing_file_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));
        let changelog_path = temp.path().join("CHANGELOG.md");
        let original = "# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Added\n- real release\n";
        std::fs::write(&changelog_path, original).unwrap();

        ensure_changelog_initialized(&component).expect("no-op on existing file");

        let after = std::fs::read_to_string(&changelog_path).unwrap();
        assert_eq!(after, original, "existing changelog must not be rewritten");
    }

    #[test]
    fn ensure_changelog_initialized_defers_to_existing_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));

        std::fs::create_dir_all(temp.path().join("docs")).unwrap();
        let fallback = temp.path().join("docs/CHANGELOG.md");
        std::fs::write(&fallback, "# Changelog\n\n## [0.1.0] - 2026-01-01\n").unwrap();

        ensure_changelog_initialized(&component).expect("defer to fallback");

        assert!(
            !temp.path().join("CHANGELOG.md").exists(),
            "should not create duplicate when fallback exists"
        );
    }

    #[test]
    fn ensure_changelog_initialized_is_noop_without_configured_target() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, None);

        ensure_changelog_initialized(&component).expect("no-op without target");

        if let Some(entry) = std::fs::read_dir(temp.path()).unwrap().next() {
            let path = entry.unwrap().path();
            panic!("should have created nothing, but found: {}", path.display());
        }
    }

    #[test]
    fn test_generate_changelog_entries() {
        let temp = git_repo();
        let dir = temp.path();
        std::fs::write(dir.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "v1.0.0"]);
        commit_file(
            dir,
            "feature.txt",
            "feature",
            "feat: add release planner (#2478)",
        );
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));
        let release_scope = ReleaseScope::resolve(&component, "fixture").expect("release scope");

        let entries = generate_changelog_entries(
            &component,
            "fixture",
            &ReleaseOptions::default(),
            &release_scope,
        )
        .expect("changelog entries should generate");

        assert_eq!(
            entries["added"],
            vec!["add release planner (#2478) (by Homeboy Test)"]
        );
    }

    #[test]
    fn test_group_commits_for_changelog() {
        let commits = vec![
            commit(
                "feat(#741): delete AgentType class — replace with string literals",
                CommitCategory::Feature,
            ),
            commit(
                "fix(#730): queue-add uses unified check-duplicate",
                CommitCategory::Fix,
            ),
        ];

        let component = Component::default();
        let entries = group_commits_for_changelog(&component, &commits);
        let added = &entries["added"];
        let fixed = &entries["fixed"];

        assert_eq!(
            added[0],
            "delete AgentType class — replace with string literals"
        );
        assert_eq!(fixed[0], "queue-add uses unified check-duplicate");
    }

    #[test]
    fn group_commits_for_changelog_strips_pr_references() {
        let commits = vec![
            commit(
                "feat: agent-first scoping — Phase 1 schema (#738)",
                CommitCategory::Feature,
            ),
            commit(
                "fix: rename $class param — fixes bootstrap crash (#711)",
                CommitCategory::Fix,
            ),
        ];

        let component = Component::default();
        let entries = group_commits_for_changelog(&component, &commits);
        let added = &entries["added"];
        let fixed = &entries["fixed"];

        assert_eq!(added[0], "agent-first scoping — Phase 1 schema (#738)");
        assert_eq!(
            fixed[0],
            "rename $class param — fixes bootstrap crash (#711)"
        );
    }

    #[test]
    fn blocks_engine_shaped_changes_exclude_siblings_and_preserve_attribution() {
        let temp = git_repo();
        let dir = temp.path();
        commit_file(dir, "README.md", "initial", "chore: initial");
        run_git(dir, &["tag", "php-transformer-v0.2.6"]);
        run_git(dir, &["config", "user.name", "PHP Author"]);
        commit_file(
            dir,
            "php-transformer/tools/visual-parity/figma.ts",
            "figma",
            "feat: figma-only change (#912)",
        );
        commit_file(
            dir,
            "php-transformer/src/index.php",
            "php",
            "fix: php-only change (#942)",
        );
        std::fs::write(dir.join("php-transformer/src/index.php"), "mixed").unwrap();
        std::fs::write(
            dir.join("php-transformer/tools/visual-parity/figma.ts"),
            "mixed",
        )
        .unwrap();
        run_git(
            dir,
            &[
                "add",
                "php-transformer/src/index.php",
                "php-transformer/tools/visual-parity/figma.ts",
            ],
        );
        run_git(
            dir,
            &[
                "commit",
                "-q",
                "-m",
                "fix: shared transformer change (#943)",
            ],
        );

        let component = Component {
            id: "php-transformer".to_string(),
            local_path: dir.join("php-transformer").to_string_lossy().to_string(),
            remote_url: Some("https://github.com/Automattic/blocks-engine.git".to_string()),
            scopes: Some(ScopeConfig {
                release: Some(CommandScopeConfig {
                    include: vec![],
                    exclude: vec!["tools/visual-parity/**".to_string()],
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let scope = ReleaseScope::resolve(&component, &component.id).unwrap();
        let (_, commits) = scope.commits_since_latest_tag().unwrap();
        let entries = group_commits_for_changelog(&component, &commits);
        let fixed = &entries["fixed"];

        assert_eq!(
            fixed.len(),
            2,
            "mixed commits are emitted once per component release"
        );
        assert!(fixed.iter().all(|entry| !entry.contains("figma-only")));
        assert!(fixed.iter().any(|entry| {
            entry.contains("php-only change")
                && entry.contains("PHP Author")
                && entry.contains("https://github.com/Automattic/blocks-engine/pull/942")
        }));
        assert_eq!(
            fixed
                .iter()
                .filter(|entry| entry.contains("shared transformer change"))
                .count(),
            1
        );
    }

    #[test]
    fn read_changelog_for_release_uses_seed_for_missing_dry_run_file() {
        let temp = tempfile::tempdir().unwrap();
        let component = component_with_changelog_target(&temp, Some("CHANGELOG.md"));
        let changelog_path = temp.path().join("CHANGELOG.md");

        let content = read_changelog_for_release(&component, &changelog_path, true)
            .expect("dry-run should simulate first-run seed");

        assert_eq!(content, super::changelog::INITIAL_CHANGELOG_CONTENT);
        assert!(
            !changelog_path.exists(),
            "dry-run must not create the changelog on disk"
        );
    }
}
