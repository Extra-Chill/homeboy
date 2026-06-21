use crate::core::component::Component;
use crate::core::engine::run_dir::RunDir;
use crate::core::engine::undo::UndoSnapshot;
use crate::core::extension::test::compute_changed_test_files;
use crate::core::git;
use crate::core::refactor::auto::FixResultsSummary;
use crate::core::Error;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod audit_source;
mod cache;
mod extension_source;
mod lint_scope;
mod planning;
mod stages;

use planning::{
    analyze_stage_overlaps, collect_collected_edits, collect_stage_changed_files,
    summarize_source_totals, FixAccumulator,
};
pub use planning::{CollectedEdit, SourceOverlap, SourceStageSummary, SourceTotals};
#[cfg(test)]
use stages::{
    collect_audit_changed_files, count_manual_only_fixes, reject_remaining_lint_fix_findings,
};
use stages::{
    format_changed_files, plan_audit_stage, require_successful_format, run_lint_stage,
    run_test_stage, AuditStageRequest,
};

pub const KNOWN_REFACTOR_SOURCES: &[&str] = &["audit", "lint", "test"];

#[derive(Debug, Clone)]
pub struct RefactorSourceRequest {
    pub component: Component,
    pub root: PathBuf,
    pub sources: Vec<String>,
    pub changed_since: Option<String>,
    pub only: Vec<crate::core::code_audit::AuditFinding>,
    pub exclude: Vec<crate::core::code_audit::AuditFinding>,
    pub settings: Vec<(String, String)>,
    pub lint: LintSourceOptions,
    pub test: TestSourceOptions,
    pub write: bool,
    /// Skip the clean working tree check (for CI or when you know what you're doing)
    pub force: bool,
}

pub fn lint_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> RefactorSourceRequest {
    RefactorSourceRequest {
        component,
        root,
        sources: vec!["lint".to_string()],
        changed_since: None,
        only: Vec::new(),
        exclude: Vec::new(),
        settings,
        lint: options,
        test: TestSourceOptions::default(),
        write,
        force: false,
    }
}

pub fn build_test_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> RefactorSourceRequest {
    RefactorSourceRequest {
        component,
        root,
        sources: vec!["test".to_string()],
        changed_since: None,
        only: Vec::new(),
        exclude: Vec::new(),
        settings,
        lint: LintSourceOptions::default(),
        test: options,
        write,
        force: false,
    }
}

#[derive(Debug, Clone, Default)]
pub struct LintSourceOptions {
    pub selected_files: Option<Vec<String>>,
    pub file: Option<String>,
    pub glob: Option<String>,
    pub errors_only: bool,
    pub sniffs: Option<String>,
    pub exclude_sniffs: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TestSourceOptions {
    pub selected_files: Option<Vec<String>>,
    pub skip_lint: bool,
    pub script_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RefactorSourceRun {
    pub component_id: String,
    pub source_path: String,
    pub sources: Vec<String>,
    pub dry_run: bool,
    pub applied: bool,
    pub merge_strategy: String,
    pub collected_edits: Vec<CollectedEdit>,
    pub stages: Vec<SourceStageSummary>,
    pub source_totals: SourceTotals,
    pub overlaps: Vec<SourceOverlap>,
    pub files_modified: usize,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    pub warnings: Vec<String>,
    pub hints: Vec<String>,
    /// When set, autofix was blocked by a safety guard. The pipeline
    /// short-circuited without modifying any files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guard_block: Option<crate::core::refactor::auto::guard::GuardBlock>,
}

pub fn collect_refactor_sources(
    request: RefactorSourceRequest,
) -> crate::core::Result<RefactorSourceRun> {
    let sources = normalize_sources(&request.sources)?;
    let root_str = request.root.to_string_lossy().to_string();
    let original_changes = git::get_uncommitted_changes(&root_str).ok();

    // Refuse to write to a dirty working tree unless --force is set.
    // Refactoring operates directly on the working tree, so mixing auto-generated
    // fixes with uncommitted manual changes makes rollback difficult.
    // Dry runs (no --write) are always safe — they don't modify files.
    //
    // Homeboy-owned CI artifacts (review output, observation bundles) are an
    // exception: they are regenerated every run and are never source code, so
    // a tree dirty only with them is safe to write. Autofix-on-failure runs in
    // exactly this state — the preceding quality-gate pass generated them (#4684).
    ensure_clean_enough_for_write(&request, original_changes.as_ref())?;

    // Autofix safety guards — check whether writing is safe in CI context.
    // Guards detect reverted/force-pushed bot commits and PR labels that
    // permanently disable autofix. Outside CI, guards are no-ops.
    if request.write {
        let guard_config = crate::core::refactor::auto::guard::GuardConfig::from_env();
        match crate::core::refactor::auto::guard::check_guards(&root_str, &guard_config) {
            crate::core::refactor::auto::guard::GuardResult::Proceed => {}
            crate::core::refactor::auto::guard::GuardResult::Blocked(block) => {
                eprintln!(
                    "[refactor] autofix blocked: {} (status: {})",
                    block.message(),
                    block.status()
                );
                return Ok(RefactorSourceRun {
                    component_id: String::new(),
                    source_path: root_str.clone(),
                    sources: normalize_sources(&request.sources)?,
                    dry_run: false,
                    applied: false,
                    merge_strategy: String::new(),
                    collected_edits: Vec::new(),
                    stages: Vec::new(),
                    source_totals: SourceTotals {
                        stages_with_edits: 0,
                        total_edits: 0,
                        total_files_selected: 0,
                    },
                    overlaps: Vec::new(),
                    files_modified: 0,
                    changed_files: Vec::new(),
                    fix_summary: None,
                    warnings: Vec::new(),
                    hints: Vec::new(),
                    guard_block: Some(block),
                });
            }
        }
    }

    let scoped_changed_files = if let Some(git_ref) = request.changed_since.as_deref() {
        Some(git::get_files_changed_since(&root_str, git_ref)?)
    } else {
        None
    };
    let scoped_test_files = if let Some(git_ref) = request.changed_since.as_deref() {
        Some(compute_changed_test_files(&request.component, git_ref)?)
    } else {
        None
    };

    let mut planned_stages = Vec::new();
    let merge_order = sources.join(" → ");
    let mut warnings = vec![format!("Deterministic merge order: {}", merge_order)];
    let mut accumulator = FixAccumulator::default();

    // Save undo snapshot before any modifications so we can roll back.
    if request.write {
        let mut snapshot_files: HashSet<String> = HashSet::new();
        if let Some(changes) = &original_changes {
            snapshot_files.extend(changes.staged.iter().cloned());
            snapshot_files.extend(changes.unstaged.iter().cloned());
            snapshot_files.extend(changes.untracked.iter().cloned());
        }
        if !snapshot_files.is_empty() {
            let mut snap = UndoSnapshot::new(&request.root, "refactor sources (pre)");
            for file in &snapshot_files {
                snap.capture_file(file);
            }
            if let Err(e) = snap.save() {
                crate::log_status!(
                    "undo",
                    "Warning: failed to save pre-refactor undo snapshot: {}",
                    e
                );
            }
        }
    }

    let run_dir = RunDir::create()?;

    for source in &sources {
        let stage = match source.as_str() {
            "audit" => plan_audit_stage(AuditStageRequest {
                component: &request.component,
                root: &request.root,
                changed_files: scoped_changed_files.as_deref(),
                only: &request.only,
                exclude: &request.exclude,
                write: request.write,
                settings: &request.settings,
            })?,
            "lint" => run_lint_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.lint,
                scoped_changed_files.as_deref(),
                request.write,
                &run_dir,
            )?,
            "test" => run_test_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.test,
                scoped_test_files.as_deref(),
                request.write,
                &run_dir,
            )?,
            _ => unreachable!("sources are normalized before orchestration"),
        };

        // Format generated/modified files so subsequent stages (especially lint)
        // see properly formatted code.
        if stage.summary.files_modified > 0 {
            format_changed_files(&request.root, &stage.summary.changed_files)?;
        }

        accumulator.extend(stage.fix_results.clone());
        planned_stages.push(stage);
    }

    let collected_edits = collect_collected_edits(&planned_stages);
    let mut stage_summaries: Vec<SourceStageSummary> = planned_stages
        .into_iter()
        .map(|stage| stage.summary)
        .collect();
    let changed_files = collect_stage_changed_files(&stage_summaries);
    let overlaps = analyze_stage_overlaps(&stage_summaries);
    if !overlaps.is_empty() {
        warnings.push(format!(
            "{} staged file overlap(s) resolved by precedence order {}",
            overlaps.len(),
            merge_order
        ));
    }

    let source_totals = summarize_source_totals(&stage_summaries, changed_files.len());
    let files_modified = changed_files.len();
    let applied = request.write && files_modified > 0;

    if applied {
        let abs_changed: Vec<PathBuf> =
            changed_files.iter().map(|f| request.root.join(f)).collect();
        require_successful_format(
            crate::core::engine::format_write::format_after_write(&request.root, &abs_changed)?,
            "post-write formatter",
        )?;
    }

    for stage in &mut stage_summaries {
        stage.applied = request.write && stage.files_modified > 0;
    }

    if files_modified == 0 {
        warnings.push("No automated fixes accumulated across audit/lint/test".to_string());
    }

    let hints = if applied {
        sources
            .iter()
            .map(|source| format!("Re-run checks: homeboy {} {}", source, request.component.id))
            .collect()
    } else if files_modified > 0 {
        vec!["Dry run. Re-run with --write to apply fixes to the working tree.".to_string()]
    } else {
        Vec::new()
    };

    Ok(RefactorSourceRun {
        component_id: request.component.id,
        source_path: root_str,
        sources,
        dry_run: !request.write,
        applied,
        merge_strategy: "sequential_source_merge".to_string(),
        collected_edits,
        stages: stage_summaries,
        source_totals,
        overlaps,
        files_modified,
        changed_files,
        fix_summary: accumulator.summary(),
        warnings,
        hints,
        guard_block: None,
    })
}

fn allows_dirty_worktree_write(request: &RefactorSourceRequest) -> bool {
    request.write
        && request.sources == ["lint"]
        && request
            .lint
            .selected_files
            .as_ref()
            .is_some_and(|files| !files.is_empty())
}

/// Homeboy-owned CI artifact path roots.
///
/// These directories are generated by Homeboy's own CI commands (review
/// output, observation bundles) and regenerated every run. They are never
/// source code, so their presence in a dirty working tree does not
/// compromise the rollback safety that the dirty-tree guard protects. When
/// the only uncommitted changes are under these roots, write-mode
/// refactor/lint-fix is safe to proceed (#4684).
const HOMEBOY_OWNED_CI_ARTIFACT_DIRS: &[&str] = &["homeboy-ci-results", "homeboy-observations"];

/// Whether a working-tree path is a Homeboy-owned CI artifact.
///
/// Matches [`HOMEBOY_OWNED_CI_ARTIFACT_DIRS`] after stripping a leading
/// `./`, normalizing backslashes, and trimming trailing slashes. Both the directory entry itself
/// (`homeboy-ci-results/`) and nested files (`homeboy-ci-results/review.json`)
/// match, so the classifier works for git's untracked-directory reporting and
/// individual staged/unstaged/deleted file paths alike.
fn is_homeboy_owned_ci_artifact(path: &str) -> bool {
    let normalized = path
        .trim_start_matches("./")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    HOMEBOY_OWNED_CI_ARTIFACT_DIRS
        .iter()
        .any(|dir| normalized == *dir || normalized.starts_with(&format!("{dir}/")))
}

fn log_git_status_short(root: &Path) {
    match Command::new("git")
        .args(["status", "--short"])
        .current_dir(root)
        .output()
    {
        Ok(output) if output.status.success() => {
            let status = String::from_utf8_lossy(&output.stdout);
            if status.trim().is_empty() {
                crate::log_status!("refactor", "git status --short: <clean>");
            } else {
                crate::log_status!("refactor", "git status --short:\n{}", status.trim_end());
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            crate::log_status!(
                "refactor",
                "git status --short failed before write-mode refusal: {}",
                stderr.trim()
            );
        }
        Err(error) => {
            crate::log_status!(
                "refactor",
                "git status --short failed before write-mode refusal: {}",
                error
            );
        }
    }
}

fn clean_homeboy_owned_ci_artifacts(root: &Path) -> crate::core::Result<bool> {
    for dir in HOMEBOY_OWNED_CI_ARTIFACT_DIRS {
        let path = root.join(dir);
        if path.exists() {
            fs::remove_dir_all(&path).map_err(|e| {
                crate::core::Error::internal_io(
                    format!(
                        "Failed to clean Homeboy CI artifact dir {}: {e}",
                        path.display()
                    ),
                    Some("refactor.clean_ci_artifacts".to_string()),
                )
            })?;
        }
    }

    let _ = Command::new("git")
        .args(["restore", "--staged", "--worktree", "--"])
        .args(HOMEBOY_OWNED_CI_ARTIFACT_DIRS)
        .current_dir(root)
        .output();

    let root_str = root.to_string_lossy().to_string();
    Ok(!git::get_uncommitted_changes(&root_str)
        .map(|changes| changes.has_changes)
        .unwrap_or(true))
}

/// Whether every uncommitted change is a Homeboy-owned CI artifact.
///
/// Used by [`ensure_clean_enough_for_write`] to keep autofix-on-failure
/// working when the preceding quality-gate pass left Homeboy-generated
/// artifacts in the checkout. A mixed tree (artifacts plus real source edits)
/// still trips the guard — only an all-artifact dirty set is treated as clean.
fn dirty_changes_are_only_homeboy_artifacts(
    changes: &crate::core::git::UncommittedChanges,
) -> bool {
    changes
        .staged
        .iter()
        .chain(changes.unstaged.iter())
        .chain(changes.untracked.iter())
        .all(|path| is_homeboy_owned_ci_artifact(path))
}

/// Enforce the dirty-working-tree guard before write-mode refactoring.
///
/// Returns `Ok(())` when the tree is clean, `--force` is set, the run is a
/// bounded lint with selected files, or the only dirty files are Homeboy-owned
/// CI artifacts (#4684). Otherwise returns the validation error that blocks the
/// write so auto-generated fixes never interleave with uncommitted manual edits.
fn ensure_clean_enough_for_write(
    request: &RefactorSourceRequest,
    changes: Option<&crate::core::git::UncommittedChanges>,
) -> crate::core::Result<()> {
    if !request.write || request.force || allows_dirty_worktree_write(request) {
        return Ok(());
    }

    let Some(changes) = changes else {
        return Ok(());
    };

    if !changes.has_changes {
        return Ok(());
    }

    if dirty_changes_are_only_homeboy_artifacts(changes) {
        let cleaned = clean_homeboy_owned_ci_artifacts(&request.root)?;
        if !cleaned {
            log_git_status_short(&request.root);
            return Err(crate::core::Error::validation_invalid_argument(
                "write",
                "Working tree still has Homeboy-owned CI artifact changes after cleanup",
                None,
                Some(vec![
                    "Inspect the git status above for remaining generated artifact changes"
                        .to_string(),
                    "Rerun with --force to allow the fixer to edit the current dirty working tree"
                        .to_string(),
                ]),
            ));
        }
        crate::log_status!(
            "refactor",
            "Cleaned Homeboy-owned CI artifacts before write-mode refactor (homeboy#4877)"
        );
        return Ok(());
    }

    log_git_status_short(&request.root);

    Err(crate::core::Error::validation_invalid_argument(
        "write",
        "Working tree has uncommitted changes",
        None,
        Some(vec![
            "Commit or stash your changes first".to_string(),
            "Rerun with --force to allow the fixer to edit the current dirty working tree"
                .to_string(),
        ]),
    ))
}

pub(crate) fn normalize_sources(sources: &[String]) -> crate::core::Result<Vec<String>> {
    let lowered: Vec<String> = sources.iter().map(|source| source.to_lowercase()).collect();

    if lowered.iter().any(|source| source == "all") {
        return Ok(KNOWN_REFACTOR_SOURCES
            .iter()
            .map(|source| source.to_string())
            .collect());
    }

    let unknown: Vec<String> = lowered
        .iter()
        .filter(|source| !KNOWN_REFACTOR_SOURCES.contains(&source.as_str()))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from",
            format!("Unknown refactor source(s): {}", unknown.join(", ")),
            None,
            Some(vec![format!(
                "Known sources: {}",
                KNOWN_REFACTOR_SOURCES.join(", ")
            )]),
        ));
    }

    let mut ordered = Vec::new();
    for known in KNOWN_REFACTOR_SOURCES {
        if lowered.iter().any(|source| source == known) {
            ordered.push((*known).to_string());
        }
    }

    if ordered.is_empty() {
        return Err(Error::validation_missing_argument(vec!["from".to_string()]));
    }

    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::Component;
    use std::fs;
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

    #[test]
    fn test_lint_refactor_request() {
        let root = PathBuf::from("/tmp/homeboy-lint-refactor-request");
        let request = lint_refactor_request(
            test_component(&root),
            root,
            vec![("key".to_string(), "value".to_string())],
            LintSourceOptions::default(),
            true,
        );

        assert_eq!(request.sources, vec!["lint".to_string()]);
        assert!(request.write);
    }

    #[test]
    fn bounded_lint_write_allows_dirty_worktree() {
        let root = PathBuf::from("/tmp/homeboy-bounded-lint-write");
        let request = lint_refactor_request(
            test_component(&root),
            root,
            Vec::new(),
            LintSourceOptions {
                selected_files: Some(vec!["src/lib.rs".to_string()]),
                ..Default::default()
            },
            true,
        );

        assert!(allows_dirty_worktree_write(&request));
    }

    #[test]
    fn broad_lint_write_still_requires_clean_worktree() {
        let root = PathBuf::from("/tmp/homeboy-broad-lint-write");
        let request = lint_refactor_request(
            test_component(&root),
            root,
            Vec::new(),
            LintSourceOptions::default(),
            true,
        );

        assert!(!allows_dirty_worktree_write(&request));
    }

    // ============================================================================
    // homeboy#4684 — autofix-on-failure vs dirty CI-artifact checkout
    // ============================================================================
    //
    // Regression tests for the CI failure where autofix's write-mode lint/refactor
    // hit "Working tree has uncommitted changes" because the preceding quality-gate
    // pass had generated Homeboy-owned artifacts (homeboy-ci-results/,
    // homeboy-observations/) in the checkout.

    use crate::core::git::UncommittedChanges;

    fn artifact_only_changes(untracked: &[&str]) -> UncommittedChanges {
        UncommittedChanges {
            has_changes: true,
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: untracked.iter().map(|s| s.to_string()).collect(),
            hint: None,
        }
    }

    #[test]
    fn homeboy_ci_results_paths_are_owned_artifacts() {
        assert!(is_homeboy_owned_ci_artifact("homeboy-ci-results"));
        assert!(is_homeboy_owned_ci_artifact("homeboy-ci-results/"));
        assert!(is_homeboy_owned_ci_artifact(
            "homeboy-ci-results/review.json"
        ));
        assert!(is_homeboy_owned_ci_artifact(
            "./homeboy-ci-results/review.json"
        ));
        // Backslash separators (Windows-style) still classify correctly.
        assert!(is_homeboy_owned_ci_artifact(
            "homeboy-ci-results\\review.json"
        ));
    }

    #[test]
    fn homeboy_observations_paths_are_owned_artifacts() {
        assert!(is_homeboy_owned_ci_artifact("homeboy-observations"));
        assert!(is_homeboy_owned_ci_artifact("homeboy-observations/"));
        assert!(is_homeboy_owned_ci_artifact(
            "homeboy-observations/run-1/manifest.json"
        ));
    }

    #[test]
    fn source_files_are_not_owned_artifacts() {
        assert!(!is_homeboy_owned_ci_artifact("src/lib.rs"));
        assert!(!is_homeboy_owned_ci_artifact("inc/class-foo.php"));
        assert!(!is_homeboy_owned_ci_artifact("homeboy.json"));
        // A similarly-prefixed source dir must NOT match — only the exact
        // artifact prefixes are tolerated.
        assert!(!is_homeboy_owned_ci_artifact("homeboy-src/lib.rs"));
    }

    #[test]
    fn dirty_changes_only_homeboy_artifacts_is_true_for_cleaned_artifact_dirs() {
        let changes = UncommittedChanges {
            has_changes: true,
            staged: Vec::new(),
            unstaged: vec![
                "homeboy-ci-results".to_string(),
                "homeboy-observations".to_string(),
            ],
            untracked: Vec::new(),
            hint: None,
        };

        assert!(dirty_changes_are_only_homeboy_artifacts(&changes));
    }

    #[test]
    fn dirty_changes_only_homeboy_artifacts_is_true_for_artifact_tree() {
        let changes = artifact_only_changes(&["homeboy-ci-results/", "homeboy-observations/"]);
        assert!(dirty_changes_are_only_homeboy_artifacts(&changes));
    }

    #[test]
    fn dirty_changes_only_homeboy_artifacts_is_false_for_mixed_tree() {
        let mut changes = artifact_only_changes(&["homeboy-ci-results/"]);
        // A real source file alongside the artifact must keep the guard active.
        changes.unstaged.push("src/lib.rs".to_string());
        assert!(!dirty_changes_are_only_homeboy_artifacts(&changes));
    }

    #[test]
    fn ensure_clean_enough_for_write_allows_artifact_only_dirty_tree() {
        let root = tmp_dir("artifact-only-guard");
        init_test_repo(&root);
        fs::create_dir_all(root.join("homeboy-ci-results")).unwrap();
        fs::write(root.join("homeboy-ci-results").join("review.json"), "{}\n").unwrap();
        let request = lint_refactor_request(
            test_component(&root),
            root.clone(),
            Vec::new(),
            LintSourceOptions::default(),
            true,
        );
        let changes = artifact_only_changes(&["homeboy-ci-results/review.json"]);

        ensure_clean_enough_for_write(&request, Some(&changes))
            .expect("write should be allowed when only Homeboy CI artifacts are dirty");
        assert!(!root.join("homeboy-ci-results").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_clean_enough_for_write_blocks_real_source_dirty_tree() {
        let root = PathBuf::from("/tmp/homeboy-real-source-guard");
        let request = lint_refactor_request(
            test_component(&root),
            root,
            Vec::new(),
            LintSourceOptions::default(),
            true,
        );
        let changes = UncommittedChanges {
            has_changes: true,
            staged: Vec::new(),
            unstaged: vec!["src/lib.rs".to_string()],
            untracked: vec!["homeboy-ci-results/".to_string()],
            hint: None,
        };

        let err = ensure_clean_enough_for_write(&request, Some(&changes))
            .expect_err("write should be blocked when real source is dirty");
        assert!(err
            .to_string()
            .contains("Working tree has uncommitted changes"));
    }

    #[test]
    fn ensure_clean_enough_for_write_force_bypasses_dirty_tree() {
        let root = PathBuf::from("/tmp/homeboy-force-guard");
        let mut request = lint_refactor_request(
            test_component(&root),
            root,
            Vec::new(),
            LintSourceOptions::default(),
            true,
        );
        request.force = true;
        let changes = UncommittedChanges {
            has_changes: true,
            staged: Vec::new(),
            unstaged: vec!["src/lib.rs".to_string()],
            untracked: Vec::new(),
            hint: None,
        };

        ensure_clean_enough_for_write(&request, Some(&changes))
            .expect("--force should bypass the dirty-tree guard");
    }

    /// Real-git integration: a checkout dirty only with Homeboy-owned CI
    /// artifacts must not trip the guard inside `collect_refactor_sources`.
    #[test]
    fn collect_refactor_sources_write_passes_guard_for_artifact_only_dirty_repo() {
        let root = tmp_dir("dirty-repo-artifact-only");
        init_test_repo(&root);
        fs::create_dir_all(root.join("homeboy-ci-results")).unwrap();
        fs::write(root.join("homeboy-ci-results").join("review.json"), "{}\n").unwrap();

        // Set a cached clean lint result so the lint stage short-circuits
        // without invoking the extension runner — isolating the guard test.
        let output_dir = tmp_dir("dirty-repo-artifact-output");
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(
            output_dir.join("lint.json"),
            serde_json::json!({"success": true, "data": {"passed": true, "findings": []}})
                .to_string(),
        )
        .unwrap();
        let prior_output_dir = std::env::var("HOMEBOY_OUTPUT_DIR").ok();
        std::env::set_var("HOMEBOY_OUTPUT_DIR", &output_dir);

        let component = test_component(&root);
        let result = collect_refactor_sources(lint_refactor_request(
            component,
            root.clone(),
            Vec::new(),
            LintSourceOptions::default(),
            true,
        ));

        match prior_output_dir {
            Some(value) => std::env::set_var("HOMEBOY_OUTPUT_DIR", value),
            None => std::env::remove_var("HOMEBOY_OUTPUT_DIR"),
        }

        let run = result.expect(
            "guard should be bypassed for a tree dirty only with Homeboy CI artifacts (#4684)",
        );
        assert!(
            !run.dry_run,
            "write-mode run should be reflected as applied-capable"
        );
        assert!(!root.join("homeboy-ci-results").exists());

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&output_dir);
    }

    /// Real-git integration: a checkout dirty with real source must still trip
    /// the guard inside `collect_refactor_sources`.
    #[test]
    fn collect_refactor_sources_write_blocks_guard_for_real_source_dirty_repo() {
        let root = tmp_dir("dirty-repo-real-source");
        init_test_repo(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("lib.rs"), "pub fn real_change() {}\n").unwrap();

        let component = test_component(&root);
        let err = collect_refactor_sources(lint_refactor_request(
            component,
            root.clone(),
            Vec::new(),
            LintSourceOptions::default(),
            true,
        ))
        .expect_err("guard should block write for a real-source-dirty tree");

        assert!(
            err.to_string()
                .contains("Working tree has uncommitted changes"),
            "expected dirty-tree guard error, got: {err}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    fn init_test_repo(path: &Path) {
        use std::process::Command;
        fn git(path: &Path, args: &[&str]) {
            let status = Command::new("git")
                .args(["-C", path.to_str().expect("utf8 path")])
                .args(args)
                .status()
                .expect("git command runs");
            assert!(status.success(), "git {:?} failed in {:?}", args, path);
        }
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        fs::write(path.join("README.md"), "initial\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);
    }

    #[test]
    fn test_build_test_refactor_request() {
        let root = PathBuf::from("/tmp/homeboy-test-refactor-request");
        let request = build_test_refactor_request(
            test_component(&root),
            root,
            Vec::new(),
            TestSourceOptions::default(),
            false,
        );

        assert_eq!(request.sources, vec!["test".to_string()]);
        assert!(!request.write);
    }

    #[test]
    fn collect_refactor_sources_audit_write_uses_audit_refactor_engine() {
        let root = tmp_dir("audit-write");
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let component = test_component(&root);
        let sources_run = collect_refactor_sources(RefactorSourceRequest {
            component,
            root: root.clone(),
            sources: vec!["audit".to_string()],
            changed_since: None,
            only: vec![crate::core::code_audit::AuditFinding::DuplicateFunction],
            exclude: vec![],
            settings: vec![],
            lint: LintSourceOptions::default(),
            test: TestSourceOptions::default(),
            write: true,
            force: false,
        })
        .unwrap();

        let audit_stage = sources_run
            .stages
            .iter()
            .find(|stage| stage.stage == "audit")
            .expect("audit stage present");

        assert!(audit_stage.collected);
        assert!(sources_run.collected_edits.is_empty());
        assert!(audit_stage.collected);
        assert!(audit_stage
            .warnings
            .iter()
            .any(|warning| warning.starts_with("audit refactor: ")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_stage_empty_selected_files_does_not_fall_back_to_broad_lint() {
        let root = tmp_dir("lint-empty-selected-files");
        fs::create_dir_all(&root).unwrap();
        let run_dir = RunDir::create().unwrap();
        let component = test_component(&root);

        let stage = run_lint_stage(
            &component,
            &root,
            &[],
            &LintSourceOptions {
                selected_files: Some(Vec::new()),
                ..Default::default()
            },
            None,
            true,
            &run_dir,
        )
        .expect("empty selected files should be a clean no-op");

        assert_eq!(stage.summary.stage, "lint");
        assert_eq!(stage.summary.detected_findings, Some(0));
        assert_eq!(stage.summary.files_modified, 0);
        assert!(stage.fix_results.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn formatter_failure_is_a_write_failure() {
        let result = require_successful_format(
            crate::core::engine::format_write::FormatResult {
                success: false,
                command: Some("format.sh src/file.php".to_string()),
                output: Some("phpcbf failed".to_string()),
                files_in_scope: 1,
            },
            "inter-stage formatter",
        );

        let err = result.expect_err("formatter failure should fail write mode");
        assert!(err.to_string().contains("inter-stage formatter"));
        assert!(err.to_string().contains("phpcbf failed"));
    }

    #[test]
    fn lint_fix_validation_rejects_remaining_findings() {
        let findings =
            vec![
                crate::core::finding::HomeboyFinding::builder("lint", "Tabs must be used")
                    .rule("WordPress.WhiteSpace.DisallowSpaceIndent")
                    .file("inc/Demo.php")
                    .fixable(true)
                    .build(),
            ];

        let err = reject_remaining_lint_fix_findings(&findings)
            .expect_err("remaining lint findings should fail fix mode");
        assert!(err.to_string().contains("Lint fix left 1 finding"));
        assert!(err.to_string().contains("inc/Demo.php"));
    }

    #[test]
    fn lint_fix_validation_accepts_clean_followup_diagnostics() {
        reject_remaining_lint_fix_findings(&[]).expect("clean follow-up diagnostics should pass");
    }

    #[test]
    fn test_collect_refactor_sources() {
        let _collect: fn(RefactorSourceRequest) -> crate::core::Result<RefactorSourceRun> =
            collect_refactor_sources;
    }

    #[test]
    fn normalize_sources_orders_known_sources() {
        let normalized =
            normalize_sources(&["test".to_string(), "audit".to_string(), "lint".to_string()])
                .expect("sources should normalize");

        assert_eq!(normalized, vec!["audit", "lint", "test"]);
    }

    #[test]
    fn test_normalize_sources() {
        assert_eq!(
            normalize_sources(&["test".to_string()]).unwrap(),
            vec!["test"]
        );
    }

    #[test]
    fn normalize_sources_rejects_unknown_sources() {
        let err =
            normalize_sources(&["weird".to_string()]).expect_err("unknown source should fail");
        assert!(err.to_string().contains("Unknown refactor source"));
    }

    // ============================================================================
    // homeboy#1159 — dry-run vs --write contract alignment
    // ============================================================================
    //
    // Regression tests for the CI deadlock where dry-run reports files_modified>0
    // for edits that `--write` silently declines (cascading findings, manual-only
    // fixes). Before the fix, dry-run exit 1 + write applies nothing = stuck PR.

    use crate::core::code_audit::AuditFinding;
    use crate::core::refactor::auto::{Fix, FixResult, Insertion, InsertionKind, NewFile};

    fn auto_insertion() -> Insertion {
        Insertion {
            primitive: None,
            kind: InsertionKind::MethodStub,
            finding: AuditFinding::MissingMethod,
            manual_only: false,
            auto_apply: true,
            blocked_reason: None,
            code: "fn foo() {}".to_string(),
            description: "stub method".to_string(),
        }
    }

    fn manual_only_insertion() -> Insertion {
        Insertion {
            primitive: None,
            kind: InsertionKind::MethodStub,
            finding: AuditFinding::IntraMethodDuplicate,
            manual_only: true,
            auto_apply: false,
            blocked_reason: Some(
                "Blocked: manual-only edit, not eligible for --from auto-write".to_string(),
            ),
            code: String::new(),
            description: "manual-only duplicate flag".to_string(),
        }
    }

    fn heuristic_manual_only_insertion() -> Insertion {
        Insertion {
            primitive: None,
            kind: InsertionKind::MethodStub,
            finding: AuditFinding::OrphanedTest,
            manual_only: false,
            auto_apply: false,
            blocked_reason: Some(
                "Blocked: heuristic confidence finding requires human review before automated writes"
                    .to_string(),
            ),
            code: String::new(),
            description: "manual-only orphaned test flag".to_string(),
        }
    }

    fn fix_result_with(fixes: Vec<Fix>, new_files: Vec<NewFile>) -> FixResult {
        let total_insertions = fixes.iter().map(|f| f.insertions.len()).sum();
        FixResult {
            fixes,
            new_files,
            decompose_plans: Vec::new(),
            skipped: Vec::new(),
            chunk_results: Vec::new(),
            total_insertions,
            files_modified: 0,
        }
    }

    #[test]
    fn collect_audit_changed_files_excludes_manual_only_only_fixes() {
        // The cascading-finding scenario from #1159 reproduction:
        //   dry-run: intramethodduplicate × edit_op_apply.rs — 2 collected edits
        //   --write: audit applied=false fixes_applied=0 files_modified=0
        // If every insertion in a fix is manual_only, --write drops the fix
        // entirely (policy.rs line ~94) — so dry-run must not count that file
        // as "would be modified".
        let fix = Fix {
            file: "src/core/engine/edit_op_apply.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![manual_only_insertion(), manual_only_insertion()],
            applied: false,
        };
        let result = fix_result_with(vec![fix], Vec::new());
        assert!(
            collect_audit_changed_files(&result).is_empty(),
            "Fix with only manual-only insertions should not count as would-modify"
        );
    }

    #[test]
    fn collect_audit_changed_files_excludes_heuristic_manual_only_fixes() {
        let fix = Fix {
            file: "tests/core/process_test.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![heuristic_manual_only_insertion()],
            applied: false,
        };
        let result = fix_result_with(vec![fix], Vec::new());
        assert!(
            collect_audit_changed_files(&result).is_empty(),
            "Manual-only heuristic fixes should not count as would-modify"
        );
    }

    #[test]
    fn collect_audit_changed_files_includes_mixed_fixes() {
        // A fix that has at least one auto-apply insertion WILL be partially
        // applied by --write (the manual-only insertions get filtered during
        // apply, but the fix as a whole survives), so dry-run correctly
        // reports the file as would-modify.
        let fix = Fix {
            file: "src/lib.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![manual_only_insertion(), auto_insertion()],
            applied: false,
        };
        let result = fix_result_with(vec![fix], Vec::new());
        assert_eq!(
            collect_audit_changed_files(&result),
            vec!["src/lib.rs".to_string()],
            "Mixed fix (manual-only + auto) should count as would-modify"
        );
    }

    #[test]
    fn collect_audit_changed_files_includes_auto_apply_fixes() {
        // Baseline: the normal case, fully auto-apply, still counted.
        let fix = Fix {
            file: "src/lib.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![auto_insertion()],
            applied: false,
        };
        let result = fix_result_with(vec![fix], Vec::new());
        assert_eq!(
            collect_audit_changed_files(&result),
            vec!["src/lib.rs".to_string()]
        );
    }

    #[test]
    fn collect_audit_changed_files_excludes_manual_only_new_files() {
        let nf = NewFile {
            file: "src/generated.rs".to_string(),
            primitive: None,
            finding: AuditFinding::MissingMethod,
            manual_only: true,
            auto_apply: false,
            blocked_reason: Some("manual-only".to_string()),
            content: String::new(),
            description: "would create".to_string(),
            written: false,
        };
        let result = fix_result_with(Vec::new(), vec![nf]);
        assert!(collect_audit_changed_files(&result).is_empty());
    }

    #[test]
    fn collect_audit_changed_files_includes_auto_apply_new_files() {
        let nf = NewFile {
            file: "src/generated.rs".to_string(),
            primitive: None,
            finding: AuditFinding::MissingMethod,
            manual_only: false,
            auto_apply: true,
            blocked_reason: None,
            content: "// generated".to_string(),
            description: "create".to_string(),
            written: false,
        };
        let result = fix_result_with(Vec::new(), vec![nf]);
        assert_eq!(
            collect_audit_changed_files(&result),
            vec!["src/generated.rs".to_string()]
        );
    }

    #[test]
    fn count_manual_only_fixes_counts_both_fixes_and_new_files() {
        let manual_fix = Fix {
            file: "src/a.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![manual_only_insertion(), manual_only_insertion()],
            applied: false,
        };
        let mixed_fix = Fix {
            file: "src/b.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![manual_only_insertion(), auto_insertion()],
            applied: false,
        };
        let auto_fix = Fix {
            file: "src/c.rs".to_string(),
            required_methods: Vec::new(),
            required_registrations: Vec::new(),
            insertions: vec![auto_insertion()],
            applied: false,
        };
        let manual_nf = NewFile {
            file: "src/d.rs".to_string(),
            primitive: None,
            finding: AuditFinding::MissingMethod,
            manual_only: true,
            auto_apply: false,
            blocked_reason: None,
            content: String::new(),
            description: String::new(),
            written: false,
        };
        let result = fix_result_with(vec![manual_fix, mixed_fix, auto_fix], vec![manual_nf]);
        // Only the entirely-manual-only fix + the manual-only new file count.
        // The mixed fix survives --write, the fully-auto fix is normal.
        assert_eq!(count_manual_only_fixes(&result), 2);
    }
}
