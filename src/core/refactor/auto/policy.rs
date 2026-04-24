use crate::code_audit::AuditFinding;
use crate::refactor::auto::{FixPolicy, FixResult, Insertion, NewFile, PolicySummary};

fn finding_allowed(finding: &AuditFinding, policy: &FixPolicy) -> bool {
    let included = policy
        .only
        .as_ref()
        .is_none_or(|only| only.contains(finding));

    included && !policy.exclude.contains(finding)
}

/// Manual-only edits never auto-apply.
/// Heuristic/graph findings remain visible in dry-run previews, but are not
/// eligible for unattended writes unless their confidence policy allows it.
fn should_auto_apply(finding: &AuditFinding, manual_only: bool) -> bool {
    !manual_only && finding.confidence().allows_automated_refactor()
}

fn blocked_reason(finding: &AuditFinding, manual_only: bool) -> String {
    if manual_only {
        "Blocked: manual-only edit, not eligible for --from auto-write".to_string()
    } else if !finding.confidence().allows_automated_refactor() {
        format!(
            "Blocked: {:?} confidence finding requires human review before automated writes",
            finding.confidence()
        )
    } else {
        "Blocked by policy".to_string()
    }
}

fn annotate_insertion_for_policy(
    insertion: &mut Insertion,
    _write: bool,
    policy: &FixPolicy,
) -> bool {
    if !finding_allowed(&insertion.finding, policy) {
        return false;
    }

    insertion.auto_apply = should_auto_apply(&insertion.finding, insertion.manual_only);
    insertion.blocked_reason = if insertion.auto_apply {
        None
    } else {
        Some(blocked_reason(&insertion.finding, insertion.manual_only))
    };

    true
}

fn annotate_new_file_for_policy(new_file: &mut NewFile, _write: bool, policy: &FixPolicy) -> bool {
    if !finding_allowed(&new_file.finding, policy) {
        return false;
    }

    new_file.auto_apply = should_auto_apply(&new_file.finding, new_file.manual_only);
    new_file.blocked_reason = if new_file.auto_apply {
        None
    } else {
        Some(blocked_reason(&new_file.finding, new_file.manual_only))
    };

    true
}

pub fn apply_fix_policy(result: &mut FixResult, write: bool, policy: &FixPolicy) -> PolicySummary {
    let mut summary = PolicySummary::default();

    result.fixes = result
        .fixes
        .drain(..)
        .filter_map(|mut fix| {
            fix.insertions
                .retain_mut(|insertion| annotate_insertion_for_policy(insertion, write, policy));

            for insertion in &fix.insertions {
                summary.visible_insertions += 1;
                if insertion.auto_apply {
                    summary.auto_apply_insertions += 1;
                } else {
                    summary.blocked_insertions += 1;
                }
            }

            if fix.insertions.is_empty() {
                return None;
            }

            if write && !fix.insertions.iter().any(|ins| ins.auto_apply) {
                summary.dropped_manual_only += 1;
                return None;
            }

            Some(fix)
        })
        .collect();

    result.new_files = result
        .new_files
        .drain(..)
        .filter_map(|mut pending| {
            if !annotate_new_file_for_policy(&mut pending, write, policy) {
                return None;
            }

            summary.visible_new_files += 1;
            if pending.auto_apply {
                summary.auto_apply_new_files += 1;
            } else {
                summary.blocked_new_files += 1;

                if write {
                    summary.dropped_manual_only += 1;
                    return None;
                }
            }

            Some(pending)
        })
        .collect();

    if let Some(ref only) = policy.only {
        result
            .decompose_plans
            .retain(|p| only.contains(&p.source_finding));
    }
    result
        .decompose_plans
        .retain(|p| !policy.exclude.contains(&p.source_finding));

    // Structural decompose writes are too risky for unattended autofix.
    // In dry-run mode they remain visible for preview; in write mode they are
    // dropped entirely. Because this clearing happens before `run_fix_iteration`
    // clones `decompose_plans`, the decompose apply path in verify.rs is
    // unreachable in write mode — any decompose application must go through
    // explicit manual commands (e.g. `refactor decompose`).
    if write {
        summary.dropped_manual_only += result.decompose_plans.len();
        result.decompose_plans.clear();
    }

    result.total_insertions = summary.visible_insertions + summary.visible_new_files;
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refactor::auto::{Fix, FixResult, InsertionKind};

    fn insertion(finding: AuditFinding) -> Insertion {
        Insertion {
            primitive: None,
            kind: InsertionKind::MethodStub,
            finding,
            manual_only: false,
            auto_apply: false,
            blocked_reason: None,
            code: "fn generated() {}".to_string(),
            description: "generated fix".to_string(),
        }
    }

    fn result_with(insertion: Insertion) -> FixResult {
        FixResult {
            fixes: vec![Fix {
                file: "src/lib.rs".to_string(),
                required_methods: Vec::new(),
                required_registrations: Vec::new(),
                insertions: vec![insertion],
                applied: false,
            }],
            new_files: Vec::new(),
            decompose_plans: Vec::new(),
            skipped: Vec::new(),
            chunk_results: Vec::new(),
            total_insertions: 1,
            files_modified: 1,
        }
    }

    #[test]
    fn heuristic_findings_remain_visible_but_not_auto_apply() {
        let mut result = result_with(insertion(AuditFinding::OrphanedTest));

        let summary = apply_fix_policy(&mut result, false, &FixPolicy::default());

        assert_eq!(summary.visible_insertions, 1);
        assert_eq!(summary.auto_apply_insertions, 0);
        assert_eq!(summary.blocked_insertions, 1);
        assert_eq!(result.fixes.len(), 1);
        assert!(!result.fixes[0].insertions[0].auto_apply);
        assert!(result.fixes[0].insertions[0]
            .blocked_reason
            .as_ref()
            .is_some_and(|reason| reason.contains("Heuristic confidence")));
    }

    #[test]
    fn heuristic_findings_are_dropped_in_write_mode() {
        let mut result = result_with(insertion(AuditFinding::OrphanedTest));

        let summary = apply_fix_policy(&mut result, true, &FixPolicy::default());

        assert_eq!(summary.visible_insertions, 1);
        assert_eq!(summary.auto_apply_insertions, 0);
        assert_eq!(summary.blocked_insertions, 1);
        assert_eq!(summary.dropped_manual_only, 1);
        assert!(result.fixes.is_empty());
    }

    #[test]
    fn structural_findings_auto_apply_by_default() {
        let mut result = result_with(insertion(AuditFinding::CompilerWarning));

        let summary = apply_fix_policy(&mut result, true, &FixPolicy::default());

        assert_eq!(summary.visible_insertions, 1);
        assert_eq!(summary.auto_apply_insertions, 1);
        assert_eq!(summary.blocked_insertions, 0);
        assert_eq!(result.fixes.len(), 1);
        assert!(result.fixes[0].insertions[0].auto_apply);
    }
}
