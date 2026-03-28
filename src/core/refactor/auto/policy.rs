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
/// In dry-run mode (write=false): everything is visible for preview purposes.
fn should_auto_apply(manual_only: bool, write: bool) -> bool {
    if !write {
        return true;
    }
    !manual_only
}

fn blocked_reason(manual_only: bool) -> String {
    if manual_only {
        "Blocked: manual-only edit, not eligible for --from auto-write".to_string()
    } else {
        "Blocked by policy".to_string()
    }
}

fn annotate_insertion_for_policy(
    insertion: &mut Insertion,
    write: bool,
    policy: &FixPolicy,
) -> bool {
    if !finding_allowed(&insertion.finding, policy) {
        return false;
    }

    insertion.auto_apply = should_auto_apply(insertion.manual_only, write);
    insertion.blocked_reason = if insertion.auto_apply {
        None
    } else {
        Some(blocked_reason(insertion.manual_only))
    };

    true
}

fn annotate_new_file_for_policy(new_file: &mut NewFile, write: bool, policy: &FixPolicy) -> bool {
    if !finding_allowed(&new_file.finding, policy) {
        return false;
    }

    new_file.auto_apply = should_auto_apply(new_file.manual_only, write);
    new_file.blocked_reason = if new_file.auto_apply {
        None
    } else {
        Some(blocked_reason(new_file.manual_only))
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

            for insertion in &mut fix.insertions {
                insertion.auto_apply = should_auto_apply(insertion.manual_only, write);
                insertion.blocked_reason = if insertion.auto_apply {
                    None
                } else {
                    Some(blocked_reason(insertion.manual_only))
                };

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

    // Structural decompose writes are still too risky for unattended autofix.
    // Keep them visible in dry-run output, but do not auto-apply them in write
    // mode until the engine is proven safe on real branches.
    if write {
        summary.dropped_manual_only += result.decompose_plans.len();
        result.decompose_plans.clear();
    }

    result.total_insertions = summary.visible_insertions + summary.visible_new_files;
    summary
}
