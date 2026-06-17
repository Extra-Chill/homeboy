use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::{git, Error, Result};

const BUILTIN_ARTIFACT_PATHS: &[(&str, &str)] = &[
    ("target", "rust_target"),
    ("node_modules", "node_modules"),
    ("dist", "generated_dist"),
];

#[derive(Debug, Clone, Default)]
pub struct ArtifactCleanupOptions {
    pub path: Option<PathBuf>,
    pub apply: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub root: String,
    pub worktree_count: usize,
    pub candidate_count: usize,
    pub skipped_count: usize,
    pub applied_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub candidates: Vec<ArtifactCleanupCandidate>,
    pub skipped: Vec<ArtifactCleanupSkipped>,
    pub applied: Vec<ArtifactCleanupApplied>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupCandidate {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub declared_by: String,
    pub size_bytes: u64,
    pub source_dirty: bool,
    pub unpushed_commits: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupSkipped {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactCleanupApplied {
    pub worktree: String,
    pub path: String,
    pub relative_path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeInfo {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactDeclaration {
    relative_path: String,
    kind: String,
    declared_by: String,
}

#[derive(Debug, Default)]
struct GitSafety {
    source_dirty: bool,
    unpushed_commits: bool,
    dirty_paths: Vec<String>,
}

pub fn cleanup_artifacts(options: ArtifactCleanupOptions) -> Result<ArtifactCleanupOutput> {
    let root = resolve_root(options.path.as_deref())?;
    let worktrees = discover_worktrees(&root)?;
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    let mut applied = Vec::new();

    for worktree in &worktrees {
        let safety = git_safety(&worktree.path)?;
        for declaration in artifact_declarations(&worktree.path)? {
            let artifact_path = worktree.path.join(&declaration.relative_path);
            let display_path = artifact_path.to_string_lossy().to_string();
            if !artifact_path.exists() {
                continue;
            }
            if !is_safe_artifact_path(&declaration.relative_path) {
                skipped.push(skip_row(
                    worktree,
                    &declaration,
                    display_path,
                    "declared artifact path is not a safe repo-relative path",
                ));
                continue;
            }
            if has_tracked_changes_under(&safety.dirty_paths, &declaration.relative_path) {
                skipped.push(skip_row(
                    worktree,
                    &declaration,
                    display_path,
                    "artifact path contains tracked or staged source changes",
                ));
                continue;
            }

            let size_bytes = path_size(&artifact_path)?;
            let candidate = ArtifactCleanupCandidate {
                worktree: worktree.path.to_string_lossy().to_string(),
                path: display_path.clone(),
                relative_path: declaration.relative_path.clone(),
                kind: declaration.kind.clone(),
                declared_by: declaration.declared_by.clone(),
                size_bytes,
                source_dirty: safety.source_dirty,
                unpushed_commits: safety.unpushed_commits,
            };

            if options.apply {
                remove_artifact_path(&artifact_path)?;
                applied.push(ArtifactCleanupApplied {
                    worktree: candidate.worktree.clone(),
                    path: candidate.path.clone(),
                    relative_path: candidate.relative_path.clone(),
                    kind: candidate.kind.clone(),
                    size_bytes,
                    removed: true,
                });
            }

            candidates.push(candidate);
        }
    }

    let estimated_bytes = candidates.iter().map(|row| row.size_bytes).sum();
    let reclaimed_bytes = applied.iter().map(|row| row.size_bytes).sum();

    Ok(ArtifactCleanupOutput {
        command: "cleanup.artifacts",
        mode: if options.apply { "apply" } else { "dry_run" },
        root: root.to_string_lossy().to_string(),
        worktree_count: worktrees.len(),
        candidate_count: candidates.len(),
        skipped_count: skipped.len(),
        applied_count: applied.len(),
        estimated_bytes,
        reclaimed_bytes,
        candidates,
        skipped,
        applied,
    })
}

fn resolve_root(path: Option<&Path>) -> Result<PathBuf> {
    let start = match path {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|e| {
            Error::internal_io(e.to_string(), Some("read current directory".to_string()))
        })?,
    };
    git_root(&start)
}

fn discover_worktrees(root: &Path) -> Result<Vec<WorktreeInfo>> {
    let output = git::run_git(
        root,
        &["worktree", "list", "--porcelain"],
        "git worktree list",
    )?;
    let mut worktrees = Vec::new();
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.push(WorktreeInfo {
                path: PathBuf::from(path),
            });
        }
    }
    if worktrees.is_empty() {
        worktrees.push(WorktreeInfo {
            path: root.to_path_buf(),
        });
    }
    Ok(worktrees)
}

fn artifact_declarations(worktree: &Path) -> Result<Vec<ArtifactDeclaration>> {
    let mut declarations = Vec::new();
    for (relative_path, kind) in BUILTIN_ARTIFACT_PATHS {
        declarations.push(ArtifactDeclaration {
            relative_path: (*relative_path).to_string(),
            kind: (*kind).to_string(),
            declared_by: "builtin".to_string(),
        });
    }

    let config_path = worktree.join("homeboy.json");
    if config_path.exists() {
        let raw = fs::read_to_string(&config_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read {}", config_path.display())),
            )
        })?;
        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some(format!("parse {}", config_path.display())),
            )
        })?;
        for path in value
            .get("artifact_cleanup_paths")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            declarations.push(ArtifactDeclaration {
                relative_path: path.to_string(),
                kind: "declared_artifact".to_string(),
                declared_by: "homeboy.json:artifact_cleanup_paths".to_string(),
            });
        }
    }

    let mut seen = HashSet::new();
    declarations.retain(|row| seen.insert(row.relative_path.clone()));
    Ok(declarations)
}

fn git_safety(worktree: &Path) -> Result<GitSafety> {
    let status = git::run_git(worktree, &["status", "--porcelain=v1"], "git status")?;
    let mut dirty_paths = Vec::new();
    let mut source_dirty = false;
    for line in status.lines() {
        if line.len() < 4 || line.starts_with("?? ") || line.starts_with("!! ") {
            continue;
        }
        let path = status_path(line);
        if !path.is_empty() {
            source_dirty = true;
            dirty_paths.push(path);
        }
    }

    let unpushed_commits = match git::run_git(
        worktree,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        "git rev-list upstream",
    ) {
        Ok(count) => count.trim().parse::<u32>().unwrap_or(0) > 0,
        Err(_) => false,
    };

    Ok(GitSafety {
        source_dirty,
        unpushed_commits,
        dirty_paths,
    })
}

fn status_path(line: &str) -> String {
    let raw = line.get(3..).unwrap_or_default();
    raw.rsplit(" -> ")
        .next()
        .unwrap_or(raw)
        .trim_matches('"')
        .to_string()
}

fn has_tracked_changes_under(dirty_paths: &[String], relative_path: &str) -> bool {
    let prefix = format!("{}/", relative_path.trim_end_matches('/'));
    dirty_paths
        .iter()
        .any(|path| path == relative_path || path.starts_with(&prefix))
}

fn skip_row(
    worktree: &WorktreeInfo,
    declaration: &ArtifactDeclaration,
    path: String,
    reason: &str,
) -> ArtifactCleanupSkipped {
    ArtifactCleanupSkipped {
        worktree: worktree.path.to_string_lossy().to_string(),
        path,
        relative_path: declaration.relative_path.clone(),
        kind: declaration.kind.clone(),
        reason: reason.to_string(),
    }
}

fn is_safe_artifact_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    !relative_path.is_empty()
        && relative_path != "."
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn path_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }

    let mut total = metadata.len();
    for entry in fs::read_dir(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read directory {}", path.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read directory entry {}", path.display())),
            )
        })?;
        total += path_size(&entry.path())?;
    }
    Ok(total)
}

fn remove_artifact_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("remove directory {}", path.display())),
            )
        })
    } else {
        fs::remove_file(path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("remove file {}", path.display())),
            )
        })
    }
}

fn git_root(path: &Path) -> Result<PathBuf> {
    let output = git::run_git(path, &["rev-parse", "--show-toplevel"], "git root")?;
    Ok(PathBuf::from(output.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn safe_artifact_paths_are_repo_relative() {
        assert!(is_safe_artifact_path("target"));
        assert!(is_safe_artifact_path("runtime/generated-fixture"));
        assert!(!is_safe_artifact_path(""));
        assert!(!is_safe_artifact_path("."));
        assert!(!is_safe_artifact_path("./target"));
        assert!(!is_safe_artifact_path("../target"));
        assert!(!is_safe_artifact_path("/tmp/target"));
    }

    #[test]
    fn tracked_changes_under_artifact_path_are_detected() {
        let dirty = vec!["target/generated.rs".to_string(), "src/lib.rs".to_string()];
        assert!(has_tracked_changes_under(&dirty, "target"));
        assert!(!has_tracked_changes_under(&dirty, "node_modules"));
    }

    #[test]
    fn declared_artifact_paths_are_loaded_from_homeboy_json() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("homeboy.json"),
            r#"{"artifact_cleanup_paths":["runtime/generated-fixture","dist"]}"#,
        )
        .expect("write config");

        let declarations = artifact_declarations(tmp.path()).expect("declarations");

        assert!(declarations.iter().any(|row| row.relative_path == "target"));
        assert!(declarations
            .iter()
            .any(|row| row.relative_path == "runtime/generated-fixture"));
        assert_eq!(
            declarations
                .iter()
                .filter(|row| row.relative_path == "dist")
                .count(),
            1,
            "declared paths should not duplicate builtins"
        );
    }

    #[test]
    fn dry_run_reports_artifact_candidates_across_worktrees() {
        let repo = git_repo();
        let sibling_parent = TempDir::new().expect("sibling parent");
        let sibling = sibling_parent.path().join("artifact-worktree");
        git(repo.path(), &["worktree", "add", sibling.to_str().unwrap()]);
        write_file(&repo.path().join("target/debug/app"), "primary artifact");
        write_file(
            &sibling.join("node_modules/pkg/index.js"),
            "dependency artifact",
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: false,
        })
        .expect("dry-run cleanup");

        assert_eq!(output.mode, "dry_run");
        assert_eq!(output.applied_count, 0);
        assert!(output.candidates.iter().any(|row| row
            .worktree
            .ends_with(repo.path().file_name().unwrap().to_str().unwrap())
            && row.relative_path == "target"));
        assert!(output
            .candidates
            .iter()
            .any(|row| row.worktree.ends_with("artifact-worktree")
                && row.relative_path == "node_modules"));
        assert!(repo.path().join("target/debug/app").exists());
        assert!(sibling.join("node_modules/pkg/index.js").exists());
    }

    #[test]
    fn apply_removes_declared_artifacts_only_and_preserves_dirty_source() {
        let repo = git_repo();
        write_file(&repo.path().join("target/debug/app"), "artifact");
        write_file(&repo.path().join("src/lib.rs"), "changed source");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
        })
        .expect("apply cleanup");

        assert_eq!(output.mode, "apply");
        assert_eq!(output.applied_count, 1);
        assert!(!repo.path().join("target").exists());
        assert_eq!(
            fs::read_to_string(repo.path().join("src/lib.rs")).expect("read source"),
            "changed source"
        );
        assert!(output.candidates.iter().any(|row| row.source_dirty));
    }

    #[test]
    fn apply_skips_artifact_path_with_tracked_source_changes() {
        let repo = git_repo();
        write_file(
            &repo.path().join("target/generated.rs"),
            "tracked artifact source",
        );
        git(repo.path(), &["add", "target/generated.rs"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "track generated source",
            ],
        );
        write_file(
            &repo.path().join("target/generated.rs"),
            "modified tracked source",
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
        })
        .expect("apply cleanup");

        assert_eq!(output.applied_count, 0);
        assert!(repo.path().join("target/generated.rs").exists());
        assert!(output.skipped.iter().any(|row| {
            row.relative_path == "target" && row.reason.contains("tracked or staged source changes")
        }));
    }

    fn git_repo() -> TempDir {
        let repo = TempDir::new().expect("repo tempdir");
        git(repo.path(), &["init", "-b", "main"]);
        write_file(&repo.path().join("src/lib.rs"), "source");
        git(repo.path(), &["add", "src/lib.rs"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );
        repo
    }

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir parent");
        fs::write(path, content).expect("write file");
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
