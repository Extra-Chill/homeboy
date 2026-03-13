mod builders;
mod convention_fixes;
mod doc_fixes;
mod duplicate_fixes;
mod orphaned_test_fixes;
mod signatures;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{DecomposeFixPlan, Fix, FixResult, SkippedFile};
use crate::core::refactor::decompose;
use std::path::Path;

use convention_fixes::apply_convention_fixes;

pub(crate) use builders::{insertion, new_file};
pub(crate) use doc_fixes::is_actionable_comment_finding;
pub(crate) use duplicate_fixes::{
    generate_duplicate_function_fixes, generate_unreferenced_export_fixes,
};
pub(crate) use signatures::{
    extract_signatures, extract_signatures_from_items, find_parsed_item_by_name,
    generate_fallback_signature, generate_method_stub, parse_items_for_dedup,
    primary_type_name_from_declaration,
};

pub fn generate_audit_fixes(result: &CodeAuditResult, root: &Path) -> FixResult {
    generate_fixes_impl(result, root)
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

pub(crate) fn generate_fixes_impl(result: &CodeAuditResult, root: &Path) -> FixResult {
    let mut fixes = Vec::new();
    let mut skipped = Vec::new();
    apply_convention_fixes(result, root, &mut fixes, &mut skipped);

    let mut new_files = Vec::new();
    generate_unreferenced_export_fixes(result, root, &mut fixes, &mut skipped);
    generate_duplicate_function_fixes(result, root, &mut fixes, &mut new_files, &mut skipped);
    orphaned_test_fixes::generate_orphaned_test_fixes(result, root, &mut fixes, &mut skipped);

    let mut decompose_plans = Vec::new();
    for finding in &result.findings {
        if finding.kind != AuditFinding::GodFile {
            continue;
        }
        let is_test = crate::code_audit::walker::is_test_path(&finding.file);
        if is_test {
            continue;
        }
        match decompose::build_plan(&finding.file, root, "grouped") {
            Ok(plan) => {
                if plan.groups.len() > 1 {
                    decompose_plans.push(DecomposeFixPlan {
                        file: finding.file.clone(),
                        plan,
                        applied: false,
                    });
                }
            }
            Err(error) => {
                skipped.push(SkippedFile {
                    file: finding.file.clone(),
                    reason: format!("Decompose plan failed: {}", error),
                });
            }
        }
    }

    doc_fixes::apply_stale_doc_reference_fixes(result, &mut fixes);
    doc_fixes::apply_broken_doc_reference_fixes(result, root, &mut fixes);

    let fixes = merge_fixes_per_file(fixes);
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
