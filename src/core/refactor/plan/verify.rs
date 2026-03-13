use crate::code_audit::{self, CodeAuditResult};
use crate::component::{self, Component};
use crate::engine::temp;
use crate::extension::test::compute_changed_test_files;
use crate::extension::{lint as extension_lint, test as extension_test};
use crate::refactor::auto as fixer;
use crate::undo::UndoSnapshot;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Copy)]
pub struct AuditVerificationToggles {
    pub lint_smoke: bool,
    pub test_smoke: bool,
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
    verification: AuditVerificationToggles,
    max_iterations: usize,
    write: bool,
) -> crate::Result<AuditRefactorOutcome> {
    let mut current_result = initial_result;
    let mut iterations = Vec::new();
    let mut seen_fingerprints = HashSet::new();
    let mut final_fix_result = fixer::FixResult {
        fixes: vec![],
        new_files: vec![],
        decompose_plans: vec![],
        skipped: vec![],
        chunk_results: vec![],
        total_insertions: 0,
        files_modified: 0,
    };
    let mut final_policy_summary = fixer::PolicySummary::default();

    if write {
        for iteration_index in 0..max_iterations.max(1) {
            let before_fingerprint = findings_fingerprint(&current_result);
            if !seen_fingerprints.insert(before_fingerprint) {
                iterations.push(AuditRefactorIterationSummary {
                    iteration: iteration_index + 1,
                    findings_before: current_result.findings.len(),
                    findings_after: current_result.findings.len(),
                    weighted_score_before: weighted_finding_score_with(&current_result, scoring),
                    weighted_score_after: weighted_finding_score_with(&current_result, scoring),
                    score_delta: 0,
                    applied_chunks: 0,
                    reverted_chunks: 0,
                    changed_files: vec![],
                    status: "stopped_cycle_detected".to_string(),
                });
                break;
            }

            let (fix_result, policy_summary, mut iteration_summary) = run_fix_iteration(
                &current_result,
                only_kinds,
                exclude_kinds,
                scoring,
                verification,
            )?;

            let changed_files = iteration_summary.changed_files.clone();
            final_fix_result = fix_result.clone();
            final_policy_summary = policy_summary;

            if changed_files.is_empty() {
                iteration_summary.iteration = iteration_index + 1;
                iteration_summary.findings_after = current_result.findings.len();
                iteration_summary.weighted_score_after =
                    weighted_finding_score_with(&current_result, scoring);
                iteration_summary.score_delta =
                    score_delta(&current_result, &current_result, scoring);
                iteration_summary.status = "stopped_no_safe_changes".to_string();
                iterations.push(iteration_summary);
                break;
            }

            let next_result = code_audit::audit_path_with_id(
                &current_result.component_id,
                &current_result.source_path,
            )?;

            iteration_summary.iteration = iteration_index + 1;
            iteration_summary.findings_after = next_result.findings.len();
            iteration_summary.weighted_score_after =
                weighted_finding_score_with(&next_result, scoring);
            iteration_summary.score_delta = score_delta(&current_result, &next_result, scoring);
            iteration_summary.status = if next_result.findings.is_empty() {
                "stopped_clean".to_string()
            } else if iteration_summary.score_delta <= 0 {
                "stopped_no_progress".to_string()
            } else {
                "continued".to_string()
            };
            let should_stop = next_result.findings.is_empty() || iteration_summary.score_delta <= 0;
            iterations.push(iteration_summary);

            if should_stop {
                current_result = next_result;
                break;
            }

            current_result = next_result;
        }
    } else {
        let root = Path::new(&current_result.source_path);
        let mut fix_result = super::generate::generate_audit_fixes(&current_result, root);
        let policy = fixer::FixPolicy {
            only: (!only_kinds.is_empty()).then_some(only_kinds.to_vec()),
            exclude: exclude_kinds.to_vec(),
        };
        let preflight_context = fixer::PreflightContext { root };
        final_policy_summary =
            fixer::apply_fix_policy(&mut fix_result, false, &policy, &preflight_context);
        final_fix_result = fix_result;
    }

    Ok(AuditRefactorOutcome {
        current_result,
        fix_result: final_fix_result,
        policy_summary: final_policy_summary,
        iterations,
    })
}

fn findings_fingerprint(result: &CodeAuditResult) -> Vec<String> {
    let mut fingerprints: Vec<String> = result.findings.iter().map(finding_fingerprint).collect();
    fingerprints.sort();
    fingerprints
}

fn load_or_discover(component_id: &str, source_path: &str) -> Option<Component> {
    component::load(component_id).ok().or_else(|| {
        let mut comp = component::discover_from_portable(Path::new(source_path))?;
        comp.id = component_id.to_string();
        comp.local_path = source_path.to_string();
        Some(comp)
    })
}

fn build_smoke_verifier<'a>(
    component_id: &'a str,
    source_path: &'a str,
    changed_files: &'a [String],
) -> Option<impl Fn(&fixer::ApplyChunkResult) -> Result<String, String> + 'a> {
    let component = load_or_discover(component_id, source_path)?;
    extension_lint::resolve_lint_command(&component).ok()?;
    let root = PathBuf::from(source_path);
    Some(move |chunk: &fixer::ApplyChunkResult| {
        if changed_files.is_empty() {
            return Ok("lint_smoke_skipped_no_files".to_string());
        }

        if chunk.files.is_empty() {
            return Ok("lint_smoke_skipped_no_chunk_files".to_string());
        }

        let target_files: Vec<String> = changed_files
            .iter()
            .filter(|file| chunk.files.contains(file))
            .cloned()
            .collect();

        if target_files.is_empty() {
            return Ok("lint_smoke_skipped_no_overlap".to_string());
        }

        let glob = if target_files.len() == 1 {
            root.join(&target_files[0]).to_string_lossy().to_string()
        } else {
            let joined = target_files
                .iter()
                .map(|file| root.join(file).to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{}}}", joined)
        };

        let output = extension_lint::build_lint_runner(
            &component,
            Some(source_path.to_string()),
            &[],
            false,
            None,
            Some(&glob),
            false,
            None,
            None,
            None,
            "/dev/null",
        )
        .and_then(|runner| runner.run())
        .map_err(|error| format!("lint smoke run failed: {}", error))?;

        if output.success {
            Ok("lint_smoke_passed".to_string())
        } else {
            Err("lint smoke failed".to_string())
        }
    })
}

fn build_test_smoke_verifier<'a>(
    component_id: &'a str,
    source_path: &'a str,
    changed_files: &'a [String],
) -> Option<impl Fn(&fixer::ApplyChunkResult) -> Result<String, String> + 'a> {
    let component = load_or_discover(component_id, source_path)?;
    extension_test::resolve_test_command(&component).ok()?;
    let changed_test_files = compute_changed_test_files(&component, "HEAD~1")
        .ok()
        .and_then(|files| (!files.is_empty()).then_some(files.join("\n")));

    Some(move |chunk: &fixer::ApplyChunkResult| {
        if chunk.files.is_empty() || changed_files.is_empty() {
            return Ok("test_smoke_skipped_no_files".to_string());
        }

        let overlapping_files: Vec<String> = changed_files
            .iter()
            .filter(|file| chunk.files.contains(file))
            .cloned()
            .collect();

        if overlapping_files.is_empty() {
            return Ok("test_smoke_skipped_no_overlap".to_string());
        }

        let results_file = temp::runtime_temp_file("homeboy-audit-test-smoke", ".json")
            .map_err(|error| format!("create test smoke temp file failed: {}", error))?;
        let results_file_str = results_file.to_string_lossy().to_string();
        let selected_test_files = changed_test_files.as_ref().map(|files| {
            files
                .split('\n')
                .filter(|file| !file.is_empty())
                .map(|file| file.to_string())
                .collect::<Vec<_>>()
        });

        let output = extension_test::build_test_runner(
            &component,
            Some(source_path.to_string()),
            &[],
            true,
            false,
            &results_file_str,
            None,
            None,
            None,
            selected_test_files.as_deref(),
        )
        .and_then(|runner| runner.run())
        .map_err(|error| format!("test smoke run failed: {}", error))?;

        let _ = std::fs::remove_file(&results_file);

        if output.success {
            Ok("test_smoke_passed".to_string())
        } else {
            Err("test smoke failed".to_string())
        }
    })
}

fn run_fix_iteration(
    audit_result: &CodeAuditResult,
    only_kinds: &[crate::code_audit::AuditFinding],
    exclude_kinds: &[crate::code_audit::AuditFinding],
    scoring: AuditConvergenceScoring,
    verification: AuditVerificationToggles,
) -> crate::Result<(
    fixer::FixResult,
    fixer::PolicySummary,
    AuditRefactorIterationSummary,
)> {
    let root = Path::new(&audit_result.source_path);
    let mut fix_result = super::generate::generate_audit_fixes(audit_result, root);
    let policy = fixer::FixPolicy {
        only: (!only_kinds.is_empty()).then_some(only_kinds.to_vec()),
        exclude: exclude_kinds.to_vec(),
    };
    let preflight_context = fixer::PreflightContext { root };
    let policy_summary =
        fixer::apply_fix_policy(&mut fix_result, true, &policy, &preflight_context);

    let mut applied_chunks = 0;
    let mut reverted_chunks = 0;
    let mut total_modified = 0;
    let mut auto_apply_result = fixer::auto_apply_subset(&fix_result);
    let changed_files: Vec<String> = auto_apply_result
        .fixes
        .iter()
        .map(|fix| fix.file.clone())
        .chain(
            auto_apply_result
                .new_files
                .iter()
                .map(|file| file.file.clone()),
        )
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

    let smoke_verifier = build_smoke_verifier(
        &audit_result.component_id,
        &audit_result.source_path,
        &changed_files,
    )
    .filter(|_| verification.lint_smoke);
    let test_smoke_verifier = build_test_smoke_verifier(
        &audit_result.component_id,
        &audit_result.source_path,
        &changed_files,
    )
    .filter(|_| verification.test_smoke);
    let mut extra_smokes: Vec<fixer::ChunkVerifier> = Vec::new();
    if let Some(verifier) = smoke_verifier.as_ref() {
        extra_smokes.push(verifier);
    }
    if let Some(verifier) = test_smoke_verifier.as_ref() {
        extra_smokes.push(verifier);
    }
    let verifier = build_chunk_verifier(root, &audit_result.findings, extra_smokes);

    if !auto_apply_result.fixes.is_empty() {
        let chunk_results = fixer::apply_fixes_chunked(
            &mut auto_apply_result.fixes,
            root,
            fixer::ApplyOptions {
                verifier: Some(&verifier),
            },
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

    if !auto_apply_result.new_files.is_empty() {
        let chunk_results = fixer::apply_new_files_chunked(
            &mut auto_apply_result.new_files,
            root,
            fixer::ApplyOptions {
                verifier: Some(&verifier),
            },
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

    if !auto_apply_result.decompose_plans.is_empty() {
        let decompose_chunk_results = fixer::apply_decompose_plans(
            &mut auto_apply_result.decompose_plans,
            root,
            fixer::ApplyOptions {
                verifier: Some(&verifier),
            },
        );
        fix_result.chunk_results.extend(decompose_chunk_results);
    }

    for applied_fix in auto_apply_result.fixes {
        if let Some(original) = fix_result
            .fixes
            .iter_mut()
            .find(|candidate| candidate.file == applied_fix.file)
        {
            original.applied = applied_fix.applied;
        }
    }

    for written_file in auto_apply_result.new_files {
        if let Some(original) = fix_result
            .new_files
            .iter_mut()
            .find(|candidate| candidate.file == written_file.file)
        {
            original.written = written_file.written;
        }
    }

    for plan in &auto_apply_result.decompose_plans {
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

fn is_cascading_finding_kind(kind: &crate::code_audit::AuditFinding) -> bool {
    use crate::code_audit::AuditFinding;

    matches!(
        kind,
        AuditFinding::GodFile
            | AuditFinding::HighItemCount
            | AuditFinding::DirectorySprawl
            | AuditFinding::MissingTestFile
            | AuditFinding::MissingTestMethod
    )
}

pub fn build_chunk_verifier<'a>(
    root: &'a Path,
    baseline_findings: &'a [crate::code_audit::Finding],
    extra_smokes: Vec<fixer::ChunkVerifier<'a>>,
) -> impl Fn(&fixer::ApplyChunkResult) -> Result<String, String> + 'a {
    move |chunk| {
        let changed_files = chunk.files.clone();
        if changed_files.is_empty() {
            return Ok("no_files".to_string());
        }

        let baseline: HashSet<String> = baseline_findings
            .iter()
            .filter(|finding| changed_files.contains(&finding.file))
            .map(finding_fingerprint)
            .collect();

        let audit_result = code_audit::audit_path_scoped(
            "audit-fix-verify",
            &root.to_string_lossy(),
            &changed_files,
            None,
        )
        .map_err(|error| format!("verification audit failed: {}", error))?;

        let new_findings: Vec<&crate::code_audit::Finding> = audit_result
            .findings
            .iter()
            .filter(|finding| changed_files.contains(&finding.file))
            .filter(|finding| !baseline.contains(&finding_fingerprint(finding)))
            .collect();

        let hard_failures: Vec<String> = new_findings
            .iter()
            .filter(|finding| !is_cascading_finding_kind(&finding.kind))
            .map(|finding| format!("{}: {:?}", finding.file, finding.kind))
            .collect();
        let cascading_count = new_findings.len() - hard_failures.len();

        if !hard_failures.is_empty() {
            Err(format!(
                "scoped re-audit introduced new findings in changed files: {}",
                hard_failures.join(", ")
            ))
        } else if cascading_count > 0 {
            let mut verification = format!(
                "scoped_reaudit_ok_with_{}_cascading_findings",
                cascading_count
            );
            for smoke in &extra_smokes {
                let smoke_result = smoke(chunk)?;
                verification.push('+');
                verification.push_str(&smoke_result);
            }
            Ok(verification)
        } else if extra_smokes.is_empty() {
            Ok("scoped_reaudit_no_new_findings".to_string())
        } else {
            let mut verification = "scoped_reaudit_no_new_findings".to_string();
            for smoke in &extra_smokes {
                let smoke_result = smoke(chunk)?;
                verification.push('+');
                verification.push_str(&smoke_result);
            }
            Ok(verification)
        }
    }
}
