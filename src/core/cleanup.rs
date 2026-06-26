use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::{git, Error, Result};

mod self_artifacts;

#[cfg(test)]
use self_artifacts::validate_homeboy_manifest_dir;
use self_artifacts::{homeboy_source_checkout, self_temp_artifact_candidates};

const BUILTIN_ARTIFACT_PATHS: &[(&str, &str)] = &[
    ("build", "generated_build"),
    ("target", "build_target"),
    ("node_modules", "node_modules"),
    ("dist", "generated_dist"),
];

#[derive(Debug, Clone, Default)]
pub struct ArtifactCleanupOptions {
    pub path: Option<PathBuf>,
    pub apply: bool,
    pub self_artifacts: bool,
    pub temp_roots: Vec<PathBuf>,
    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream (ancestor or patch-equivalent / squash-merged). This
    /// keeps in-progress cooks' build dirs intact while reclaiming the large
    /// `target/` dirs left behind by merged worktrees.
    pub merged_only: bool,
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
pub(crate) struct ArtifactDeclaration {
    pub(crate) relative_path: String,
    pub(crate) kind: String,
    pub(crate) declared_by: String,
}

#[derive(Debug, Default)]
struct GitSafety {
    source_dirty: bool,
    unpushed_commits: bool,
    dirty_paths: Vec<String>,
}

pub fn cleanup_artifacts(options: ArtifactCleanupOptions) -> Result<ArtifactCleanupOutput> {
    let root = resolve_root(&options)?;
    let worktrees = discover_worktrees(&root)?;
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    let mut applied = Vec::new();

    for worktree in &worktrees {
        let safety = git_safety(&worktree.path)?;
        if options.merged_only && !branch_is_merged(&worktree.path) {
            for declaration in artifact_declarations(&worktree.path)? {
                let artifact_path = worktree.path.join(&declaration.relative_path);
                if !artifact_path.exists() {
                    continue;
                }
                skipped.push(skip_row(
                    worktree,
                    &declaration,
                    artifact_path.to_string_lossy().to_string(),
                    "worktree branch is not merged into its upstream",
                ));
            }
            continue;
        }
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
                applied.push(applied_row(&candidate));
            }

            candidates.push(candidate);
        }
    }

    for candidate in self_temp_artifact_candidates(&options)? {
        if options.apply {
            remove_artifact_path(Path::new(&candidate.path))?;
            applied.push(applied_row(&candidate));
        }

        candidates.push(candidate);
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

fn resolve_root(options: &ArtifactCleanupOptions) -> Result<PathBuf> {
    if options.path.is_some() && options.self_artifacts {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            "cannot be combined with path",
            None,
            None,
        ));
    }

    let start = match options.path.as_deref() {
        Some(path) => path.to_path_buf(),
        None if options.self_artifacts => homeboy_source_checkout()?,
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

pub(crate) fn artifact_declarations(worktree: &Path) -> Result<Vec<ArtifactDeclaration>> {
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

/// Returns true when the worktree's current branch is already merged into its
/// upstream tracking branch. "Merged" covers three git-native cases, so it is
/// agnostic to merge strategy and ecosystem:
///   1. HEAD has no commits ahead of `@{upstream}` (fast-forward / ancestor).
///   2. Every commit ahead of `@{upstream}` is reported as already-applied by
///      `git cherry` (prefix `-`), i.e. patch-equivalent — the rebase merge.
///   3. Same patch-equivalence covers squash-merges whose single commit lands
///      upstream with a matching patch-id.
///
/// When upstream cannot be resolved (no tracking branch) the worktree is
/// treated as NOT merged, so its artifacts are preserved conservatively.
fn branch_is_merged(worktree: &Path) -> bool {
    let ahead = match git::run_git(
        worktree,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        "git rev-list upstream",
    ) {
        Ok(count) => count.trim().parse::<u32>().unwrap_or(u32::MAX),
        Err(_) => return false,
    };
    if ahead == 0 {
        return true;
    }

    // Commits exist ahead of upstream; treat as merged only if git reports
    // every one of them as already applied upstream (patch-equivalent).
    match git::run_git(
        worktree,
        &["cherry", "@{upstream}", "HEAD"],
        "git cherry upstream",
    ) {
        Ok(output) => {
            let mut saw_commit = false;
            for line in output.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                saw_commit = true;
                // `+ <sha>` means the commit is NOT present upstream.
                if line.starts_with('+') {
                    return false;
                }
            }
            saw_commit
        }
        Err(_) => false,
    }
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

fn applied_row(candidate: &ArtifactCleanupCandidate) -> ArtifactCleanupApplied {
    ArtifactCleanupApplied {
        worktree: candidate.worktree.clone(),
        path: candidate.path.clone(),
        relative_path: candidate.relative_path.clone(),
        kind: candidate.kind.clone(),
        size_bytes: candidate.size_bytes,
        removed: true,
    }
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

pub(crate) fn is_safe_artifact_path(relative_path: &str) -> bool {
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
    fn self_artifact_manifest_must_be_homeboy_crate() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"other\"\n",
        )
        .expect("write manifest");

        let err = validate_homeboy_manifest_dir(tmp.path()).expect_err("reject non-homeboy crate");

        assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn self_artifact_manifest_resolves_homeboy_crate() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        )
        .expect("write manifest");

        let root = validate_homeboy_manifest_dir(tmp.path()).expect("homeboy manifest");

        assert_eq!(root, tmp.path());
    }

    #[test]
    fn self_artifacts_cannot_be_combined_with_explicit_path() {
        let tmp = TempDir::new().expect("tempdir");
        let err = resolve_root(&ArtifactCleanupOptions {
            path: Some(tmp.path().to_path_buf()),
            apply: false,
            self_artifacts: true,
            temp_roots: Vec::new(),
            merged_only: false,
        })
        .expect_err("reject ambiguous cleanup root");

        assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
    }

    #[test]
    fn detached_homeboy_temp_artifacts_are_detected_conservatively() {
        let temp_root = TempDir::new().expect("temp root");
        fs::create_dir_all(temp_root.path().join("homeboy-4483-target/debug"))
            .expect("mkdir target artifact");
        fs::create_dir_all(temp_root.path().join("homeboy-target-4318/debug"))
            .expect("mkdir target artifact");
        fs::create_dir_all(temp_root.path().join("homeboy-d6b2bc65-build"))
            .expect("mkdir build artifact");
        fs::create_dir_all(temp_root.path().join("homeboy-runtime-helper-path"))
            .expect("mkdir non-artifact temp");
        fs::create_dir_all(temp_root.path().join("homeboy-main-source-28703209"))
            .expect("mkdir source temp");
        fs::write(
            temp_root
                .path()
                .join("homeboy-main-source-28703209/Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        )
        .expect("write source manifest");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("temp artifact candidates");

        assert_eq!(candidates.len(), 3);
        assert!(candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-4483-target"));
        assert!(candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-target-4318"));
        assert!(candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-d6b2bc65-build"));
        assert!(!candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-runtime-helper-path"));
        assert!(!candidates
            .iter()
            .any(|row| row.relative_path == "homeboy-main-source-28703209"));
    }

    #[test]
    fn apply_removes_detached_temp_artifacts_from_explicit_temp_root() {
        let repo = git_repo();
        let temp_root = TempDir::new().expect("temp root");
        write_file(
            &temp_root.path().join("homeboy-4477-target/debug/homeboy"),
            "binary",
        );
        write_file(
            &temp_root
                .path()
                .join("homeboy-main-source-28703209/src/lib.rs"),
            "source",
        );

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("apply cleanup");

        assert!(output
            .candidates
            .iter()
            .any(|row| row.kind == "detached_homeboy_temp_artifact"
                && row.relative_path == "homeboy-4477-target"));
        assert!(!temp_root.path().join("homeboy-4477-target").exists());
        assert!(temp_root
            .path()
            .join("homeboy-main-source-28703209")
            .exists());
    }

    #[test]
    fn temp_homeboy_source_checkout_targets_are_detected_conservatively() {
        let temp_root = TempDir::new().expect("temp root");
        let checkout = temp_homeboy_checkout(temp_root.path(), "homeboy-main-source-28703209");
        write_file(&checkout.join("target/debug/homeboy"), "binary");

        let non_homeboy = temp_root.path().join("homeboy-runtime-helper-path");
        fs::create_dir_all(non_homeboy.join(".git")).expect("mkdir git");
        write_file(
            &non_homeboy.join("Cargo.toml"),
            "[package]\nname = \"other\"\n",
        );
        write_file(&non_homeboy.join("target/debug/other"), "binary");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("temp artifact candidates");

        let candidate = candidates
            .iter()
            .find(|row| row.kind == "temp_homeboy_checkout_target")
            .expect("homeboy checkout target candidate");
        assert_eq!(candidate.worktree, checkout.to_string_lossy());
        assert_eq!(candidate.path, checkout.join("target").to_string_lossy());
        assert_eq!(candidate.relative_path, "target");
        assert_eq!(candidate.declared_by, "self_temp_root");
        assert!(!candidates
            .iter()
            .any(|row| row.worktree == non_homeboy.to_string_lossy()));
    }

    #[test]
    fn apply_removes_only_target_from_temp_homeboy_source_checkout() {
        let repo = git_repo();
        let temp_root = TempDir::new().expect("temp root");
        let checkout = temp_homeboy_checkout(temp_root.path(), "homeboy-main-4447-upgrade-full");
        write_file(&checkout.join("target/debug/homeboy"), "binary");
        write_file(&checkout.join("src/lib.rs"), "changed source");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("apply cleanup");

        assert!(output.candidates.iter().any(|row| {
            row.kind == "temp_homeboy_checkout_target" && row.worktree == checkout.to_string_lossy()
        }));
        assert!(!checkout.join("target").exists());
        assert!(checkout.join(".git").exists());
        assert_eq!(
            fs::read_to_string(checkout.join("src/lib.rs")).expect("read source"),
            "changed source"
        );
    }

    #[test]
    fn temp_homeboy_source_checkout_target_with_tracked_changes_is_skipped() {
        let temp_root = TempDir::new().expect("temp root");
        let checkout = temp_homeboy_checkout(temp_root.path(), "homeboy-main-4447-upgrade");
        write_file(
            &checkout.join("target/generated.rs"),
            "tracked target source",
        );
        git(&checkout, &["add", "target/generated.rs"]);

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("temp artifact candidates");

        assert!(!candidates
            .iter()
            .any(|row| row.kind == "temp_homeboy_checkout_target"));
    }

    #[test]
    fn partial_homeboy_temp_target_is_detected_when_source_skeleton_is_empty() {
        let temp_root = TempDir::new().expect("temp root");
        let partial = temp_root.path().join("homeboy-upgrade-sync-main");
        fs::create_dir_all(partial.join(".github")).expect("mkdir github");
        fs::create_dir_all(partial.join("docs")).expect("mkdir docs");
        fs::create_dir_all(partial.join("src")).expect("mkdir src");
        fs::create_dir_all(partial.join("tests")).expect("mkdir tests");
        write_file(&partial.join("target/debug/homeboy"), "binary");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("temp artifact candidates");

        let candidate = candidates
            .iter()
            .find(|row| row.kind == "partial_homeboy_temp_target")
            .expect("partial temp target candidate");
        assert_eq!(candidate.worktree, partial.to_string_lossy());
        assert_eq!(candidate.path, partial.join("target").to_string_lossy());
        assert_eq!(candidate.relative_path, "target");
    }

    #[test]
    fn partial_homeboy_temp_target_is_skipped_when_source_skeleton_has_content() {
        let temp_root = TempDir::new().expect("temp root");
        let partial = temp_root.path().join("homeboy-upgrade-sync-main");
        write_file(&partial.join("src/lib.rs"), "source");
        write_file(&partial.join("target/debug/homeboy"), "binary");

        let candidates = self_temp_artifact_candidates(&ArtifactCleanupOptions {
            path: None,
            apply: false,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("temp artifact candidates");

        assert!(!candidates
            .iter()
            .any(|row| row.kind == "partial_homeboy_temp_target"));
    }

    #[test]
    fn apply_removes_only_target_from_partial_homeboy_temp() {
        let repo = git_repo();
        let temp_root = TempDir::new().expect("temp root");
        let partial = temp_root.path().join("homeboy-upgrade-sync-main");
        fs::create_dir_all(partial.join("src")).expect("mkdir src");
        write_file(&partial.join("target/debug/homeboy"), "binary");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: vec![temp_root.path().to_path_buf()],
            merged_only: false,
        })
        .expect("apply cleanup");

        assert!(output.candidates.iter().any(|row| {
            row.kind == "partial_homeboy_temp_target" && row.worktree == partial.to_string_lossy()
        }));
        assert!(!partial.join("target").exists());
        assert!(partial.join("src").exists());
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
            self_artifacts: false,
            temp_roots: Vec::new(),
            merged_only: false,
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
            self_artifacts: false,
            temp_roots: Vec::new(),
            merged_only: false,
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
            self_artifacts: false,
            temp_roots: Vec::new(),
            merged_only: false,
        })
        .expect("apply cleanup");

        assert_eq!(output.applied_count, 0);
        assert!(repo.path().join("target/generated.rs").exists());
        assert!(output.skipped.iter().any(|row| {
            row.relative_path == "target" && row.reason.contains("tracked or staged source changes")
        }));
    }

    #[test]
    fn branch_is_merged_detects_ancestor_and_unmerged_worktrees() {
        // upstream "remote" repo
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_string_lossy().to_string();

        let merged = git_repo();
        git(merged.path(), &["remote", "add", "origin", &remote_url]);
        git(merged.path(), &["push", "-u", "origin", "main"]);
        // No commits ahead of upstream → merged (ancestor case).
        assert!(branch_is_merged(merged.path()));

        // Add a local commit that has not been pushed → not merged.
        write_file(&merged.path().join("src/feature.rs"), "feature");
        git(merged.path(), &["add", "src/feature.rs"]);
        git(
            merged.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "unmerged feature",
            ],
        );
        assert!(!branch_is_merged(merged.path()));
    }

    #[test]
    fn branch_is_merged_false_without_upstream() {
        let repo = git_repo();
        // No tracking branch configured at all.
        assert!(!branch_is_merged(repo.path()));
    }

    #[test]
    fn merged_only_preserves_unmerged_worktree_target() {
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_string_lossy().to_string();

        let repo = git_repo();
        git(repo.path(), &["remote", "add", "origin", &remote_url]);
        git(repo.path(), &["push", "-u", "origin", "main"]);

        // Local unmerged commit → branch is ahead of upstream.
        write_file(&repo.path().join("src/feature.rs"), "feature");
        git(repo.path(), &["add", "src/feature.rs"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "unmerged feature",
            ],
        );
        write_file(&repo.path().join("target/debug/app"), "artifact");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            merged_only: true,
        })
        .expect("merged-only cleanup");

        assert_eq!(output.applied_count, 0, "unmerged target must be preserved");
        assert!(repo.path().join("target/debug/app").exists());
        assert!(output.skipped.iter().any(|row| {
            row.relative_path == "target" && row.reason.contains("not merged into its upstream")
        }));
    }

    #[test]
    fn merged_only_reclaims_merged_worktree_target() {
        let remote = TempDir::new().expect("remote");
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        let remote_url = remote.path().to_string_lossy().to_string();

        let repo = git_repo();
        git(repo.path(), &["remote", "add", "origin", &remote_url]);
        git(repo.path(), &["push", "-u", "origin", "main"]);

        // Branch tip equals upstream → merged. Leftover target/ should be reclaimed.
        write_file(&repo.path().join("target/debug/app"), "artifact");

        let output = cleanup_artifacts(ArtifactCleanupOptions {
            path: Some(repo.path().to_path_buf()),
            apply: true,
            self_artifacts: false,
            temp_roots: Vec::new(),
            merged_only: true,
        })
        .expect("merged-only cleanup");

        assert!(output.applied_count >= 1, "merged target must be reclaimed");
        assert!(!repo.path().join("target").exists());
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

    fn temp_homeboy_checkout(temp_root: &Path, name: &str) -> PathBuf {
        let checkout = temp_root.join(name);
        fs::create_dir_all(&checkout).expect("mkdir checkout");
        git(&checkout, &["init", "-b", "main"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy.git",
            ],
        );
        write_file(
            &checkout.join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\n",
        );
        write_file(&checkout.join("src/lib.rs"), "source");
        git(&checkout, &["add", "Cargo.toml", "src/lib.rs"]);
        git(
            &checkout,
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
        checkout
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
