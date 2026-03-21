mod builders;
mod compiler_warning_fixes;
mod convention_fixes;
mod doc_fixes;
mod duplicate_fixes;
mod intra_duplicate_fixes;
mod orphaned_test_fixes;
mod parameter_fixes;
mod signatures;
mod test_gen_fixes;

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
    let mut decompose_seen = std::collections::HashSet::new();
    for finding in &result.findings {
        if !matches!(
            finding.kind,
            AuditFinding::GodFile | AuditFinding::HighItemCount
        ) {
            continue;
        }
        // A file can trigger both GodFile and HighItemCount — only plan once.
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

    doc_fixes::apply_stale_doc_reference_fixes(result, &mut fixes);
    doc_fixes::apply_broken_doc_reference_fixes(result, root, &mut fixes);
    parameter_fixes::generate_parameter_fixes(result, root, &mut fixes, &mut skipped);
    test_gen_fixes::generate_test_file_fixes(
        result,
        root,
        &mut new_files,
        &mut fixes,
        &mut skipped,
    );
    test_gen_fixes::generate_test_method_fixes(result, root, &mut fixes, &mut skipped);
    compiler_warning_fixes::generate_compiler_warning_fixes(result, root, &mut fixes, &mut skipped);
    intra_duplicate_fixes::generate_intra_duplicate_fixes(result, root, &mut fixes, &mut skipped);

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
