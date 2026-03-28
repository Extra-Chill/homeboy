use super::outcome::{FixApplied, FixResultsSummary, PrimitiveFixCount, RuleFixCount};
use crate::refactor::FixResult;

pub fn summarize_fix_results(fixes: &[FixApplied]) -> FixResultsSummary {
    use std::collections::{BTreeMap, HashSet};

    let mut files = HashSet::new();
    let mut rule_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut primitive_counts: BTreeMap<String, usize> = BTreeMap::new();

    for fix in fixes {
        files.insert(fix.file.clone());
        *rule_counts.entry(fix.rule.clone()).or_insert(0) += 1;
        if let Some(primitive) = &fix.primitive {
            *primitive_counts.entry(primitive.clone()).or_insert(0) += 1;
        }
    }

    let rules = rule_counts
        .into_iter()
        .map(|(rule, count)| RuleFixCount { rule, count })
        .collect();

    let primitives = primitive_counts
        .into_iter()
        .map(|(primitive, count)| PrimitiveFixCount { primitive, count })
        .collect();

    FixResultsSummary {
        fixes_applied: fixes.len(),
        files_modified: files.len(),
        rules,
        primitives,
    }
}

pub fn summarize_optional_fix_results(fixes: &[FixApplied]) -> Option<FixResultsSummary> {
    if fixes.is_empty() {
        None
    } else {
        Some(summarize_fix_results(fixes))
    }
}

pub fn summarize_audit_fix_result(fix_result: &FixResult) -> FixResultsSummary {
    use std::collections::{BTreeMap, HashSet};

    let mut files = HashSet::new();
    let mut rule_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut primitive_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_fixes = 0usize;

    for fix in &fix_result.fixes {
        if !fix.applied {
            continue;
        }
        files.insert(fix.file.clone());
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                let rule = format!("{:?}", insertion.finding).to_lowercase();
                *rule_counts.entry(rule).or_insert(0) += 1;
                if let Some(primitive) = insertion.primitive.as_ref().map(primitive_name) {
                    *primitive_counts.entry(primitive).or_insert(0) += 1;
                }
                total_fixes += 1;
            }
        }
    }

    for new_file in &fix_result.new_files {
        if new_file.written {
            files.insert(new_file.file.clone());
            let rule = format!("{:?}", new_file.finding).to_lowercase();
            *rule_counts.entry(rule).or_insert(0) += 1;
            if let Some(primitive) = new_file.primitive.as_ref().map(primitive_name) {
                *primitive_counts.entry(primitive).or_insert(0) += 1;
            }
            total_fixes += 1;
        }
    }

    for plan in &fix_result.decompose_plans {
        if plan.applied {
            files.insert(plan.file.clone());
            let rule = format!("{:?}", plan.source_finding).to_lowercase();
            *rule_counts.entry(rule).or_insert(0) += 1;
            total_fixes += 1;
        }
    }

    let rules = rule_counts
        .into_iter()
        .map(|(rule, count)| RuleFixCount { rule, count })
        .collect();

    let primitives = primitive_counts
        .into_iter()
        .map(|(primitive, count)| PrimitiveFixCount { primitive, count })
        .collect();

    FixResultsSummary {
        fixes_applied: total_fixes,
        files_modified: files.len(),
        rules,
        primitives,
    }
}

pub fn primitive_name(primitive: &crate::refactor::RefactorPrimitive) -> String {
    match primitive {
        crate::refactor::RefactorPrimitive::MoveTestFile => "move_test_file".to_string(),
        crate::refactor::RefactorPrimitive::RenameTestMethod => "rename_test_method".to_string(),
        crate::refactor::RefactorPrimitive::RemoveOrphanedTest => {
            "remove_orphaned_test".to_string()
        }
        crate::refactor::RefactorPrimitive::RemoveCompilerDeadCode => {
            "remove_compiler_dead_code".to_string()
        }
        crate::refactor::RefactorPrimitive::ApplyCompilerReplacement => {
            "apply_compiler_replacement".to_string()
        }
        crate::refactor::RefactorPrimitive::RemoveUnusedParameter => {
            "remove_unused_parameter".to_string()
        }
        crate::refactor::RefactorPrimitive::RemoveNearDuplicateImplementation => {
            "remove_near_duplicate_implementation".to_string()
        }
        crate::refactor::RefactorPrimitive::ImportCanonicalImplementation => {
            "import_canonical_implementation".to_string()
        }
        crate::refactor::RefactorPrimitive::WidenCanonicalVisibility => {
            "widen_canonical_visibility".to_string()
        }
        crate::refactor::RefactorPrimitive::UpdateStaleDocReference => {
            "update_stale_doc_reference".to_string()
        }
        crate::refactor::RefactorPrimitive::RemoveBrokenDocReferenceLine => {
            "remove_broken_doc_reference_line".to_string()
        }
    }
}
