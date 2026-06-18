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
    pub self_artifacts: bool,
    pub temp_roots: Vec<PathBuf>,
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
    let root = resolve_root(&options)?;
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

    for candidate in self_temp_artifact_candidates(&options)? {
        if options.apply {
            remove_artifact_path(Path::new(&candidate.path))?;
            applied.push(ArtifactCleanupApplied {
                worktree: candidate.worktree.clone(),
                path: candidate.path.clone(),
                relative_path: candidate.relative_path.clone(),
                kind: candidate.kind.clone(),
                size_bytes: candidate.size_bytes,
                removed: true,
            });
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

fn homeboy_source_checkout() -> Result<PathBuf> {
    let manifest_dir = option_env!("CARGO_MANIFEST_DIR").ok_or_else(|| {
        Error::validation_invalid_argument(
            "self_artifacts",
            "Homeboy source checkout is unavailable for this binary",
            None,
            None,
        )
    })?;
    validate_homeboy_manifest_dir(Path::new(manifest_dir))
}

fn validate_homeboy_manifest_dir(manifest_dir: &Path) -> Result<PathBuf> {
    let cargo_toml = manifest_dir.join("Cargo.toml");
    if !cargo_toml.is_file() {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            format!("{} does not contain Cargo.toml", manifest_dir.display()),
            None,
            None,
        ));
    }

    let raw = fs::read_to_string(&cargo_toml).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", cargo_toml.display())),
        )
    })?;
    if !raw.lines().any(|line| line.trim() == "name = \"homeboy\"") {
        return Err(Error::validation_invalid_argument(
            "self_artifacts",
            format!("{} is not the Homeboy crate manifest", cargo_toml.display()),
            None,
            None,
        ));
    }

    Ok(manifest_dir.to_path_buf())
}

fn self_temp_artifact_candidates(
    options: &ArtifactCleanupOptions,
) -> Result<Vec<ArtifactCleanupCandidate>> {
    if !options.self_artifacts && options.temp_roots.is_empty() {
        return Ok(Vec::new());
    }

    let roots = if options.temp_roots.is_empty() {
        default_self_temp_roots()
    } else {
        options.temp_roots.clone()
    };
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    for root in roots {
        if !root.is_dir() || !seen.insert(root.clone()) {
            continue;
        }
        for entry in fs::read_dir(&root).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read temp root {}", root.display())),
            )
        })? {
            let entry = entry.map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read temp root entry {}", root.display())),
                )
            })?;
            let path = entry.path();
            if !is_detached_homeboy_temp_artifact(&path) {
                if let Some(candidate) = temp_homeboy_checkout_target_candidate(&path)? {
                    candidates.push(candidate);
                } else if let Some(candidate) = partial_homeboy_temp_target_candidate(&path)? {
                    candidates.push(candidate);
                }
                continue;
            }
            let size_bytes = path_size(&path)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            candidates.push(ArtifactCleanupCandidate {
                worktree: root.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                relative_path: name,
                kind: "detached_homeboy_temp_artifact".to_string(),
                declared_by: "self_temp_root".to_string(),
                size_bytes,
                source_dirty: false,
                unpushed_commits: false,
            });
        }
    }

    Ok(candidates)
}

fn temp_homeboy_checkout_target_candidate(
    checkout: &Path,
) -> Result<Option<ArtifactCleanupCandidate>> {
    if !is_homeboy_source_checkout(checkout)? {
        return Ok(None);
    }

    let target = checkout.join("target");
    let Ok(metadata) = fs::symlink_metadata(&target) else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }

    let safety = match git_safety(checkout) {
        Ok(safety) => safety,
        Err(_) => return Ok(None),
    };
    if has_tracked_changes_under(&safety.dirty_paths, "target") {
        return Ok(None);
    }

    let size_bytes = path_size(&target)?;
    Ok(Some(ArtifactCleanupCandidate {
        worktree: checkout.to_string_lossy().to_string(),
        path: target.to_string_lossy().to_string(),
        relative_path: "target".to_string(),
        kind: "temp_homeboy_checkout_target".to_string(),
        declared_by: "self_temp_root".to_string(),
        size_bytes,
        source_dirty: safety.source_dirty,
        unpushed_commits: safety.unpushed_commits,
    }))
}

fn partial_homeboy_temp_target_candidate(
    temp_dir: &Path,
) -> Result<Option<ArtifactCleanupCandidate>> {
    let Ok(metadata) = fs::symlink_metadata(temp_dir) else {
        return Ok(None);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(None);
    }

    let Some(name) = temp_dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    if !name.starts_with("homeboy-")
        || temp_dir.join(".git").exists()
        || temp_dir.join("Cargo.toml").exists()
    {
        return Ok(None);
    }

    let target = temp_dir.join("target");
    let Ok(target_metadata) = fs::symlink_metadata(&target) else {
        return Ok(None);
    };
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Ok(None);
    }
    if !partial_homeboy_temp_skeleton_is_safe(temp_dir)? {
        return Ok(None);
    }

    let size_bytes = path_size(&target)?;
    Ok(Some(ArtifactCleanupCandidate {
        worktree: temp_dir.to_string_lossy().to_string(),
        path: target.to_string_lossy().to_string(),
        relative_path: "target".to_string(),
        kind: "partial_homeboy_temp_target".to_string(),
        declared_by: "self_temp_root".to_string(),
        size_bytes,
        source_dirty: false,
        unpushed_commits: false,
    }))
}

fn partial_homeboy_temp_skeleton_is_safe(temp_dir: &Path) -> Result<bool> {
    let mut saw_target = false;
    for entry in fs::read_dir(temp_dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read partial temp dir {}", temp_dir.display())),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!(
                    "read partial temp dir entry {}",
                    temp_dir.display()
                )),
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        match name {
            "target" => saw_target = true,
            ".github" | "docs" | "src" | "tests" => {
                if !directory_tree_has_no_files(&entry.path())? {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(saw_target)
}

fn directory_tree_has_no_files(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("stat {}", path.display()))))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
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
        let entry_path = entry.path();
        let entry_metadata = fs::symlink_metadata(&entry_path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("stat {}", entry_path.display())),
            )
        })?;
        if !entry_metadata.is_dir() || entry_metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if !directory_tree_has_no_files(&entry_path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_homeboy_source_checkout(path: &Path) -> Result<bool> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(false);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    if !path.join(".git").exists() || !path.join("Cargo.toml").is_file() {
        return Ok(false);
    }
    if !cargo_manifest_package_is_homeboy(&path.join("Cargo.toml"))? {
        return Ok(false);
    }

    let remotes = match git::run_git(path, &["remote", "-v"], "git remote -v") {
        Ok(output) => output,
        Err(_) => return Ok(false),
    };
    Ok(remotes.lines().any(|line| {
        line.contains("Extra-Chill/homeboy.git") || line.contains("Extra-Chill/homeboy ")
    }))
}

fn cargo_manifest_package_is_homeboy(cargo_toml: &Path) -> Result<bool> {
    let raw = fs::read_to_string(cargo_toml).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", cargo_toml.display())),
        )
    })?;

    let mut in_package = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed == "name = \"homeboy\"" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn default_self_temp_roots() -> Vec<PathBuf> {
    let temp_dir = std::env::temp_dir();
    vec![temp_dir.clone(), temp_dir.join("opencode")]
}

fn is_detached_homeboy_temp_artifact(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    if path.join(".git").exists() || path.join("Cargo.toml").exists() {
        return false;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with("homeboy-")
        && (name.ends_with("-target") || name.contains("-target-") || name.ends_with("-build"))
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
