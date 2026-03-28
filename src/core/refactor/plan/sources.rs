use crate::code_audit::CodeAuditResult;
use crate::component::Component;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::extension;
use crate::extension::test::compute_changed_test_files;
use crate::git;
use crate::refactor::auto as fixer;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::Error;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::verify::AuditConvergenceScoring;

pub const KNOWN_REFACTOR_SOURCES: &[&str] = &["audit", "lint", "test"];

/// Name of the env var pointing to previous command output files.
///
/// When set, `--from audit` reads the cached audit result instead of
/// re-running the audit. The action sets this during `run-homeboy-commands.sh`
/// and it persists across steps via `GITHUB_ENV`.
const OUTPUT_DIR_ENV: &str = "HOMEBOY_OUTPUT_DIR";

#[derive(Debug, Clone)]
pub struct RefactorSourceRequest {
    pub component: Component,
    pub root: PathBuf,
    pub sources: Vec<String>,
    pub changed_since: Option<String>,
    pub only: Vec<crate::code_audit::AuditFinding>,
    pub exclude: Vec<crate::code_audit::AuditFinding>,
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

pub fn test_refactor_request(
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

pub fn run_lint_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> crate::Result<RefactorSourceRun> {
    collect_refactor_sources(lint_refactor_request(
        component, root, settings, options, write,
    ))
}

pub fn run_test_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> crate::Result<RefactorSourceRun> {
    collect_refactor_sources(test_refactor_request(
        component, root, settings, options, write,
    ))
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
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceStageSummary {
    pub stage: String,
    pub collected: bool,
    pub applied: bool,
    pub edit_count: usize,
    pub files_modified: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_findings: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceOverlap {
    pub file: String,
    pub earlier_stage: String,
    pub later_stage: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceTotals {
    pub stages_with_edits: usize,
    pub total_edits: usize,
    pub total_files_selected: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CollectedEdit {
    pub source: String,
    pub file: String,
    pub rule_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Default)]
struct FixAccumulator {
    fixes: Vec<FixApplied>,
}

impl FixAccumulator {
    fn extend(&mut self, items: Vec<FixApplied>) {
        self.fixes.extend(items);
    }

    fn summary(&self) -> Option<FixResultsSummary> {
        if self.fixes.is_empty() {
            None
        } else {
            Some(auto::summarize_fix_results(&self.fixes))
        }
    }
}

struct PlannedStage {
    source: String,
    summary: SourceStageSummary,
    fix_results: Vec<FixApplied>,
}

pub fn collect_refactor_sources(
    request: RefactorSourceRequest,
) -> crate::Result<RefactorSourceRun> {
    let sources = normalize_sources(&request.sources)?;
    let root_str = request.root.to_string_lossy().to_string();
    let original_changes = git::get_uncommitted_changes(&root_str).ok();

    // Refuse to write to a dirty working tree unless --force is set.
    // Refactoring operates directly on the working tree, so mixing auto-generated
    // fixes with uncommitted manual changes makes rollback difficult.
    // Dry runs (no --write) are always safe — they don't modify files.
    if request.write && !request.force {
        if let Some(ref changes) = original_changes {
            if changes.has_changes {
                return Err(crate::Error::validation_invalid_argument(
                    "write",
                    "Working tree has uncommitted changes",
                    None,
                    Some(vec![
                        "Commit or stash your changes first".to_string(),
                        "Or use --force to proceed anyway".to_string(),
                    ]),
                ));
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
            "audit" => plan_audit_stage(
                &request.component.id,
                &request.root,
                scoped_changed_files.as_deref(),
                &request.only,
                &request.exclude,
                request.write,
            )?,
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
            format_changed_files(&request.root, &stage.summary.changed_files, &mut warnings);
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
        match crate::engine::format_write::format_after_write(&request.root, &abs_changed) {
            Ok(fmt) => {
                if let Some(cmd) = &fmt.command {
                    if !fmt.success {
                        warnings.push(format!("Formatter ({}) exited non-zero", cmd));
                    }
                }
            }
            Err(e) => {
                crate::log_status!("format", "Warning: post-write format failed: {}", e);
            }
        }
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
    })
}

pub fn normalize_sources(sources: &[String]) -> crate::Result<Vec<String>> {
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

/// Format modified files between refactor stages.
///
/// This ensures generated code (test files, refactored sources) is properly
/// formatted before subsequent stages run. Without this, the lint stage's
/// `cargo fmt --check` fails on unformatted auto-generated code — blocking
/// the pipeline on problems it didn't create.
///
/// Uses the same `format_after_write` as the post-write step. Non-fatal:
/// if formatting fails, it logs a warning and continues.
fn format_changed_files(root: &Path, changed_files: &[String], warnings: &mut Vec<String>) {
    if changed_files.is_empty() {
        return;
    }

    let abs_changed: Vec<PathBuf> = changed_files.iter().map(|f| root.join(f)).collect();

    match crate::engine::format_write::format_after_write(root, &abs_changed) {
        Ok(fmt) => {
            if let Some(cmd) = &fmt.command {
                if fmt.success {
                    crate::log_status!(
                        "format",
                        "Formatted {} file(s) via {} (inter-stage)",
                        abs_changed.len(),
                        cmd
                    );
                } else {
                    warnings.push(format!(
                        "Inter-stage formatter ({}) exited non-zero (continuing)",
                        cmd
                    ));
                }
            }
        }
        Err(e) => {
            crate::log_status!(
                "format",
                "Warning: inter-stage format failed (continuing): {}",
                e
            );
        }
    }
}

fn collect_collected_edits(stages: &[PlannedStage]) -> Vec<CollectedEdit> {
    let mut edits = Vec::new();

    for stage in stages {
        for fix in &stage.fix_results {
            edits.push(CollectedEdit {
                source: stage.source.clone(),
                file: fix.file.clone(),
                rule_id: fix.rule.clone(),
                action: fix.action.clone(),
            });
        }
    }

    edits.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.file.cmp(&b.file))
            .then(a.rule_id.cmp(&b.rule_id))
    });

    edits
}

fn collect_stage_changed_files(stages: &[SourceStageSummary]) -> Vec<String> {
    let mut final_changed_files = BTreeSet::new();
    for stage in stages {
        for file in &stage.changed_files {
            final_changed_files.insert(file.clone());
        }
    }
    final_changed_files.into_iter().collect()
}

/// Try to load a cached audit result from a previous `homeboy audit` run.
///
/// Checks `HOMEBOY_OUTPUT_DIR/audit.json` for a `CliResponse<CodeAuditResult>`
/// envelope. If found and parseable, returns the `CodeAuditResult` without
/// re-running the audit. This avoids redundant full-codebase scans when the
/// refactor step runs after an audit gate that already produced the results.
///
/// Returns `None` if:
/// - `HOMEBOY_OUTPUT_DIR` is not set
/// - The file doesn't exist
/// - The file can't be parsed (e.g. the audit failed and wrote an error envelope)
fn try_load_cached_audit() -> Option<CodeAuditResult> {
    let output_dir = std::env::var(OUTPUT_DIR_ENV).ok()?;
    let audit_file = PathBuf::from(&output_dir).join("audit.json");

    let content = std::fs::read_to_string(&audit_file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Only use cached results from successful runs
    if !json.get("success")?.as_bool()? {
        return None;
    }

    // The `--output` envelope wraps the audit in a `data` field
    let data = json.get("data")?;
    let result: CodeAuditResult = serde_json::from_value(data.clone()).ok()?;

    crate::log_status!(
        "refactor",
        "Using cached audit result ({} findings from {})",
        result.findings.len(),
        audit_file.display()
    );

    Some(result)
}

fn plan_audit_stage(
    component_id: &str,
    root: &Path,
    changed_files: Option<&[String]>,
    only: &[crate::code_audit::AuditFinding],
    exclude: &[crate::code_audit::AuditFinding],
    write: bool,
) -> crate::Result<PlannedStage> {
    let result = if let Some(cached) = try_load_cached_audit() {
        cached
    } else if let Some(changed) = changed_files {
        if changed.is_empty() {
            CodeAuditResult {
                component_id: component_id.to_string(),
                source_path: root.to_string_lossy().to_string(),
                summary: crate::code_audit::AuditSummary {
                    files_scanned: 0,
                    conventions_detected: 0,
                    outliers_found: 0,
                    alignment_score: None,
                    files_skipped: 0,
                    warnings: vec![],
                },
                conventions: vec![],
                directory_conventions: vec![],
                findings: vec![],
                duplicate_groups: vec![],
            }
        } else {
            crate::code_audit::audit_path_scoped(
                component_id,
                &root.to_string_lossy(),
                changed,
                None,
            )?
        }
    } else {
        crate::code_audit::audit_path_with_id(component_id, &root.to_string_lossy())?
    };

    let policy = fixer::FixPolicy {
        only: (!only.is_empty()).then_some(only.to_vec()),
        exclude: exclude.to_vec(),
    };
    let mut fix_result = super::generate::generate_audit_fixes(&result, root, &policy);
    let (fix_result, policy_summary, changed_files, stage_warnings): (
        fixer::FixResult,
        fixer::PolicySummary,
        Vec<String>,
        Vec<String>,
    ) = if write {
        // Single pass: generate fixes from the provided findings, apply, validate.
        // The audit already ran and provided findings in `result` — the refactor
        // command does not re-run the audit internally. The convergence loop
        // (audit → fix → merge → re-audit) belongs in the full orchestration
        // pipeline, not inside a single refactor invocation.
        let outcome = super::verify::run_audit_refactor(
            result.clone(),
            only,
            exclude,
            AuditConvergenceScoring::default(),
            1,
            true,
        )?;

        let changed_files = outcome
            .fix_result
            .chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .flat_map(|chunk| chunk.files.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        let warnings = outcome
            .iterations
            .iter()
            .filter(|iteration| iteration.status != "continued")
            .map(|iteration| {
                format!(
                    "audit iteration {}: {}",
                    iteration.iteration, iteration.status
                )
            })
            .collect::<Vec<_>>();

        (
            outcome.fix_result,
            outcome.policy_summary,
            changed_files,
            warnings,
        )
    } else {
        let policy_summary = fixer::apply_fix_policy(&mut fix_result, false, &policy);
        let changed_files = collect_audit_changed_files(&fix_result);
        (fix_result, policy_summary, changed_files, Vec::new())
    };

    let fix_results = summarize_audit_fix_result_entries(&fix_result);
    let edit_count = fix_results.len();

    Ok(PlannedStage {
        source: "audit".to_string(),
        summary: SourceStageSummary {
            stage: "audit".to_string(),
            collected: true,
            applied: write && !changed_files.is_empty(),
            edit_count,
            files_modified: changed_files.len(),
            detected_findings: Some(result.findings.len()),
            changed_files,
            fix_summary: if write {
                if fix_result.files_modified > 0 {
                    Some(auto::summarize_audit_fix_result(&fix_result))
                } else {
                    None
                }
            } else if policy_summary.visible_insertions + policy_summary.visible_new_files > 0 {
                Some(auto::summarize_audit_fix_result(&fix_result))
            } else {
                None
            },
            warnings: stage_warnings,
        },
        fix_results,
    })
}

fn run_lint_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &LintSourceOptions,
    changed_files: Option<&[String]>,
    write: bool,
    run_dir: &RunDir,
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
    let fix_sidecars = auto::AutofixSidecarFiles::for_run_dir(run_dir);
    let before_dirty = if write {
        git::get_dirty_files(&root_str).unwrap_or_default()
    } else {
        Vec::new()
    };

    let selected_files = options.selected_files.as_deref().or(changed_files);
    let effective_glob = if let Some(changed_files) = selected_files {
        if changed_files.is_empty() {
            None
        } else {
            let abs_files: Vec<String> = changed_files
                .iter()
                .map(|f| format!("{}/{}", root_str, f))
                .collect();
            if abs_files.len() == 1 {
                Some(abs_files[0].clone())
            } else {
                Some(format!("{{{}}}", abs_files.join(",")))
            }
        }
    } else {
        options.glob.clone()
    };

    let runner = extension::lint::build_lint_runner(
        component,
        None,
        settings,
        false,
        options.file.as_deref(),
        effective_glob.as_deref(),
        options.errors_only,
        options.sniffs.as_deref(),
        options.exclude_sniffs.as_deref(),
        options.category.as_deref(),
        run_dir,
    )?
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    runner.run()?;

    let stage_changed_files = if write {
        let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let before_set: HashSet<&str> = before_dirty.iter().map(|s| s.as_str()).collect();
        after_dirty
            .into_iter()
            .filter(|f| !before_set.contains(f.as_str()))
            .collect()
    } else {
        Vec::new()
    };

    let fix_results = fix_sidecars.consume_fix_results();
    let edit_count = fix_results.len();
    let lint_findings =
        crate::extension::lint::baseline::parse_findings_file(&findings_file).unwrap_or_default();

    Ok(PlannedStage {
        source: "lint".to_string(),
        summary: SourceStageSummary {
            stage: "lint".to_string(),
            collected: true,
            applied: write && !stage_changed_files.is_empty(),
            edit_count,
            files_modified: stage_changed_files.len(),
            detected_findings: Some(lint_findings.len()),
            changed_files: stage_changed_files,
            fix_summary: auto::summarize_optional_fix_results(&fix_results),
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn run_test_stage(
    component: &Component,
    root: &Path,
    settings: &[(String, String)],
    options: &TestSourceOptions,
    changed_test_files: Option<&[String]>,
    write: bool,
    run_dir: &RunDir,
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let fix_sidecars = auto::AutofixSidecarFiles::for_run_dir(run_dir);
    let before_dirty = if write {
        git::get_dirty_files(&root_str).unwrap_or_default()
    } else {
        Vec::new()
    };

    let selected_test_files = options.selected_files.as_deref().or(changed_test_files);

    let mut runner = extension::test::build_test_runner(
        component,
        None,
        settings,
        options.skip_lint,
        false,
        None,
        selected_test_files,
        run_dir,
    )?
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    if !options.script_args.is_empty() {
        runner = runner.script_args(&options.script_args);
    }

    runner.run()?;

    let stage_changed_files = if write {
        let after_dirty = git::get_dirty_files(&root_str).unwrap_or_default();
        let before_set: HashSet<&str> = before_dirty.iter().map(|s| s.as_str()).collect();
        after_dirty
            .into_iter()
            .filter(|f| !before_set.contains(f.as_str()))
            .collect()
    } else {
        Vec::new()
    };

    let fix_results = fix_sidecars.consume_fix_results();
    let edit_count = fix_results.len();
    Ok(PlannedStage {
        source: "test".to_string(),
        summary: SourceStageSummary {
            stage: "test".to_string(),
            collected: true,
            applied: write && !stage_changed_files.is_empty(),
            edit_count,
            files_modified: stage_changed_files.len(),
            detected_findings: None,
            changed_files: stage_changed_files,
            fix_summary: auto::summarize_optional_fix_results(&fix_results),
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn collect_audit_changed_files(fix_result: &fixer::FixResult) -> Vec<String> {
    let mut files = BTreeSet::new();
    for fix in &fix_result.fixes {
        if !fix.insertions.is_empty() {
            files.insert(fix.file.clone());
        }
    }
    for file in &fix_result.new_files {
        files.insert(file.file.clone());
    }
    files.into_iter().collect()
}

fn summarize_audit_fix_result_entries(fix_result: &fixer::FixResult) -> Vec<FixApplied> {
    let mut entries = Vec::new();

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                entries.push(FixApplied {
                    file: fix.file.clone(),
                    rule: format!("{:?}", insertion.finding).to_lowercase(),
                    action: Some("insert".to_string()),
                    primitive: insertion.primitive.as_ref().map(auto::primitive_name),
                });
            }
        }
    }

    for new_file in &fix_result.new_files {
        entries.push(FixApplied {
            file: new_file.file.clone(),
            rule: format!("{:?}", new_file.finding).to_lowercase(),
            action: Some("create".to_string()),
            primitive: new_file.primitive.as_ref().map(auto::primitive_name),
        });
    }

    entries
}

pub fn analyze_stage_overlaps(stages: &[SourceStageSummary]) -> Vec<SourceOverlap> {
    let mut overlaps = Vec::new();

    for (later_index, later_stage) in stages.iter().enumerate() {
        if later_stage.changed_files.is_empty() {
            continue;
        }

        let later_files: BTreeSet<&str> = later_stage
            .changed_files
            .iter()
            .map(String::as_str)
            .collect();

        for earlier_stage in stages.iter().take(later_index) {
            if earlier_stage.changed_files.is_empty() {
                continue;
            }

            for file in earlier_stage.changed_files.iter().map(String::as_str) {
                if later_files.contains(file) {
                    overlaps.push(SourceOverlap {
                        file: file.to_string(),
                        earlier_stage: earlier_stage.stage.clone(),
                        later_stage: later_stage.stage.clone(),
                        resolution: format!(
                            "{} pass ran after {} in pipeline sequence",
                            later_stage.stage, earlier_stage.stage
                        ),
                    });
                }
            }
        }
    }

    overlaps.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.earlier_stage.cmp(&b.earlier_stage))
            .then(a.later_stage.cmp(&b.later_stage))
    });

    overlaps
}

pub fn summarize_source_totals(
    stages: &[SourceStageSummary],
    total_files_selected: usize,
) -> SourceTotals {
    SourceTotals {
        stages_with_edits: stages.iter().filter(|stage| stage.edit_count > 0).count(),
        total_edits: stages.iter().map(|stage| stage.edit_count).sum(),
        total_files_selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
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
    fn analyze_stage_overlaps_reports_later_stage_precedence() {
        let stages = vec![
            SourceStageSummary {
                stage: "audit".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "lint".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 2,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "test".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: None,
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let overlaps = analyze_stage_overlaps(&stages);

        assert_eq!(
            overlaps,
            vec![
                SourceOverlap {
                    file: "src/lib.rs".to_string(),
                    earlier_stage: "audit".to_string(),
                    later_stage: "lint".to_string(),
                    resolution: "lint pass ran after audit in pipeline sequence".to_string(),
                },
                SourceOverlap {
                    file: "src/main.rs".to_string(),
                    earlier_stage: "lint".to_string(),
                    later_stage: "test".to_string(),
                    resolution: "test pass ran after lint in pipeline sequence".to_string(),
                },
            ]
        );
    }

    #[test]
    fn analyze_stage_overlaps_ignores_disjoint_files() {
        let stages = vec![
            SourceStageSummary {
                stage: "audit".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "lint".to_string(),
                collected: true,
                applied: true,
                edit_count: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        assert!(analyze_stage_overlaps(&stages).is_empty());
    }

    #[test]
    fn summarize_source_totals_counts_stage_and_fix_totals() {
        let stages = vec![
            SourceStageSummary {
                stage: "audit".to_string(),
                collected: true,
                applied: false,
                edit_count: 2,
                files_modified: 1,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "lint".to_string(),
                collected: true,
                applied: false,
                edit_count: 0,
                files_modified: 0,
                detected_findings: Some(1),
                changed_files: Vec::new(),
                fix_summary: None,
                warnings: Vec::new(),
            },
            SourceStageSummary {
                stage: "test".to_string(),
                collected: true,
                applied: false,
                edit_count: 3,
                files_modified: 2,
                detected_findings: None,
                changed_files: vec!["tests/foo.rs".to_string(), "tests/bar.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let totals = summarize_source_totals(&stages, 3);

        assert_eq!(totals.stages_with_edits, 2);
        assert_eq!(totals.total_edits, 5);
        assert_eq!(totals.total_files_selected, 3);
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
            only: vec![crate::code_audit::AuditFinding::DuplicateFunction],
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
            .any(|warning| warning.starts_with("audit iteration ")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn try_load_cached_audit_reads_output_dir() {
        std::env::remove_var(OUTPUT_DIR_ENV);
        let dir = tmp_dir("cached-audit");
        fs::create_dir_all(&dir).unwrap();
        let audit_result = CodeAuditResult {
            component_id: "test".to_string(),
            source_path: "/tmp/test".to_string(),
            summary: crate::code_audit::AuditSummary {
                files_scanned: 10,
                conventions_detected: 2,
                outliers_found: 1,
                alignment_score: None,
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![],
            duplicate_groups: vec![],
        };

        // Write a CliResponse envelope
        let envelope = serde_json::json!({
            "success": true,
            "data": audit_result,
        });
        fs::write(
            dir.join("audit.json"),
            serde_json::to_string_pretty(&envelope).unwrap(),
        )
        .unwrap();

        // Set the env var and load
        std::env::set_var(OUTPUT_DIR_ENV, dir.to_string_lossy().as_ref());
        let loaded = try_load_cached_audit();
        std::env::remove_var(OUTPUT_DIR_ENV);

        let loaded = loaded.expect("should load cached audit");
        assert_eq!(loaded.component_id, "test");
        assert_eq!(loaded.summary.files_scanned, 10);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn try_load_cached_audit_skips_failed_envelope() {
        std::env::remove_var(OUTPUT_DIR_ENV);
        let dir = tmp_dir("cached-audit-fail");
        fs::create_dir_all(&dir).unwrap();
        let envelope = serde_json::json!({
            "success": false,
            "error": {
                "code": "internal.io_error",
                "message": "something broke",
                "details": {},
            },
        });
        fs::write(
            dir.join("audit.json"),
            serde_json::to_string_pretty(&envelope).unwrap(),
        )
        .unwrap();

        std::env::set_var(OUTPUT_DIR_ENV, dir.to_string_lossy().as_ref());
        let loaded = try_load_cached_audit();
        std::env::remove_var(OUTPUT_DIR_ENV);

        assert!(loaded.is_none(), "should not use failed audit result");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn try_load_cached_audit_returns_none_when_unset() {
        std::env::remove_var(OUTPUT_DIR_ENV);
        assert!(try_load_cached_audit().is_none());
    }

    #[test]
    fn normalize_sources_orders_known_sources() {
        let normalized =
            normalize_sources(&["test".to_string(), "audit".to_string(), "lint".to_string()])
                .expect("sources should normalize");

        assert_eq!(normalized, vec!["audit", "lint", "test"]);
    }

    #[test]
    fn normalize_sources_rejects_unknown_sources() {
        let err =
            normalize_sources(&["weird".to_string()]).expect_err("unknown source should fail");
        assert!(err.to_string().contains("Unknown refactor source"));
    }
}
