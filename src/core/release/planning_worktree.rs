use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git::UncommittedChanges;
use crate::core::release::changelog;
use crate::core::release::version::ComponentVersionInfo;

use super::types::ReleaseOptions;

/// Apply release working-tree policy after changelog/version planning has resolved
/// the exact generated files that may be dirty.
pub(super) fn validate_release_worktree(
    component: &Component,
    options: &ReleaseOptions,
    version_info: &ComponentVersionInfo,
) -> Result<Option<serde_json::Value>> {
    if options.pipeline.head && options.pipeline.from_artifacts.is_some() {
        return Ok(None);
    }

    let uncommitted = crate::core::git::get_uncommitted_changes(&component.local_path)?;
    if !uncommitted.has_changes {
        return Ok(None);
    }

    let changelog_path = changelog::resolve_changelog_path(component)?;
    let version_targets: Vec<String> = version_info
        .targets
        .iter()
        .map(|t| t.full_path.clone())
        .collect();

    let allowed = get_release_allowed_files(
        &changelog_path,
        &version_targets,
        std::path::Path::new(&component.local_path),
    );
    let unexpected = get_unexpected_uncommitted_files(&uncommitted, &allowed);

    if !unexpected.is_empty() {
        return Ok(Some(serde_json::json!({
            "files": unexpected,
            "hint": "Commit changes or stash before release"
        })));
    }

    if !options.dry_run {
        // Only changelog/version files are uncommitted; stage them so the
        // release commit includes generated release metadata.
        log_status!(
            "release",
            "Auto-staging changelog/version files for release commit"
        );
        let all_files: Vec<&String> = uncommitted
            .staged
            .iter()
            .chain(uncommitted.unstaged.iter())
            .collect();
        for file in all_files {
            let full_path = std::path::Path::new(&component.local_path).join(file);
            let _ = std::process::Command::new("git")
                .args(["add", &full_path.to_string_lossy()])
                .current_dir(&component.local_path)
                .output();
        }
    }

    Ok(None)
}

/// Stage 0 fail-fast: refuse to run release work when the working tree has
/// unexplained dirty files before lint/test/build can drown out the real error.
pub(super) fn validate_working_tree_fail_fast(component: &Component) -> Result<()> {
    let uncommitted = crate::core::git::get_uncommitted_changes(&component.local_path)?;
    if !uncommitted.has_changes {
        return Ok(());
    }

    let all_files: Vec<String> = uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .cloned()
        .collect();

    let unexpected = filter_homeboy_managed(all_files);
    if unexpected.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "working_tree",
        "Uncommitted changes detected — refusing to release",
        None,
        Some(vec![
            "Commit, stash, or discard changes before releasing".to_string(),
            format!(
                "Unexpected dirty files ({}): {}{}",
                unexpected.len(),
                unexpected
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if unexpected.len() > 10 { ", …" } else { "" }
            ),
        ]),
    ))
}

const HOMEBOY_MANAGED_PREFIXES: &[&str] = &[
    ".homeboy-build/",
    ".homeboy-build",
    ".homeboy-bin/",
    ".homeboy-bin",
    ".homeboy/",
    ".homeboy",
];

fn is_homeboy_managed_path(rel_path: &str) -> bool {
    HOMEBOY_MANAGED_PREFIXES
        .iter()
        .any(|prefix| rel_path == *prefix || rel_path.starts_with(prefix))
}

pub(super) fn filter_homeboy_managed(files: Vec<String>) -> Vec<String> {
    files
        .into_iter()
        .filter(|f| !is_homeboy_managed_path(f))
        .collect()
}

pub(super) fn get_release_allowed_files(
    changelog_path: &std::path::Path,
    version_targets: &[String],
    repo_root: &std::path::Path,
) -> Vec<String> {
    let mut allowed = Vec::new();

    if let Ok(relative) = changelog_path.strip_prefix(repo_root) {
        allowed.push(relative.to_string_lossy().to_string());
    }

    for target in version_targets {
        if let Ok(relative) = std::path::Path::new(target).strip_prefix(repo_root) {
            let relative = relative.to_string_lossy().to_string();
            allowed.push(relative.clone());
            allowed.extend(derived_release_lockfiles(&relative));
        }
    }

    allowed.sort();
    allowed.dedup();
    allowed
}

fn derived_release_lockfiles(version_target: &str) -> Vec<String> {
    let path = std::path::Path::new(version_target);
    let parent = path
        .parent()
        .and_then(|parent| parent.to_str())
        .unwrap_or("");
    let prefix = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/")
    };

    match path.file_name().and_then(|file| file.to_str()) {
        Some("Cargo.toml") => vec![format!("{prefix}Cargo.lock")],
        Some("package.json") => vec![
            format!("{prefix}package-lock.json"),
            format!("{prefix}pnpm-lock.yaml"),
            format!("{prefix}yarn.lock"),
        ],
        Some("composer.json") => vec![format!("{prefix}composer.lock")],
        _ => Vec::new(),
    }
}

fn get_unexpected_uncommitted_files(
    uncommitted: &UncommittedChanges,
    allowed: &[String],
) -> Vec<String> {
    let all_uncommitted: Vec<&String> = uncommitted
        .staged
        .iter()
        .chain(uncommitted.unstaged.iter())
        .chain(uncommitted.untracked.iter())
        .collect();

    all_uncommitted
        .into_iter()
        .filter(|f| !is_homeboy_managed_path(f))
        .filter(|f| !allowed.iter().any(|a| f.ends_with(a) || a.ends_with(*f)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        filter_homeboy_managed, get_release_allowed_files, get_unexpected_uncommitted_files,
        is_homeboy_managed_path, validate_release_worktree, validate_working_tree_fail_fast,
    };
    use crate::core::component::Component;
    use crate::core::git::UncommittedChanges;
    use crate::core::release::types::ReleaseOptions;
    use crate::core::release::version::{ComponentVersionInfo, VersionTargetInfo};

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

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Homeboy Test"]);
        temp
    }

    fn git_component(dir: &std::path::Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            changelog_target: Some("CHANGELOG.md".to_string()),
            ..Default::default()
        }
    }

    fn version_info(dir: &std::path::Path) -> ComponentVersionInfo {
        ComponentVersionInfo {
            version: "1.0.0".to_string(),
            targets: vec![VersionTargetInfo {
                file: "manifest.toml".to_string(),
                pattern: "version".to_string(),
                full_path: dir.join("manifest.toml").to_string_lossy().to_string(),
                match_count: 1,
                warning: None,
            }],
        }
    }

    fn uncommitted(staged: &[&str], unstaged: &[&str], untracked: &[&str]) -> UncommittedChanges {
        UncommittedChanges {
            has_changes: !staged.is_empty() || !unstaged.is_empty() || !untracked.is_empty(),
            staged: staged.iter().map(|s| s.to_string()).collect(),
            unstaged: unstaged.iter().map(|s| s.to_string()).collect(),
            untracked: untracked.iter().map(|s| s.to_string()).collect(),
            hint: None,
        }
    }

    #[test]
    fn test_validate_release_worktree() {
        let temp = git_repo();
        let dir = temp.path();
        std::fs::write(dir.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        std::fs::write(dir.join("manifest.toml"), "version = \"1.0.0\"\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "chore: initial"]);
        std::fs::write(dir.join("src.rs"), "unexpected\n").unwrap();

        let details = validate_release_worktree(
            &git_component(dir),
            &ReleaseOptions::default(),
            &version_info(dir),
        )
        .expect("worktree policy should inspect changes")
        .expect("unexpected user file should be reported");

        let files = details
            .get("files")
            .and_then(|value| value.as_array())
            .expect("details should include dirty files");
        assert_eq!(files[0].as_str(), Some("src.rs"));
    }

    #[test]
    fn head_release_artifacts_skip_release_worktree_validation() {
        let temp = git_repo();
        let dir = temp.path();
        std::fs::write(dir.join("CHANGELOG.md"), "# Changelog\n").unwrap();
        std::fs::write(dir.join("manifest.toml"), "version = \"1.0.0\"\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "chore: initial"]);
        std::fs::create_dir(dir.join("artifacts")).unwrap();
        std::fs::write(dir.join("artifacts/release.zip"), "fixture\n").unwrap();

        let details = validate_release_worktree(
            &git_component(dir),
            &ReleaseOptions {
                pipeline: crate::core::release::types::ReleasePipelineOptions {
                    head: true,
                    from_artifacts: Some("artifacts".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            &version_info(dir),
        )
        .expect("head artifact releases should allow artifact directories");

        assert!(details.is_none());
    }

    #[test]
    fn test_validate_working_tree_fail_fast() {
        let temp = git_repo();
        let dir = temp.path();
        std::fs::write(dir.join("README.md"), "initial\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "chore: initial"]);
        std::fs::write(dir.join("src.rs"), "unexpected\n").unwrap();

        let err = validate_working_tree_fail_fast(&git_component(dir))
            .expect_err("unexpected user file should fail fast");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("Uncommitted changes detected"));
        assert!(err.details.to_string().contains("src.rs"));
    }

    #[test]
    fn homeboy_build_dir_is_managed_path() {
        assert!(is_homeboy_managed_path(".homeboy-build/artifact.zip"));
        assert!(is_homeboy_managed_path(".homeboy-build/"));
        assert!(is_homeboy_managed_path(".homeboy-build"));
    }

    #[test]
    fn homeboy_bin_dir_is_managed_path() {
        assert!(is_homeboy_managed_path(".homeboy-bin/homeboy"));
        assert!(is_homeboy_managed_path(".homeboy-bin"));
    }

    #[test]
    fn homeboy_scratch_dir_is_managed_path() {
        assert!(is_homeboy_managed_path(".homeboy/cache"));
    }

    #[test]
    fn user_paths_are_not_managed() {
        assert!(!is_homeboy_managed_path("src/main.rs"));
        assert!(!is_homeboy_managed_path("docs/changelog.md"));
        assert!(!is_homeboy_managed_path("homeboy.json"));
        assert!(!is_homeboy_managed_path(".gitignore"));
        assert!(!is_homeboy_managed_path("src/.homeboy-build/foo"));
    }

    #[test]
    fn test_filter_homeboy_managed() {
        let files = vec![
            ".homeboy-build/artifact.zip".to_string(),
            "src/main.rs".to_string(),
            ".homeboy-bin/homeboy".to_string(),
            "manifest.toml".to_string(),
        ];
        let filtered = filter_homeboy_managed(files);
        assert_eq!(filtered, vec!["src/main.rs", "manifest.toml"]);
    }

    #[test]
    fn unexpected_files_skip_homeboy_build_dir() {
        let changes = uncommitted(&[], &[], &[".homeboy-build/sample-plugin-0.70.1.zip"]);
        let unexpected = get_unexpected_uncommitted_files(&changes, &[]);
        assert!(
            unexpected.is_empty(),
            "homeboy-managed scratch should never trigger working_tree error, got: {:?}",
            unexpected
        );
    }

    #[test]
    fn unexpected_files_still_catch_user_changes() {
        let changes = uncommitted(&["src/lib.rs"], &[], &[".homeboy-build/foo"]);
        let unexpected = get_unexpected_uncommitted_files(&changes, &[]);
        assert_eq!(unexpected, vec!["src/lib.rs"]);
    }

    #[test]
    fn unexpected_files_honor_allowed_list_alongside_homeboy_filter() {
        let changes = uncommitted(
            &["docs/changelog.md", "manifest.toml"],
            &[],
            &[".homeboy-build/foo"],
        );
        let allowed = vec!["docs/changelog.md".to_string(), "manifest.toml".to_string()];
        let unexpected = get_unexpected_uncommitted_files(&changes, &allowed);
        assert!(
            unexpected.is_empty(),
            "allowed files + homeboy scratch should yield clean result, got: {:?}",
            unexpected
        );
    }

    #[test]
    fn release_allowed_files_are_only_declared_targets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo_root = temp_dir.path();
        let changelog = repo_root.join("docs/changelog.md");
        let manifest = repo_root.join("manifest.toml");

        let allowed = get_release_allowed_files(
            &changelog,
            &[manifest.to_string_lossy().to_string()],
            repo_root,
        );

        assert_eq!(allowed, vec!["docs/changelog.md", "manifest.toml"]);
    }

    #[test]
    fn release_allowed_files_include_lockfiles_derived_from_version_targets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let repo_root = temp_dir.path();
        let changelog = repo_root.join("docs/changelog.md");
        let cargo = repo_root.join("Cargo.toml");
        let package = repo_root.join("packages/app/package.json");
        let composer = repo_root.join("plugin/composer.json");

        let allowed = get_release_allowed_files(
            &changelog,
            &[
                cargo.to_string_lossy().to_string(),
                package.to_string_lossy().to_string(),
                composer.to_string_lossy().to_string(),
            ],
            repo_root,
        );

        for expected in [
            "Cargo.lock",
            "Cargo.toml",
            "docs/changelog.md",
            "packages/app/package-lock.json",
            "packages/app/package.json",
            "packages/app/pnpm-lock.yaml",
            "packages/app/yarn.lock",
            "plugin/composer.json",
            "plugin/composer.lock",
        ] {
            assert!(
                allowed.contains(&expected.to_string()),
                "allowed release files should include {expected}, got {allowed:?}"
            );
        }
    }
}
