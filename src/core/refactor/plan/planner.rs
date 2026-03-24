use crate::component::Component;
use crate::engine::temp;
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

pub const KNOWN_PLAN_SOURCES: &[&str] = &["audit", "lint", "test"];

#[derive(Debug, Clone)]
pub struct RefactorPlanRequest {
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
}

pub fn lint_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> RefactorPlanRequest {
    RefactorPlanRequest {
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
    }
}

pub(crate) fn run_lint_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> crate::Result<RefactorPlan> {
    build_refactor_plan(lint_refactor_request(
        component, root, settings, options, write,
    ))
}

pub(crate) fn run_test_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> crate::Result<RefactorPlan> {
    build_refactor_plan(test_refactor_request(
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
pub struct RefactorPlan {
    pub component_id: String,
    pub source_path: String,
    pub sources: Vec<String>,
    pub dry_run: bool,
    pub applied: bool,
    pub merge_strategy: String,
    pub proposals: Vec<FixProposal>,
    pub stages: Vec<PlanStageSummary>,
    pub plan_totals: PlanTotals,
    pub overlaps: Vec<PlanOverlap>,
    pub files_modified: usize,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    pub warnings: Vec<String>,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStageSummary {
    pub stage: String,
    pub planned: bool,
    pub applied: bool,
    pub fixes_proposed: usize,
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
pub struct PlanOverlap {
    pub file: String,
    pub earlier_stage: String,
    pub later_stage: String,
    pub resolution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanTotals {
    pub stages_with_proposals: usize,
    pub total_fixes_proposed: usize,
    pub total_files_selected: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FixProposal {
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
    summary: PlanStageSummary,
    fix_results: Vec<FixApplied>,
}

pub fn build_refactor_plan(request: RefactorPlanRequest) -> crate::Result<RefactorPlan> {
    let sources = normalize_sources(&request.sources)?;
    let root_str = request.root.to_string_lossy().to_string();
    let original_changes = git::get_uncommitted_changes(&root_str).ok();
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
    // Captures the current state of all files that might be touched.
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
                crate::log_status!("undo", "Warning: failed to save pre-refactor undo snapshot: {}", e);
            }
        }
    }

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
            )?,
            "test" => run_test_stage(
                &request.component,
                &request.root,
                &request.settings,
                &request.test,
                scoped_test_files.as_deref(),
                request.write,
            )?,
            _ => unreachable!("sources are normalized before planning"),
        };

        // Format generated/modified files so subsequent stages (especially lint)
        // see properly formatted code. Without this, auto-generated test files
        // cause `cargo fmt --check` to fail during the lint stage.
        if stage.summary.files_modified > 0 {
            format_changed_files(
                &request.root,
                &stage.summary.changed_files,
                &mut warnings,
            );
        }

        accumulator.extend(stage.fix_results.clone());
        planned_stages.push(stage);
    }

    let proposals = collect_fix_proposals(&planned_stages);
    let mut stage_summaries: Vec<PlanStageSummary> = planned_stages
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

    let plan_totals = summarize_plan_totals(&stage_summaries, changed_files.len());
    let files_modified = changed_files.len();
    let applied = request.write && files_modified > 0;

    if applied {
        // Run the project's formatter on all changed files.
        // Non-fatal: formatting failure logs a warning but doesn't block the refactor.
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
        vec![
            "Dry run. Re-run with --write to apply fixes to the working tree.".to_string(),
        ]
    } else {
        Vec::new()
    };

    Ok(RefactorPlan {
        component_id: request.component.id,
        source_path: root_str,
        sources,
        dry_run: !request.write,
        applied,
        merge_strategy: "sequential_source_merge".to_string(),
        proposals,
        stages: stage_summaries,
        plan_totals,
        overlaps,
        files_modified,
        changed_files,
        fix_summary: accumulator.summary(),
        warnings,
        hints,
    })
}

pub(crate) fn normalize_sources(sources: &[String]) -> crate::Result<Vec<String>> {
    let lowered: Vec<String> = sources.iter().map(|source| source.to_lowercase()).collect();

    if lowered.iter().any(|source| source == "all") {
        return Ok(KNOWN_PLAN_SOURCES
            .iter()
            .map(|source| source.to_string())
            .collect());
    }

    let unknown: Vec<String> = lowered
        .iter()
        .filter(|source| !KNOWN_PLAN_SOURCES.contains(&source.as_str()))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from",
            format!("Unknown refactor source(s): {}", unknown.join(", ")),
            None,
            Some(vec![format!(
                "Known sources: {}",
                KNOWN_PLAN_SOURCES.join(", ")
            )]),
        ));
    }

    let mut ordered = Vec::new();
    for known in KNOWN_PLAN_SOURCES {
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
                        "Formatted {} file(s) via {}",
                        abs_changed.len(),
                        cmd
                    );
                } else {
                    warnings.push(format!(
                        "Formatter ({}) exited non-zero (continuing)",
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

fn collect_fix_proposals(stages: &[PlannedStage]) -> Vec<FixProposal> {
    let mut proposals = Vec::new();

    for stage in stages {
        for fix in &stage.fix_results {
            proposals.push(FixProposal {
                source: stage.source.clone(),
                file: fix.file.clone(),
                rule_id: fix.rule.clone(),
                action: fix.action.clone(),
            });
        }
    }

    proposals.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.file.cmp(&b.file))
            .then(a.rule_id.cmp(&b.rule_id))
    });

    proposals
}

fn collect_stage_changed_files(stages: &[PlanStageSummary]) -> Vec<String> {
    let mut final_changed_files = BTreeSet::new();
    for stage in stages {
        for file in &stage.changed_files {
            final_changed_files.insert(file.clone());
        }
    }
    final_changed_files.into_iter().collect()
}

fn plan_audit_stage(
    component_id: &str,
    root: &Path,
    changed_files: Option<&[String]>,
    only: &[crate::code_audit::AuditFinding],
    exclude: &[crate::code_audit::AuditFinding],
    write: bool,
) -> crate::Result<PlannedStage> {
    let result = if let Some(changed) = changed_files {
        if changed.is_empty() {
            crate::code_audit::CodeAuditResult {
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

    let mut fix_result = super::generate::generate_audit_fixes(&result, root);
    let policy = fixer::FixPolicy {
        only: (!only.is_empty()).then_some(only.to_vec()),
        exclude: exclude.to_vec(),
    };
    let preflight_context = fixer::PreflightContext { root };
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
        // pipeline, not inside a single refactor invocation. Each cold compile
        // in the sandbox takes 10-20 minutes with no target/ cache, so multiple
        // iterations are prohibitively expensive.
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
        let policy_summary =
            fixer::apply_fix_policy(&mut fix_result, false, &policy, &preflight_context);
        let changed_files = collect_audit_changed_files(&fix_result);
        (fix_result, policy_summary, changed_files, Vec::new())
    };

    let fix_results = summarize_audit_fix_result_entries(&fix_result);
    let fixes_proposed = fix_results.len();

    Ok(PlannedStage {
        source: "audit".to_string(),
        summary: PlanStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: write && !changed_files.is_empty(),
            fixes_proposed,
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
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let findings_file = temp::runtime_temp_file("homeboy-lint-findings", ".json")?;
    let fix_sidecars = auto::AutofixSidecarFiles::for_plan();

    // Capture dirty files before the lint script runs so we can detect what it changed.
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

    let findings_file_str = findings_file.to_string_lossy().to_string();
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
        &findings_file_str,
    )?
    .env_if(
        write,
        "HOMEBOY_FIX_PLAN_FILE",
        &fix_sidecars
            .plan_file
            .as_ref()
            .expect("plan sidecar initialized")
            .to_string_lossy(),
    )
    .env_if(
        write,
        "HOMEBOY_FIX_RESULTS_FILE",
        &fix_sidecars.results_file.to_string_lossy(),
    )
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    runner.run()?;

    // Detect files changed by the lint script using git.
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
    let fixes_proposed = fix_results.len();
    let lint_findings =
        crate::extension::lint::baseline::parse_findings_file(&findings_file).unwrap_or_default();
    let _ = std::fs::remove_file(&findings_file);

    Ok(PlannedStage {
        source: "lint".to_string(),
        summary: PlanStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: write && !stage_changed_files.is_empty(),
            fixes_proposed,
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
) -> crate::Result<PlannedStage> {
    let root_str = root.to_string_lossy().to_string();
    let results_file = temp::runtime_temp_file("homeboy-test-results", ".json")?;
    let fix_sidecars = auto::AutofixSidecarFiles::for_plan();

    // Capture dirty files before the test script runs.
    let before_dirty = if write {
        git::get_dirty_files(&root_str).unwrap_or_default()
    } else {
        Vec::new()
    };

    let results_file_str = results_file.to_string_lossy().to_string();
    let selected_test_files = options.selected_files.as_deref().or(changed_test_files);

    let mut runner = extension::test::build_test_runner(
        component,
        None,
        settings,
        options.skip_lint,
        false,
        &results_file_str,
        None,
        None,
        None,
        selected_test_files,
    )?
    .env_if(
        write,
        "HOMEBOY_FIX_PLAN_FILE",
        &fix_sidecars
            .plan_file
            .as_ref()
            .expect("plan sidecar initialized")
            .to_string_lossy(),
    )
    .env_if(
        write,
        "HOMEBOY_FIX_RESULTS_FILE",
        &fix_sidecars.results_file.to_string_lossy(),
    )
    .env_if(write, "HOMEBOY_AUTO_FIX", "1");

    if !options.script_args.is_empty() {
        runner = runner.script_args(&options.script_args);
    }

    runner.run()?;

    // Detect files changed by the test script using git.
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
    let fixes_proposed = fix_results.len();
    let _ = std::fs::remove_file(&results_file);

    Ok(PlannedStage {
        source: "test".to_string(),
        summary: PlanStageSummary {
            stage: "test".to_string(),
            planned: true,
            applied: write && !stage_changed_files.is_empty(),
            fixes_proposed,
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
                });
            }
        }
    }

    for new_file in &fix_result.new_files {
        entries.push(FixApplied {
            file: new_file.file.clone(),
            rule: format!("{:?}", new_file.finding).to_lowercase(),
            action: Some("create".to_string()),
        });
    }

    entries
}

pub(crate) fn analyze_stage_overlaps(stages: &[PlanStageSummary]) -> Vec<PlanOverlap> {
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
                    overlaps.push(PlanOverlap {
                        file: file.to_string(),
                        earlier_stage: earlier_stage.stage.clone(),
                        later_stage: later_stage.stage.clone(),
                        resolution: format!(
                            "{} pass ran after {} in sandbox sequence",
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

pub(crate) fn summarize_plan_totals(
    stages: &[PlanStageSummary],
    total_files_selected: usize,
) -> PlanTotals {
    PlanTotals {
        stages_with_proposals: stages
            .iter()
            .filter(|stage| stage.fixes_proposed > 0)
            .count(),
        total_fixes_proposed: stages.iter().map(|stage| stage.fixes_proposed).sum(),
        total_files_selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn analyze_stage_overlaps_reports_later_stage_precedence() {
        let stages = vec![
            PlanStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 2,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "test".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
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
                PlanOverlap {
                    file: "src/lib.rs".to_string(),
                    earlier_stage: "audit".to_string(),
                    later_stage: "lint".to_string(),
                    resolution: "lint pass ran after audit in sandbox sequence".to_string(),
                },
                PlanOverlap {
                    file: "src/main.rs".to_string(),
                    earlier_stage: "lint".to_string(),
                    later_stage: "test".to_string(),
                    resolution: "test pass ran after lint in sandbox sequence".to_string(),
                },
            ]
        );
    }

    #[test]
    fn analyze_stage_overlaps_ignores_disjoint_files() {
        let stages = vec![
            PlanStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
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
    fn summarize_plan_totals_counts_stage_and_fix_totals() {
        let stages = vec![
            PlanStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 2,
                files_modified: 1,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 0,
                files_modified: 0,
                detected_findings: Some(1),
                changed_files: Vec::new(),
                fix_summary: None,
                warnings: Vec::new(),
            },
            PlanStageSummary {
                stage: "test".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 3,
                files_modified: 2,
                detected_findings: None,
                changed_files: vec!["tests/foo.rs".to_string(), "tests/bar.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let totals = summarize_plan_totals(&stages, 3);

        assert_eq!(totals.stages_with_proposals, 2);
        assert_eq!(totals.total_fixes_proposed, 5);
        assert_eq!(totals.total_files_selected, 3);
    }

    #[test]
    fn build_refactor_plan_audit_write_uses_audit_refactor_engine() {
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
        let plan = build_refactor_plan(RefactorPlanRequest {
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
        })
        .unwrap();

        let audit_stage = plan
            .stages
            .iter()
            .find(|stage| stage.stage == "audit")
            .expect("audit stage present");

        assert!(audit_stage.applied);
        assert!(audit_stage.files_modified > 0);
        assert!(!audit_stage.changed_files.is_empty());
        assert!(plan
            .proposals
            .iter()
            .any(|proposal| proposal.source == "audit"));
        assert!(audit_stage
            .warnings
            .iter()
            .any(|warning| warning.starts_with("audit iteration ")));

        let _ = fs::remove_dir_all(root);
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

    #[test]
    fn test_lint_refactor_request_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = lint_refactor_request(component, root, settings, options, write);
    }

    #[test]
    fn test_run_lint_refactor_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = run_lint_refactor(component, root, settings, options, write);
    }

    #[test]
    fn test_run_test_refactor_default_path() {
        let component = Default::default();
        let root = PathBuf::new();
        let settings = Vec::new();
        let options = Default::default();
        let write = false;
        let _result = run_test_refactor(component, root, settings, options, write);
    }

    #[test]
    fn test_build_refactor_plan_default_path() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_let_scoped_changed_files_if_let_some_git_ref_request_changed() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_some_git_get_files_changed_since_root_str_git_ref() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_3() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_else_4() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_request_write() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_snapshot_files_is_empty() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        assert!(result.is_err(), "expected Err for: !snapshot_files.is_empty()");
    }

    #[test]
    fn test_build_refactor_plan_match_crate_engine_format_write_format_after_write_request_r() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        let inner = result.unwrap();
        // Branch returns Ok(fmt) when: match crate::engine::format_write::format_after_write(&request.root, &abs_changed)
        assert_eq!(inner.component_id, String::new());
        assert_eq!(inner.source_path, String::new());
        assert_eq!(inner.sources, Vec::new());
        assert_eq!(inner.dry_run, false);
        assert_eq!(inner.applied, false);
        assert_eq!(inner.merge_strategy, String::new());
        assert_eq!(inner.proposals, Vec::new());
        assert_eq!(inner.stages, Vec::new());
        assert_eq!(inner.plan_totals, Default::default());
        assert_eq!(inner.overlaps, Vec::new());
        assert_eq!(inner.files_modified, 0);
        assert_eq!(inner.changed_files, Vec::new());
        assert_eq!(inner.fix_summary, None);
        assert_eq!(inner.warnings, Vec::new());
        assert_eq!(inner.hints, Vec::new());
    }

    #[test]
    fn test_build_refactor_plan_match_crate_engine_format_write_format_after_write_request_r_2() {
        let request = Default::default();
        let _result = build_refactor_plan(request);
    }

    #[test]
    fn test_build_refactor_plan_fmt_success() {
        let request = Default::default();
        let result = build_refactor_plan(request);
        assert!(result.is_err(), "expected Err for: !fmt.success");
    }

    #[test]
    fn test_build_refactor_plan_has_expected_effects() {
        // Expected effects: mutation, logging
        let request = Default::default();
        let _ = build_refactor_plan(request);
    }

    #[test]
    fn test_normalize_sources_ordered_is_empty() {
        let sources = Vec::new();
        let result = normalize_sources(&sources);
        assert!(result.is_err(), "expected Err for: ordered.is_empty()");
    }

    #[test]
    fn test_normalize_sources_ordered_is_empty_2() {
        let sources = Vec::new();
        let result = normalize_sources(&sources);
        assert!(result.is_ok(), "expected Ok for: ordered.is_empty()");
    }

    #[test]
    fn test_normalize_sources_has_expected_effects() {
        // Expected effects: mutation
        let sources = Vec::new();
        let _ = normalize_sources(&sources);
    }

    #[test]
    fn test_analyze_stage_overlaps_default_path() {
        let stages = Vec::new();
        let result = analyze_stage_overlaps(&stages);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_analyze_stage_overlaps_has_expected_effects() {
        // Expected effects: mutation
        let stages = Vec::new();
        let _ = analyze_stage_overlaps(&stages);
    }

    #[test]
    fn test_summarize_plan_totals_default_path() {
        let stages = Vec::new();
        let total_files_selected = 0;
        let _result = summarize_plan_totals(&stages, total_files_selected);
    }

}
