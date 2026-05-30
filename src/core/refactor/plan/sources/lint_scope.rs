use crate::core::component::Component;
use crate::core::git;
use crate::core::Error;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

pub(super) struct ReleaseOwnedFileSnapshot {
    relative: String,
    existed: bool,
    bytes: Vec<u8>,
}

pub(super) struct LintFixScopeOutcome {
    pub(super) changed_files: Vec<String>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn capture_release_owned_files(
    component: &Component,
    root: &Path,
) -> crate::core::Result<Vec<ReleaseOwnedFileSnapshot>> {
    release_owned_file_paths(component)
        .into_iter()
        .map(|relative| {
            let path = root.join(&relative);
            let existed = path.exists();
            let bytes = if existed {
                fs::read(&path).map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some(format!(
                            "Failed to snapshot release-owned file {}",
                            path.display()
                        )),
                    )
                })?
            } else {
                Vec::new()
            };

            Ok(ReleaseOwnedFileSnapshot {
                relative,
                existed,
                bytes,
            })
        })
        .collect()
}

pub(super) fn restore_release_owned_files(
    root: &Path,
    snapshots: &[ReleaseOwnedFileSnapshot],
) -> crate::core::Result<()> {
    for snapshot in snapshots {
        let path = root.join(&snapshot.relative);
        if snapshot.existed {
            fs::write(&path, &snapshot.bytes).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "Failed to restore release-owned file {}",
                        path.display()
                    )),
                )
            })?;
        } else if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "Failed to remove generated release-owned file {}",
                        path.display()
                    )),
                )
            })?;
        }
    }

    Ok(())
}

pub(super) fn constrain_lint_fix_changes(
    root: &Path,
    selected_files: Option<&[String]>,
    before_dirty: &[String],
    after_dirty: Vec<String>,
    release_owned: &[ReleaseOwnedFileSnapshot],
) -> crate::core::Result<LintFixScopeOutcome> {
    let before_set: HashSet<&str> = before_dirty.iter().map(|file| file.as_str()).collect();
    let release_owned_set: HashSet<&str> = release_owned
        .iter()
        .map(|snapshot| snapshot.relative.as_str())
        .collect();
    let newly_changed: Vec<String> = after_dirty
        .into_iter()
        .filter(|file| {
            !before_set.contains(file.as_str()) && !release_owned_set.contains(file.as_str())
        })
        .collect();

    let Some(selected_files) = selected_files else {
        return Ok(LintFixScopeOutcome {
            changed_files: newly_changed,
            warnings: Vec::new(),
        });
    };

    let selected_set: HashSet<&str> = selected_files.iter().map(|file| file.as_str()).collect();
    let mut allowed = Vec::new();
    let mut out_of_scope = Vec::new();

    for file in newly_changed {
        if selected_set.contains(file.as_str()) {
            allowed.push(file);
        } else {
            out_of_scope.push(file);
        }
    }

    let mut warnings = Vec::new();
    if !out_of_scope.is_empty() {
        git::discard_worktree_changes(&root.to_string_lossy(), &out_of_scope)?;
        warnings.push(format!(
            "Discarded {} lint autofix change(s) outside selected scope: {}",
            out_of_scope.len(),
            out_of_scope.join(", ")
        ));
    }

    Ok(LintFixScopeOutcome {
        changed_files: allowed,
        warnings,
    })
}

pub(super) fn reject_unsafe_lint_autofix_changes(
    root: &Path,
    changed_files: &[String],
) -> crate::core::Result<()> {
    if changed_files.is_empty() {
        return Ok(());
    }

    let diff = lint_autofix_diff(root, changed_files)?;
    let violations = unsafe_signature_changes(&diff);
    if violations.is_empty() {
        return Ok(());
    }

    git::discard_worktree_changes(&root.to_string_lossy(), changed_files)?;

    Err(Error::validation_invalid_argument(
        "fix",
        format!(
            "Unsafe lint autofix reverted: changed function or method signature(s): {}",
            violations.join("; ")
        ),
        None,
        Some(vec![
            "Run lint without --fix and apply behavior-affecting changes manually".to_string(),
            "Autofix is limited to edits that preserve callable signatures".to_string(),
        ]),
    ))
}

fn lint_autofix_diff(root: &Path, changed_files: &[String]) -> crate::core::Result<String> {
    let output = Command::new("git")
        .arg("diff")
        .arg("--unified=0")
        .arg("--")
        .args(changed_files)
        .current_dir(root)
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), Some("run git diff".to_string())))?;

    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git diff failed while checking lint autofix safety: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn unsafe_signature_changes(diff: &str) -> Vec<String> {
    let mut removed = Vec::new();
    let mut added = BTreeSet::new();

    for line in diff.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }

        if line.is_empty() {
            continue;
        }
        let (prefix, body) = line.split_at(1);
        if prefix != "+" && prefix != "-" {
            continue;
        }

        let body = body.trim();
        let Some(signature) = normalized_signature_line(body) else {
            continue;
        };

        if prefix == "+" {
            added.insert(signature);
        } else {
            removed.push((body.to_string(), signature));
        }
    }

    removed
        .into_iter()
        .filter_map(|(line, signature)| (!added.contains(&signature)).then_some(line))
        .collect()
}

fn normalized_signature_line(line: &str) -> Option<String> {
    if !looks_like_signature_line(line) {
        return None;
    }

    let without_body = line
        .split_once('{')
        .map_or(line, |(head, _)| head)
        .trim_end_matches(';')
        .trim();

    Some(
        without_body
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect(),
    )
}

fn looks_like_signature_line(line: &str) -> bool {
    if !line.contains('(') || !line.contains(')') {
        return false;
    }

    let trimmed = line.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    let tokens: Vec<&str> = lowered
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .collect();

    tokens.iter().any(|token| {
        matches!(
            *token,
            "fn" | "function" | "def" | "sub" | "func" | "method"
        )
    })
}

pub(super) fn lint_finding_scope_files(
    findings: &[crate::core::finding::HomeboyFinding],
) -> Vec<String> {
    let mut files = BTreeSet::new();
    for finding in findings {
        let Some(file) = finding.location.file.as_deref() else {
            continue;
        };
        if let Some(normalized) = normalize_relative_release_path(file) {
            files.insert(normalized);
        }
    }
    files.into_iter().collect()
}

pub(super) fn lint_scope_glob(root_str: &str, files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }

    let abs_files: Vec<String> = files
        .iter()
        .map(|file| format!("{}/{}", root_str, file))
        .collect();
    if abs_files.len() == 1 {
        Some(abs_files[0].clone())
    } else {
        Some(format!("{{{}}}", abs_files.join(",")))
    }
}

pub(super) fn release_owned_file_paths(component: &Component) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();

    if let Some(changelog) = component.changelog_target.as_deref() {
        if let Some(path) = normalize_relative_release_path(changelog) {
            paths.insert(path);
        }
    }

    if let Some(targets) = component.version_targets.as_deref() {
        for target in targets {
            if let Some(path) = normalize_relative_release_path(&target.file) {
                paths.insert(path);
            }
        }
    }

    paths
}

fn normalize_relative_release_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return None;
    }

    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }

    (!parts.is_empty()).then(|| parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::{Component, VersionTarget};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-refactor-sources-{name}-{nanos}"))
    }

    fn test_component(root: &Path) -> Component {
        Component {
            id: "component".to_string(),
            local_path: root.to_string_lossy().to_string(),
            remote_path: String::new(),
            ..Default::default()
        }
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn release_owned_file_paths_include_changelog_and_version_targets() {
        let root = PathBuf::from("/tmp/homeboy-release-owned-files");
        let mut component = test_component(&root);
        component.changelog_target = Some("./CHANGELOG.md".to_string());
        component.version_targets = Some(vec![
            VersionTarget {
                file: "Cargo.toml".to_string(),
                pattern: None,
            },
            VersionTarget {
                file: "nested/package.json".to_string(),
                pattern: None,
            },
        ]);

        let paths = release_owned_file_paths(&component);

        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            vec!["CHANGELOG.md", "Cargo.toml", "nested/package.json"]
        );
    }

    #[test]
    fn release_owned_snapshots_restore_existing_and_remove_generated_files() {
        let root = tmp_dir("release-owned-snapshot");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("CHANGELOG.md"), "before\n").unwrap();

        let mut component = test_component(&root);
        component.changelog_target = Some("CHANGELOG.md".to_string());
        component.version_targets = Some(vec![VersionTarget {
            file: "Cargo.toml".to_string(),
            pattern: None,
        }]);

        let snapshots = capture_release_owned_files(&component, &root).unwrap();
        fs::write(root.join("CHANGELOG.md"), "after\n").unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nversion = \"1.0.0\"\n").unwrap();

        restore_release_owned_files(&root, &snapshots).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("CHANGELOG.md")).unwrap(),
            "before\n"
        );
        assert!(!root.join("Cargo.toml").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_fix_scope_discards_changes_outside_selected_files() {
        let root = tmp_dir("lint-fix-scope");
        fs::create_dir_all(root.join("src")).unwrap();
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.email", "test@example.com"]);
        run_git(&root, &["config", "user.name", "test"]);

        fs::write(root.join("src/scoped.rs"), "before scoped\n").unwrap();
        fs::write(root.join("src/unrelated.rs"), "before unrelated\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "init"]);

        fs::write(root.join("src/scoped.rs"), "after scoped\n").unwrap();
        fs::write(root.join("src/unrelated.rs"), "after unrelated\n").unwrap();
        fs::write(root.join("src/generated.rs"), "generated\n").unwrap();

        let after_dirty = git::get_dirty_files(&root.to_string_lossy()).unwrap();
        let selected_files = vec!["src/scoped.rs".to_string()];
        let outcome =
            constrain_lint_fix_changes(&root, Some(&selected_files), &[], after_dirty, &[])
                .unwrap();

        assert_eq!(outcome.changed_files, vec!["src/scoped.rs"]);
        assert_eq!(
            fs::read_to_string(root.join("src/scoped.rs")).unwrap(),
            "after scoped\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("src/unrelated.rs")).unwrap(),
            "before unrelated\n"
        );
        assert!(!root.join("src/generated.rs").exists());
        assert!(outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("outside selected scope")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unsafe_signature_changes_ignores_signature_formatting_only() {
        let diff = r#"
diff --git a/src/lib.php b/src/lib.php
@@ -1 +1 @@
-public function warm( $args){
+public function warm( $args ) {
"#;

        assert!(unsafe_signature_changes(diff).is_empty());
    }

    #[test]
    fn unsafe_signature_changes_detects_removed_parameter() {
        let diff = r#"
diff --git a/src/lib.php b/src/lib.php
@@ -1 +1 @@
-public function warm( $args, $assoc_args ) {
+public function warm( $args) {
"#;

        let violations = unsafe_signature_changes(diff);

        assert_eq!(
            violations,
            vec!["public function warm( $args, $assoc_args ) {"]
        );
    }

    #[test]
    fn unsafe_lint_autofix_reverts_signature_changes() {
        let root = tmp_dir("lint-autofix-signature-safety");
        fs::create_dir_all(root.join("src")).unwrap();
        run_git(&root, &["init", "-q"]);
        run_git(&root, &["config", "user.email", "test@example.com"]);
        run_git(&root, &["config", "user.name", "test"]);

        let original = "<?php\npublic function warm( $args, $assoc_args ) {\n}\n";
        fs::write(root.join("src/command.php"), original).unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-q", "-m", "init"]);

        fs::write(
            root.join("src/command.php"),
            "<?php\npublic function warm( $args) {\n}\n",
        )
        .unwrap();

        let changed_files = vec!["src/command.php".to_string()];
        let error = reject_unsafe_lint_autofix_changes(&root, &changed_files)
            .expect_err("signature edits should be rejected");

        assert!(error.message.contains("Unsafe lint autofix reverted"));
        assert_eq!(
            fs::read_to_string(root.join("src/command.php")).unwrap(),
            original
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_finding_scope_files_normalizes_reported_files() {
        let findings = vec![
            crate::core::finding::HomeboyFinding::builder("lint", "message")
                .file("src/b.php")
                .build(),
            crate::core::finding::HomeboyFinding::builder("lint", "message")
                .file("./src/a.php")
                .build(),
            crate::core::finding::HomeboyFinding::builder("lint", "message")
                .file("src/b.php")
                .build(),
            crate::core::finding::HomeboyFinding::builder("lint", "message")
                .file("../outside.php")
                .build(),
            crate::core::finding::HomeboyFinding::builder("lint", "message").build(),
        ];

        assert_eq!(
            lint_finding_scope_files(&findings),
            vec!["src/a.php".to_string(), "src/b.php".to_string()]
        );
    }
}
