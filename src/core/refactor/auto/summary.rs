use super::outcome::{FixApplied, FixResultsSummary, RuleFixCount};

pub fn summarize_fix_results(fixes: &[FixApplied]) -> FixResultsSummary {
    use std::collections::{BTreeMap, HashSet};

    let mut files = HashSet::new();
    let mut rule_counts: BTreeMap<String, usize> = BTreeMap::new();

    for fix in fixes {
        files.insert(fix.file.clone());
        *rule_counts.entry(fix.rule.clone()).or_insert(0) += 1;
    }

    let rules = rule_counts
        .into_iter()
        .map(|(rule, count)| RuleFixCount { rule, count })
        .collect();

    FixResultsSummary {
        fixes_applied: fixes.len(),
        files_modified: files.len(),
        rules,
    }
}

pub fn summarize_optional_fix_results(fixes: &[FixApplied]) -> Option<FixResultsSummary> {
    if fixes.is_empty() {
        None
    } else {
        Some(summarize_fix_results(fixes))
    }
}

pub fn summarize_audit_fix_result(
    fix_result: &crate::refactor::auto::FixResult,
) -> FixResultsSummary {
    use std::collections::{BTreeMap, HashSet};

    let mut files = HashSet::new();
    let mut rule_counts: BTreeMap<String, usize> = BTreeMap::new();
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
                total_fixes += 1;
            }
        }
    }

    for new_file in &fix_result.new_files {
        if new_file.written {
            files.insert(new_file.file.clone());
            let rule = format!("{:?}", new_file.finding).to_lowercase();
            *rule_counts.entry(rule).or_insert(0) += 1;
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

    FixResultsSummary {
        fixes_applied: total_fixes,
        files_modified: files.len(),
        rules,
    }
}
