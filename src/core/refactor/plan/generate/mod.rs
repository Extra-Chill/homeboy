mod builders;
mod comment_fixes;
mod compiler_warning_fixes;
mod convention_fixes;
mod doc_fixes;
mod duplicate_fixes;
mod intra_duplicate_fixes;
mod module_surface;
mod near_duplicate_fixes;
mod orphaned_test_fixes;
mod parameter_fixes;
mod signatures;
mod test_gen_fixes;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{DecomposeFixPlan, Fix, FixPolicy, FixResult, SkippedFile};
use crate::core::refactor::decompose;
use crate::core::refactor::plan::file_intent::{FileIntent, FileIntentMap};
use std::path::Path;

use convention_fixes::apply_convention_fixes;

pub(crate) use builders::{insertion, new_file};
pub(crate) use doc_fixes::is_actionable_comment_finding;
pub(crate) use duplicate_fixes::{
    generate_duplicate_function_fixes, generate_unreferenced_export_fixes,
};
pub(crate) use module_surface::{FileRole, ModuleSurfaceIndex};
pub(crate) use signatures::{
    extract_signatures, extract_signatures_from_items, find_parsed_item_by_name,
    generate_fallback_signature, generate_method_stub, parse_items_for_dedup,
    primary_type_name_from_declaration,
};

pub fn generate_audit_fixes(
    result: &CodeAuditResult,
    root: &Path,
    policy: &FixPolicy,
) -> FixResult {
    generate_fixes_impl(result, root, policy)
}

pub(crate) fn merge_fixes_per_file(fixes: Vec<Fix>) -> Vec<Fix> {
    let mut map: std::collections::HashMap<String, Fix> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for fix in fixes {
        if let Some(existing) = map.get_mut(&fix.file) {
            for method in fix.required_methods {
                if !existing.required_methods.contains(&method) {
                    existing.required_methods.push(method);
                }
            }
            for registration in fix.required_registrations {
                if !existing.required_registrations.contains(&registration) {
                    existing.required_registrations.push(registration);
                }
            }
            existing.insertions.extend(fix.insertions);
        } else {
            order.push(fix.file.clone());
            map.insert(fix.file.clone(), fix);
        }
    }

    order
        .into_iter()
        .filter_map(|file| map.remove(&file))
        .collect()
}

pub(crate) fn generate_fixes_impl(
    result: &CodeAuditResult,
    root: &Path,
    policy: &FixPolicy,
) -> FixResult {
    let mut fixes = Vec::new();
    let mut skipped = Vec::new();
    let module_surfaces = ModuleSurfaceIndex::build(root);
    let finding_enabled = |finding: &AuditFinding| {
        policy
            .only
            .as_ref()
            .is_none_or(|only| only.contains(finding))
            && !policy.exclude.contains(finding)
    };

    // ── Phase 0: Build file intent map ─────────────────────────────────
    // Identify structural operations (decompose, move, delete) planned for
    // each file BEFORE generating content fixes. After all fixes are
    // generated, resolve_conflicts() drops content fixes that would
    // conflict with structural intents — replacing ad-hoc skip sets with
    // declarative conflict rules.
    let mut intent_map = FileIntentMap::new();
    for finding in &result.findings {
        if !finding_enabled(&finding.kind) {
            continue;
        }
        if matches!(
            finding.kind,
            AuditFinding::GodFile | AuditFinding::HighItemCount
        ) && !crate::code_audit::walker::is_test_path(&finding.file)
        {
            intent_map.set(finding.file.clone(), FileIntent::Decompose);
        }
    }

    // ── Phase 1: Generate all content fixes ────────────────────────────
    // Fixers run freely against the audit data. Conflicts with structural
    // intents are resolved after generation, not during.
    if policy.only.is_none() && policy.exclude.is_empty() {
        apply_convention_fixes(result, root, &mut fixes, &mut skipped);
    }

    let mut new_files = Vec::new();
    if finding_enabled(&AuditFinding::UnreferencedExport) {
        generate_unreferenced_export_fixes(
            result,
            root,
            &module_surfaces,
            &mut fixes,
            &mut skipped,
        );
    }
    if finding_enabled(&AuditFinding::DuplicateFunction) {
        generate_duplicate_function_fixes(
            result,
            root,
            &module_surfaces,
            &mut fixes,
            &mut new_files,
            &mut skipped,
        );
    }
    if finding_enabled(&AuditFinding::OrphanedTest) {
        orphaned_test_fixes::generate_orphaned_test_fixes(result, root, &mut fixes, &mut skipped);
    }

    // ── Phase 2: Build decompose plans ─────────────────────────────────
    let mut decompose_plans = Vec::new();
    let mut decompose_seen = std::collections::HashSet::new();
    if finding_enabled(&AuditFinding::GodFile) || finding_enabled(&AuditFinding::HighItemCount) {
        for finding in &result.findings {
            if !finding_enabled(&finding.kind) {
                continue;
            }
            if !matches!(
                finding.kind,
                AuditFinding::GodFile | AuditFinding::HighItemCount
            ) {
                continue;
            }
            if decompose_seen.contains(&finding.file) {
                continue;
            }
            let is_test = crate::code_audit::walker::is_test_path(&finding.file);
            if is_test {
                continue;
            }
            match decompose::build_plan(&finding.file, root, "grouped") {
                Ok(plan) => {
                    if plan.groups.len() > 1 {
                        decompose_seen.insert(finding.file.clone());
                        decompose_plans.push(DecomposeFixPlan {
                            file: finding.file.clone(),
                            plan,
                            source_finding: finding.kind.clone(),
                            applied: false,
                        });
                    }
                }
                Err(error) => {
                    decompose_seen.insert(finding.file.clone());
                    skipped.push(SkippedFile {
                        file: finding.file.clone(),
                        reason: format!("Decompose plan failed: {}", error),
                    });
                }
            }
        }
    }

    if finding_enabled(&AuditFinding::StaleDocReference) {
        doc_fixes::apply_stale_doc_reference_fixes(result, &mut fixes);
    }
    if finding_enabled(&AuditFinding::BrokenDocReference) {
        doc_fixes::apply_broken_doc_reference_fixes(result, root, &mut fixes);
    }
    if finding_enabled(&AuditFinding::UnusedParameter) {
        parameter_fixes::generate_parameter_fixes(result, root, &mut fixes, &mut skipped);
    }
    if finding_enabled(&AuditFinding::MissingTestFile) {
        test_gen_fixes::generate_test_file_fixes(
            result,
            root,
            &mut new_files,
            &mut fixes,
            &mut skipped,
        );
    }
    if finding_enabled(&AuditFinding::MissingTestMethod) {
        test_gen_fixes::generate_test_method_fixes(result, root, &mut fixes, &mut skipped);
    }
    if finding_enabled(&AuditFinding::CompilerWarning) {
        compiler_warning_fixes::generate_compiler_warning_fixes(
            result,
            root,
            &mut fixes,
            &mut skipped,
        );
    }
    if finding_enabled(&AuditFinding::TodoMarker) || finding_enabled(&AuditFinding::LegacyComment) {
        comment_fixes::generate_comment_fixes(result, root, &mut fixes, &mut skipped);
    }
    if finding_enabled(&AuditFinding::NearDuplicate) {
        near_duplicate_fixes::generate_near_duplicate_fixes(
            result,
            root,
            &module_surfaces,
            &mut fixes,
            &mut skipped,
        );
    }
    if finding_enabled(&AuditFinding::IntraMethodDuplicate) {
        intra_duplicate_fixes::generate_intra_duplicate_fixes(
            result,
            root,
            &mut fixes,
            &mut skipped,
        );
    }

    let mut fixes = merge_fixes_per_file(fixes);

    // ── Phase 4: Resolve cross-fixer conflicts ─────────────────────────
    // Drop content fixes that conflict with structural intents. This is
    // the central conflict resolution — no individual fixer needs to know
    // about other fixers' existence.
    let dropped = intent_map.resolve_conflicts(&mut fixes);
    if dropped > 0 {
        eprintln!(
            "FileIntent conflict resolution: dropped {} dominated insertions",
            dropped
        );
    }

    let total_insertions: usize = fixes.iter().map(|fix| fix.insertions.len()).sum();
    let files_modified = fixes.len();

    FixResult {
        fixes,
        new_files,
        decompose_plans,
        skipped,
        chunk_results: vec![],
        total_insertions,
        files_modified,
    }
}
