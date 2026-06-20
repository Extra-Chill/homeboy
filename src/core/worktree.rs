use std::fs;
use std::path::{Path, PathBuf};

use crate::core::component::{self, TargetSpec};
use crate::core::error::{Error, Result};
use crate::core::ownership;
use crate::core::{git, paths};

mod types {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum TaskWorktreeState {
        Active,
        Removed,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum CleanupPolicy {
        RemoveWhenSafe,
        PreserveOnFailure,
    }

    impl CleanupPolicy {
        pub(super) fn default_for_run(run_id: Option<&str>) -> Self {
            if run_id.is_some() {
                Self::PreserveOnFailure
            } else {
                Self::RemoveWhenSafe
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct TaskWorktreeRecord {
        pub id: String,
        pub component_id: String,
        pub source_checkout: String,
        pub worktree_path: String,
        pub branch: String,
        pub base_ref: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub task_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub run_id: Option<String>,
        pub cleanup_policy: CleanupPolicy,
        pub created_at: String,
        pub state: TaskWorktreeState,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct WorktreeSafetyReport {
        pub dirty: bool,
        pub unpushed_commits: u32,
        pub primary_checkout: bool,
        pub path_contained: bool,
        pub worktree_missing: bool,
        /// Whether the worktree's branch content is already integrated into
        /// its base ref. True for squash/rebase-merged PRs whose local
        /// commits are no longer ancestors of the base but whose patch-ids
        /// are present in it. Drives merge-aware artifact reclamation.
        pub merged: bool,
        pub safe: bool,
        pub reasons: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCreateOutput {
        pub record: TaskWorktreeRecord,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeListOutput {
        pub worktrees: Vec<TaskWorktreeRecord>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeStatusOutput {
        pub record: TaskWorktreeRecord,
        pub safety: WorktreeSafetyReport,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeRemoveOutput {
        pub record: TaskWorktreeRecord,
        pub safety: WorktreeSafetyReport,
        pub removed: bool,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCleanupOutput {
        pub candidates: Vec<WorktreeRemoveOutput>,
        /// Build artifacts reclaimed from merged worktrees that survived full
        /// removal (e.g. squash-merged branches blocked by the unpushed-commit
        /// safety gate). Each entry is a `target/`-style rebuildable directory
        /// freed in place without deleting the worktree itself.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub reclaimed_artifacts: Vec<ReclaimedArtifact>,
        /// Total bytes freed by `reclaimed_artifacts`.
        #[serde(default)]
        pub reclaimed_bytes: u64,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct ReclaimedArtifact {
        pub worktree_id: String,
        pub worktree_path: String,
        pub path: String,
        pub relative_path: String,
        pub kind: String,
        pub size_bytes: u64,
        pub removed: bool,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeCreateOptions {
        pub component_id: String,
        pub branch: String,
        pub from: Option<String>,
        pub task_url: Option<String>,
        pub run_id: Option<String>,
        pub cleanup_policy: Option<CleanupPolicy>,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeRemoveOptions {
        pub id: String,
        pub force: bool,
    }
}

pub use types::{
    CleanupPolicy, ReclaimedArtifact, TaskWorktreeRecord, TaskWorktreeState, WorktreeCleanupOutput,
    WorktreeCreateOptions, WorktreeCreateOutput, WorktreeListOutput, WorktreeRemoveOptions,
    WorktreeRemoveOutput, WorktreeSafetyReport, WorktreeStatusOutput,
};

pub fn create(options: WorktreeCreateOptions) -> Result<WorktreeCreateOutput> {
    create_with_store(options, &metadata_dir()?)
}

pub fn list() -> Result<WorktreeListOutput> {
    list_with_store(&metadata_dir()?)
}

pub fn status(id: &str) -> Result<WorktreeStatusOutput> {
    status_with_store(id, &metadata_dir()?)
}

pub fn resolve(id: &str) -> Result<TaskWorktreeRecord> {
    read_record(&metadata_dir()?, id)
}

pub fn remove(options: WorktreeRemoveOptions) -> Result<WorktreeRemoveOutput> {
    remove_with_store(options, &metadata_dir()?)
}

pub fn cleanup(force: bool) -> Result<WorktreeCleanupOutput> {
    let store = metadata_dir()?;
    cleanup_with_store(force, &store)
}

fn cleanup_with_store(force: bool, store: &Path) -> Result<WorktreeCleanupOutput> {
    let mut candidates = Vec::new();
    let mut reclaimed_artifacts = Vec::new();
    for record in list_with_store(store)?.worktrees {
        if record.state != TaskWorktreeState::Active {
            continue;
        }

        // Inspect safety (including merge state) once so we can decide between
        // full removal and in-place artifact reclamation without aborting the
        // whole cleanup pass on a single un-removable worktree.
        let safety = safety_report(&record)?;

        // A worktree is fully removable when it clears the soft gate (safe, or
        // forced) and the hard gates (not the primary checkout, path
        // contained). This mirrors `remove_with_store` so we never trip its
        // validation error mid-loop.
        let hard_gates_ok = !safety.primary_checkout && safety.path_contained;
        let removable = hard_gates_ok && (force || safety.safe);

        if record.cleanup_policy != CleanupPolicy::PreserveOnFailure && removable {
            candidates.push(remove_with_store(
                WorktreeRemoveOptions {
                    id: record.id,
                    force,
                },
                store,
            )?);
            continue;
        }

        // The worktree survives this pass — either it is preserved on failure,
        // or it is blocked by the safety gates. Squash-/rebase-merged branches
        // keep "unpushed" commits relative to base, so a merged worktree stays
        // blocked here while its multi-gigabyte `target/` lingers. Reclaim the
        // rebuildable build artifacts in place so the disk is freed even though
        // the worktree itself is kept for inspection.
        if safety.merged && !safety.worktree_missing {
            reclaimed_artifacts.extend(reclaim_record_artifacts(&record)?);
        }
    }
    let reclaimed_bytes = reclaimed_artifacts.iter().map(|row| row.size_bytes).sum();
    Ok(WorktreeCleanupOutput {
        candidates,
        reclaimed_artifacts,
        reclaimed_bytes,
    })
}

/// Reclaim rebuildable build artifacts (e.g. `target/`) for a single worktree
/// record, mapping the cleanup module's applied rows into [`ReclaimedArtifact`]
/// entries tagged with the worktree id.
fn reclaim_record_artifacts(record: &TaskWorktreeRecord) -> Result<Vec<ReclaimedArtifact>> {
    let worktree_path = Path::new(&record.worktree_path);
    let applied = crate::core::cleanup::reclaim_worktree_artifacts(worktree_path, true)?;
    Ok(applied
        .into_iter()
        .map(|row| ReclaimedArtifact {
            worktree_id: record.id.clone(),
            worktree_path: record.worktree_path.clone(),
            path: row.path,
            relative_path: row.relative_path,
            kind: row.kind,
            size_bytes: row.size_bytes,
            removed: row.removed,
        })
        .collect())
}

fn create_with_store(
    options: WorktreeCreateOptions,
    store_dir: &Path,
) -> Result<WorktreeCreateOutput> {
    let target = component::resolve_target(TargetSpec {
        component_id: Some(&options.component_id),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: false,
        accept_bare_directory: false,
        ..TargetSpec::default()
    })?;
    let source_checkout = target
        .git_root
        .clone()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "component",
                "Component local_path is not inside a git checkout",
                Some(options.component_id.clone()),
                Some(vec!["Register a git-backed component checkout".to_string()]),
            )
        })?
        .canonicalize()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(target.source_path.display().to_string()),
            )
        })?;

    let parent = source_checkout.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source_checkout.display()
        ))
    })?;
    let id = format!("{}@{}", target.component_id, branch_slug(&options.branch));
    let worktree_path = parent.join(&id);
    if worktree_path.exists() {
        return Err(Error::validation_invalid_argument(
            "branch",
            "Task worktree path already exists",
            Some(worktree_path.display().to_string()),
            Some(vec![
                "Use a unique branch name or remove the existing task worktree".to_string(),
            ]),
        ));
    }

    let worktree_owner = ownership::owner_for_path_or_ancestor(parent)?;
    let base_ref = options.from.unwrap_or_else(|| "HEAD".to_string());
    git::run_git(
        &source_checkout,
        &[
            "worktree",
            "add",
            "-b",
            &options.branch,
            &worktree_path.to_string_lossy(),
            &base_ref,
        ],
        "git worktree add",
    )?;
    ownership::normalize_created_path(&worktree_path, worktree_owner, true, "git worktree add")?;

    let record = TaskWorktreeRecord {
        id,
        component_id: target.component_id,
        source_checkout: source_checkout.to_string_lossy().to_string(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        branch: options.branch,
        base_ref,
        task_url: options.task_url,
        run_id: options.run_id.clone(),
        cleanup_policy: options
            .cleanup_policy
            .unwrap_or_else(|| CleanupPolicy::default_for_run(options.run_id.as_deref())),
        created_at: chrono::Utc::now().to_rfc3339(),
        state: TaskWorktreeState::Active,
    };
    write_record(store_dir, &record)?;
    Ok(WorktreeCreateOutput { record })
}

fn list_with_store(store_dir: &Path) -> Result<WorktreeListOutput> {
    let mut worktrees = Vec::new();
    if !store_dir.exists() {
        return Ok(WorktreeListOutput { worktrees });
    }
    for entry in fs::read_dir(store_dir)
        .map_err(|err| Error::internal_io(err.to_string(), Some(store_dir.display().to_string())))?
    {
        let entry = entry.map_err(|err| Error::internal_io(err.to_string(), None))?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        worktrees.push(read_record_path(&entry.path())?);
    }
    worktrees.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(WorktreeListOutput { worktrees })
}

fn status_with_store(id: &str, store_dir: &Path) -> Result<WorktreeStatusOutput> {
    let record = read_record(store_dir, id)?;
    let safety = safety_report(&record)?;
    Ok(WorktreeStatusOutput { record, safety })
}

fn remove_with_store(
    options: WorktreeRemoveOptions,
    store_dir: &Path,
) -> Result<WorktreeRemoveOutput> {
    let mut record = read_record(store_dir, &options.id)?;
    let safety = safety_report(&record)?;
    if !options.force && !safety.safe {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Task worktree is not safe to remove",
            Some(record.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }
    if safety.primary_checkout || !safety.path_contained {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Task worktree failed hard removal safety gates",
            Some(record.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }

    if !safety.worktree_missing {
        git::run_git(
            Path::new(&record.source_checkout),
            &["worktree", "remove", &record.worktree_path],
            "git worktree remove",
        )?;
    }
    record.state = TaskWorktreeState::Removed;
    write_record(store_dir, &record)?;
    Ok(WorktreeRemoveOutput {
        record,
        safety,
        removed: true,
    })
}

fn safety_report(record: &TaskWorktreeRecord) -> Result<WorktreeSafetyReport> {
    let source = canonical_existing_path(&record.source_checkout)?;
    let parent = source.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source.display()
        ))
    })?;
    let raw_worktree = Path::new(&record.worktree_path);
    let worktree = match raw_worktree.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            normalize_missing_path(raw_worktree)
        }
        Err(err) => {
            return Err(Error::internal_io(
                err.to_string(),
                Some(record.worktree_path.clone()),
            ))
        }
    };
    let worktree_missing = !raw_worktree.exists();
    let primary_checkout = source == worktree;
    let path_contained = worktree.starts_with(parent) && worktree != source;
    let dirty = !worktree_missing && is_dirty(&worktree)?;
    let unpushed_commits = if worktree_missing {
        0
    } else {
        unpushed_commit_count(&worktree, &record.base_ref)?
    };
    let merged = if worktree_missing {
        false
    } else {
        branch_merged_into_base(&worktree, &record.base_ref)
    };
    let mut reasons = Vec::new();
    if dirty {
        reasons.push("dirty worktree".to_string());
    }
    if unpushed_commits > 0 {
        reasons.push(format!("{unpushed_commits} unpushed commit(s)"));
    }
    if primary_checkout {
        reasons.push("refuses to remove primary checkout".to_string());
    }
    if !path_contained {
        reasons.push("worktree path is outside the component checkout parent".to_string());
    }
    let safe = reasons.is_empty();
    Ok(WorktreeSafetyReport {
        dirty,
        unpushed_commits,
        primary_checkout,
        path_contained,
        worktree_missing,
        merged,
        safe,
        reasons,
    })
}

/// Whether every commit on the worktree's branch is already integrated into
/// `base_ref`, including squash- and rebase-merged PRs.
///
/// Uses `git cherry <base> HEAD`, which lists each commit reachable from HEAD
/// but not from base, prefixing `-` when the commit's patch-id is already
/// present in base and `+` when it is not. A branch is considered merged when
/// there are no `+` lines: either HEAD is an ancestor of base (no output) or
/// every commit's patch-id appears in base (squash/rebase merge). Returns
/// `false` conservatively whenever the probe cannot run (detached upstream,
/// missing base, git error) so reclamation never fires on uncertain state.
fn branch_merged_into_base(worktree: &Path, base_ref: &str) -> bool {
    let Ok(output) = git::run_git(worktree, &["cherry", base_ref, "HEAD"], "git cherry") else {
        return false;
    };
    let mut saw_commit = false;
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        saw_commit = true;
        if line.starts_with('+') {
            return false;
        }
    }
    // No `+` lines: either base already contains HEAD (no output) or every
    // commit's patch-id is in base. Treat an empty/ancestor result as merged
    // only when the branch actually has commits relative to base to avoid
    // reclaiming a brand-new worktree that has not diverged yet.
    saw_commit
}

fn is_dirty(path: &Path) -> Result<bool> {
    Ok(
        !git::run_git(path, &["status", "--porcelain=v1"], "git status")?
            .trim()
            .is_empty(),
    )
}

fn unpushed_commit_count(path: &Path, base_ref: &str) -> Result<u32> {
    let upstream = git::run_git(path, &["rev-parse", "--abbrev-ref", "@{u}"], "git upstream");
    let range = if let Ok(upstream) = upstream {
        let upstream = upstream.trim();
        if upstream.is_empty() {
            format!("{base_ref}..HEAD")
        } else {
            format!("{upstream}..HEAD")
        }
    } else {
        format!("{base_ref}..HEAD")
    };
    let count = git::run_git(path, &["rev-list", "--count", &range], "git rev-list")?;
    Ok(count.trim().parse::<u32>().unwrap_or(0))
}

fn canonical_existing_path(path: &str) -> Result<PathBuf> {
    Path::new(path)
        .canonicalize()
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.to_string())))
}

fn normalize_missing_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };
    parent
        .canonicalize()
        .map(|parent| parent.join(file_name))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn metadata_dir() -> Result<PathBuf> {
    let observation_db = paths::observation_db()?;
    let data_root = observation_db.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "observation database path `{}` has no parent directory",
            observation_db.display()
        ))
    })?;

    Ok(data_root.join("task-worktrees"))
}

fn record_path(store_dir: &Path, id: &str) -> PathBuf {
    store_dir.join(format!("{}.json", paths::sanitize_path_segment(id)))
}

fn write_record(store_dir: &Path, record: &TaskWorktreeRecord) -> Result<()> {
    let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
    fs::create_dir_all(store_dir).map_err(|err| {
        Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|err| Error::internal_json(err.to_string(), Some(record.id.clone())))?;
    let path = record_path(store_dir, &record.id);
    fs::write(&path, format!("{json}\n"))
        .map_err(|err| Error::internal_io(err.to_string(), Some(record.id.clone())))?;
    ownership::normalize_created_path(store_dir, store_owner, false, "write worktree metadata")?;
    ownership::normalize_created_path(&path, store_owner, false, "write worktree metadata")?;
    Ok(())
}

fn read_record(store_dir: &Path, id: &str) -> Result<TaskWorktreeRecord> {
    read_record_path(&record_path(store_dir, id))
}

fn read_record_path(path: &Path) -> Result<TaskWorktreeRecord> {
    let raw = fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|err| Error::internal_json(err.to_string(), Some(path.display().to_string())))
}

fn branch_slug(branch: &str) -> String {
    branch
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
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

    fn fixture_record(source: &Path, worktree: &Path) -> TaskWorktreeRecord {
        TaskWorktreeRecord {
            id: "fixture@task".to_string(),
            component_id: "fixture".to_string(),
            source_checkout: source.to_string_lossy().to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            branch: "task".to_string(),
            base_ref: "HEAD".to_string(),
            task_url: Some("https://example.com/task".to_string()),
            run_id: None,
            cleanup_policy: CleanupPolicy::RemoveWhenSafe,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            state: TaskWorktreeState::Active,
        }
    }

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init", "-q"]);
        run_git(
            temp.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(temp.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(temp.path().join("README.md"), "initial\n").unwrap();
        run_git(temp.path(), &["add", "."]);
        run_git(temp.path(), &["commit", "-q", "-m", "initial"]);
        temp
    }

    #[test]
    fn metadata_round_trips_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let worktree = dir.path().join("source@task");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let store = dir.path().join("store");
        let record = fixture_record(&source, &worktree);

        write_record(&store, &record).unwrap();
        let listed = list_with_store(&store).unwrap();

        assert_eq!(listed.worktrees, vec![record]);
    }

    #[test]
    fn safety_report_blocks_dirty_worktree() {
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "dirty");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "dirty-task",
                &worktree.to_string_lossy(),
            ],
        );
        fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();

        let report = safety_report(&fixture_record(source.path(), &worktree)).unwrap();

        assert!(report.dirty);
        assert!(!report.safe);
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason == "dirty worktree"));
    }

    #[test]
    fn safety_report_blocks_primary_checkout() {
        let source = git_repo();

        let report = safety_report(&fixture_record(source.path(), source.path())).unwrap();

        assert!(report.primary_checkout);
        assert!(!report.path_contained);
        assert!(!report.worktree_missing);
        assert!(!report.safe);
    }

    #[test]
    fn safety_report_allows_missing_contained_worktree() {
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "missing");

        let report = safety_report(&fixture_record(source.path(), &worktree)).unwrap();

        assert!(report.worktree_missing);
        assert!(report.path_contained);
        assert!(!report.primary_checkout);
        assert!(!report.dirty);
        assert_eq!(report.unpushed_commits, 0);
        assert!(report.safe);
    }

    #[test]
    fn cleanup_marks_missing_worktree_record_removed() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "missing-cleanup");
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), &worktree);
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(false, &store).unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert_eq!(output.candidates.len(), 1);
        assert!(output.candidates[0].removed);
        assert!(output.candidates[0].safety.worktree_missing);
        assert_eq!(updated.state, TaskWorktreeState::Removed);
    }

    #[test]
    fn safety_report_blocks_unpushed_commits() {
        let remote = tempfile::tempdir().unwrap();
        run_git(remote.path(), &["init", "--bare", "-q"]);
        let source = tempfile::tempdir().unwrap();
        run_git(
            source.path(),
            &["clone", &remote.path().to_string_lossy(), "."],
        );
        run_git(
            source.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(source.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(source.path().join("README.md"), "initial\n").unwrap();
        run_git(source.path(), &["add", "."]);
        run_git(source.path(), &["commit", "-q", "-m", "initial"]);
        run_git(source.path(), &["push", "-u", "origin", "HEAD:main"]);

        let worktree = sibling_worktree_path(source.path(), "unpushed");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "unpushed-task",
                &worktree.to_string_lossy(),
                "HEAD",
            ],
        );
        fs::write(worktree.join("change.txt"), "change\n").unwrap();
        run_git(&worktree, &["add", "."]);
        run_git(&worktree, &["commit", "-q", "-m", "change"]);

        let mut record = fixture_record(source.path(), &worktree);
        record.base_ref = "origin/main".to_string();
        let report = safety_report(&record).unwrap();

        assert_eq!(report.unpushed_commits, 1);
        assert!(!report.safe);
    }

    fn sibling_worktree_path(source: &Path, suffix: &str) -> PathBuf {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source");
        source.with_file_name(format!("{name}-{suffix}-worktree"))
    }

    /// Build a source checkout with an `origin/main` baseline so squash-merge
    /// scenarios can be reproduced. Returns the temp dir holding the bare
    /// remote (kept alive by the caller) and the source checkout temp dir.
    fn source_with_remote() -> (tempfile::TempDir, tempfile::TempDir) {
        let remote = tempfile::tempdir().unwrap();
        run_git(remote.path(), &["init", "--bare", "-q"]);
        let source = tempfile::tempdir().unwrap();
        run_git(
            source.path(),
            &["clone", &remote.path().to_string_lossy(), "."],
        );
        run_git(
            source.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(source.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(source.path().join("README.md"), "initial\n").unwrap();
        run_git(source.path(), &["add", "."]);
        run_git(source.path(), &["commit", "-q", "-m", "initial"]);
        run_git(source.path(), &["push", "-u", "origin", "HEAD:main"]);
        (remote, source)
    }

    #[test]
    fn branch_merged_into_base_detects_squash_merge() {
        let (_remote, source) = source_with_remote();

        // Branch with a single feature commit.
        let worktree = sibling_worktree_path(source.path(), "squash");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "squash-task",
                &worktree.to_string_lossy(),
                "origin/main",
            ],
        );
        fs::write(worktree.join("feature.txt"), "feature\n").unwrap();
        run_git(&worktree, &["add", "."]);
        run_git(&worktree, &["commit", "-q", "-m", "add feature"]);

        // Not yet integrated into base.
        assert!(!branch_merged_into_base(&worktree, "origin/main"));

        // Simulate a squash-merge: same content lands on main as a brand-new
        // commit with an unrelated SHA, then origin/main advances.
        fs::write(source.path().join("feature.txt"), "feature\n").unwrap();
        run_git(source.path(), &["add", "."]);
        run_git(source.path(), &["commit", "-q", "-m", "squashed feature"]);
        run_git(source.path(), &["push", "origin", "HEAD:main"]);
        run_git(&worktree, &["fetch", "-q", "origin"]);

        // git cherry now reports the patch-id as present in base ("- " prefix).
        assert!(branch_merged_into_base(&worktree, "origin/main"));
    }

    #[test]
    fn branch_merged_into_base_is_false_for_fresh_worktree() {
        let (_remote, source) = source_with_remote();
        let worktree = sibling_worktree_path(source.path(), "fresh");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "fresh-task",
                &worktree.to_string_lossy(),
                "origin/main",
            ],
        );

        // No commits relative to base yet → not a reclamation candidate.
        assert!(!branch_merged_into_base(&worktree, "origin/main"));
    }

    #[test]
    fn cleanup_reclaims_target_for_merged_blocked_worktree() {
        let (_remote, source) = source_with_remote();
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");

        let worktree = sibling_worktree_path(source.path(), "merged-target");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "merged-target-task",
                &worktree.to_string_lossy(),
                "origin/main",
            ],
        );
        fs::write(worktree.join("feature.txt"), "feature\n").unwrap();
        run_git(&worktree, &["add", "."]);
        run_git(&worktree, &["commit", "-q", "-m", "add feature"]);

        // Squash-merge the content onto main.
        fs::write(source.path().join("feature.txt"), "feature\n").unwrap();
        run_git(source.path(), &["add", "."]);
        run_git(source.path(), &["commit", "-q", "-m", "squashed feature"]);
        run_git(source.path(), &["push", "origin", "HEAD:main"]);
        run_git(&worktree, &["fetch", "-q", "origin"]);

        // A multi-gigabyte `target/` stand-in lingers in the merged worktree.
        fs::create_dir_all(worktree.join("target/debug")).unwrap();
        fs::write(worktree.join("target/debug/bin"), "build artifact\n").unwrap();

        let mut record = fixture_record(source.path(), &worktree);
        record.id = "fixture@merged-target-task".to_string();
        record.base_ref = "origin/main".to_string();
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(false, &store).unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        // Full removal stays blocked (squash-merge leaves "unpushed" commits),
        // so the worktree is neither removed nor recorded as a removal
        // candidate, and its record stays active...
        assert!(output.candidates.is_empty());
        assert_eq!(updated.state, TaskWorktreeState::Active);
        assert!(worktree.exists());
        assert!(worktree.join("feature.txt").exists());

        // ...but its `target/` build directory is reclaimed in place.
        assert!(!worktree.join("target").exists());
        assert_eq!(output.reclaimed_artifacts.len(), 1);
        assert_eq!(output.reclaimed_artifacts[0].relative_path, "target");
        assert_eq!(output.reclaimed_artifacts[0].worktree_id, record.id);
        assert!(output.reclaimed_artifacts[0].removed);
        assert!(output.reclaimed_bytes > 0);
    }
}
