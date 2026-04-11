use crate::code_audit::CodeAuditResult;
use crate::engine::undo::UndoSnapshot;
use crate::refactor::auto as fixer;
use serde::Serialize;
use std::path::Path;

pub use crate::code_audit::{
    finding_fingerprint, score_delta, weighted_finding_score_with, AuditConvergenceScoring,
};

pub(crate) fn rewrite_callers_after_dedup(fix: &fixer::Fix, root: &Path) {
    use crate::core::engine::symbol_graph;

    for insertion in &fix.insertions {
        if !matches!(insertion.kind, fixer::InsertionKind::FunctionRemoval { .. }) {
            continue;
        }
        if insertion.finding != crate::code_audit::AuditFinding::DuplicateFunction {
            continue;
        }

        let Some(fn_name) = insertion.description.split('`').nth(1) else {
            continue;
        };
        let Some(canonical_file) = insertion
            .description
            .split("canonical copy in ")
            .nth(1)
            .map(|value| value.trim_end_matches(')'))
        else {
            continue;
        };

        let old_module = symbol_graph::module_path_from_file(&fix.file);
        let new_module = symbol_graph::module_path_from_file(canonical_file);

        if old_module == new_module {
            continue;
        }

        let ext = Path::new(&fix.file)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("rs");

        let result =
            symbol_graph::rewrite_imports(fn_name, &old_module, &new_module, root, &[ext], true);

        if !result.rewrites.is_empty() {
            log_status!(
                "fix",
                "Rewrote {} caller import(s) for `{}`: {} → {}",
                result.rewrites.len(),
                fn_name,
                old_module,
                new_module
            );
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRefactorIterationSummary {
    pub iteration: usize,
    pub findings_before: usize,
    pub findings_after: usize,
    pub weighted_score_before: usize,
    pub weighted_score_after: usize,
    pub score_delta: isize,
    pub applied_chunks: usize,
    pub reverted_chunks: usize,
    pub changed_files: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct AuditRefactorOutcome {
    pub current_result: CodeAuditResult,
    pub fix_result: fixer::FixResult,
    pub policy_summary: fixer::PolicySummary,
    pub iterations: Vec<AuditRefactorIterationSummary>,
}

pub fn run_audit_refactor(
    initial_result: CodeAuditResult,
    only_kinds: &[crate::code_audit::AuditFinding],
    exclude_kinds: &[crate::code_audit::AuditFinding],
    scoring: AuditConvergenceScoring,
    _max_iterations: usize,
    write: bool,
) -> crate::Result<AuditRefactorOutcome> {
    let current_result = initial_result;
    let mut iterations = Vec::new();
    let final_fix_result;
    let final_policy_summary;

    if write {
        // Single pass: take the provided findings, generate fixes, apply, validate.
        // The refactor command receives findings from the audit that already ran —
        // it does not re-run the audit internally. The convergence loop
        // (audit → fix → PR → merge → re-audit) belongs in the orchestration
        // pipeline, not inside a single refactor invocation.
        let (fix_result, policy_summary, mut iteration_summary) =
            run_fix_iteration(&current_result, only_kinds, exclude_kinds, scoring)?;

        let changed_files = iteration_summary.changed_files.clone();
        final_fix_result = fix_result;
        final_policy_summary = policy_summary;

        iteration_summary.iteration = 1;
        iteration_summary.findings_after = current_result.findings.len();
        iteration_summary.weighted_score_after =
            weighted_finding_score_with(&current_result, scoring);
        iteration_summary.score_delta = 0;

        if changed_files.is_empty() {
            iteration_summary.status = "no_automated_changes".to_string();
        } else {
            iteration_summary.status = "completed".to_string();
        }

        iterations.push(iteration_summary);
    } else {
        let policy = fixer::FixPolicy {
            only: (!only_kinds.is_empty()).then_some(only_kinds.to_vec()),
            exclude: exclude_kinds.to_vec(),
        };
        let root = Path::new(&current_result.source_path);
        let mut fix_result = super::generate::generate_audit_fixes(&current_result, root, &policy);
        final_policy_summary = fixer::apply_fix_policy(&mut fix_result, false, &policy);
        final_fix_result = fix_result;
    }

    Ok(AuditRefactorOutcome {
        current_result,
        fix_result: final_fix_result,
        policy_summary: final_policy_summary,
        iterations,
    })
}

fn run_fix_iteration(
    audit_result: &CodeAuditResult,
    only_kinds: &[crate::code_audit::AuditFinding],
    exclude_kinds: &[crate::code_audit::AuditFinding],
    scoring: AuditConvergenceScoring,
) -> crate::Result<(
    fixer::FixResult,
    fixer::PolicySummary,
    AuditRefactorIterationSummary,
)> {
    let policy = fixer::FixPolicy {
        only: (!only_kinds.is_empty()).then_some(only_kinds.to_vec()),
        exclude: exclude_kinds.to_vec(),
    };
    let root = Path::new(&audit_result.source_path);
    let mut fix_result = super::generate::generate_audit_fixes(audit_result, root, &policy);
    let policy_summary = fixer::apply_fix_policy(&mut fix_result, true, &policy);

    let mut applied_chunks = 0;
    let mut reverted_chunks = 0;
    let mut total_modified = 0;

    // Filter to auto-apply eligible fixes and new files (inlined from removed auto_apply_subset)
    let mut auto_fixes: Vec<fixer::Fix> = fix_result
        .fixes
        .iter()
        .filter_map(|fix| {
            let insertions: Vec<fixer::Insertion> = fix
                .insertions
                .iter()
                .filter(|ins| ins.auto_apply)
                .cloned()
                .collect();
            if insertions.is_empty() {
                None
            } else {
                Some(fixer::Fix {
                    file: fix.file.clone(),
                    required_methods: fix.required_methods.clone(),
                    required_registrations: fix.required_registrations.clone(),
                    insertions,
                    applied: false,
                })
            }
        })
        .collect();
    let mut auto_new_files: Vec<fixer::NewFile> = fix_result
        .new_files
        .iter()
        .filter(|nf| nf.auto_apply)
        .cloned()
        .collect();
    let mut auto_decompose_plans = fix_result.decompose_plans.clone();
    let changed_files: Vec<String> = auto_fixes
        .iter()
        .map(|fix| fix.file.clone())
        .chain(auto_new_files.iter().map(|file| file.file.clone()))
        .collect();

    if !changed_files.is_empty() {
        let mut snap = UndoSnapshot::new(root, "audit fix");
        for file in &changed_files {
            snap.capture_file(file);
        }
        if let Err(error) = snap.save() {
            crate::log_status!("undo", "Warning: failed to save undo snapshot: {}", error);
        }
    }

    // Apply content fixes, new files, and file moves through the unified
    // EditOp pipeline. This converts Fix/NewFile → TaggedEditOp, applies them
    // via apply_edit_ops(), then runs format_after_write and caller rewriting.
    if !auto_fixes.is_empty() || !auto_new_files.is_empty() {
        let chunk_results = fixer::apply_fixes_via_edit_ops(
            &mut auto_fixes,
            &mut auto_new_files,
            root,
        );
        applied_chunks += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .count();
        reverted_chunks += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Reverted))
            .count();
        total_modified += chunk_results
            .iter()
            .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
            .map(|chunk| chunk.applied_files)
            .sum::<usize>();
        fix_result.chunk_results.extend(chunk_results);
    }

    // Decompose plans use their own apply path (out of scope for EditOp migration)
    if !auto_decompose_plans.is_empty() {
        let decompose_chunk_results = fixer::apply_decompose_plans(
            &mut auto_decompose_plans,
            root,
            fixer::ApplyOptions { verifier: None },
        );
        fix_result.chunk_results.extend(decompose_chunk_results);
    }

    for applied_fix in auto_fixes {
        if let Some(original) = fix_result
            .fixes
            .iter_mut()
            .find(|candidate| candidate.file == applied_fix.file)
        {
            original.applied = applied_fix.applied;
        }
    }

    for written_file in auto_new_files {
        if let Some(original) = fix_result
            .new_files
            .iter_mut()
            .find(|candidate| candidate.file == written_file.file)
        {
            original.written = written_file.written;
        }
    }

    for plan in &auto_decompose_plans {
        if let Some(original) = fix_result
            .decompose_plans
            .iter_mut()
            .find(|candidate| candidate.file == plan.file)
        {
            original.applied = plan.applied;
        }
    }

    fix_result.files_modified = total_modified;

    let changed_files: Vec<String> = fix_result
        .chunk_results
        .iter()
        .filter(|chunk| matches!(chunk.status, fixer::ChunkStatus::Applied))
        .flat_map(|chunk| chunk.files.clone())
        .collect();

    Ok((
        fix_result,
        policy_summary,
        AuditRefactorIterationSummary {
            iteration: 0,
            findings_before: audit_result.findings.len(),
            findings_after: 0,
            weighted_score_before: weighted_finding_score_with(audit_result, scoring),
            weighted_score_after: 0,
            score_delta: 0,
            applied_chunks,
            reverted_chunks,
            changed_files,
            status: String::new(),
        },
    ))
}
