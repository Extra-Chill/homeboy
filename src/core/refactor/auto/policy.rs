use crate::code_audit::AuditFinding;
use crate::refactor::auto::preflight;
use crate::refactor::auto::{
    FixPolicy, FixResult, FixSafetyTier, Insertion, NewFile, PolicySummary, PreflightContext,
    PreflightReport, PreflightStatus,
};

pub(crate) fn blocked_reason_from_preflight(report: &PreflightReport) -> Option<String> {
    report
        .checks
        .iter()
        .find(|check| !check.passed)
        .map(|check| format!("Blocked by preflight {}: {}", check.name, check.detail))
}

fn finding_allowed(finding: &AuditFinding, policy: &FixPolicy) -> bool {
    let included = policy
        .only
        .as_ref()
        .is_none_or(|only| only.contains(finding));

    included && !policy.exclude.contains(finding)
}

/// Determine if an insertion should be auto-applied.
///
/// Safe tier: auto-apply if preflight passes (or no preflight applicable).
/// PlanOnly: never auto-apply.
/// In dry-run mode (write=false): everything is "auto-apply" for preview purposes.
fn should_auto_apply(
    tier: FixSafetyTier,
    preflight: Option<&PreflightReport>,
    write: bool,
) -> bool {
    if !write {
        return true;
    }
    match tier {
        FixSafetyTier::Safe => preflight
            .map(|report| {
                matches!(
                    report.status,
                    PreflightStatus::Passed | PreflightStatus::NotApplicable
                )
            })
            .unwrap_or(true), // No preflight report → auto-apply (simple fix)
        FixSafetyTier::PlanOnly => false,
    }
}

/// Determine the blocked reason for a non-auto-applied fix.
fn blocked_reason(tier: FixSafetyTier, preflight: Option<&PreflightReport>) -> String {
    match tier {
        FixSafetyTier::Safe => preflight
            .and_then(blocked_reason_from_preflight)
            .unwrap_or_else(|| "Blocked by preflight validation".to_string()),
        FixSafetyTier::PlanOnly => {
            "Blocked: plan-only fix, not eligible for auto-write".to_string()
        }
    }
}

fn annotate_insertion_for_policy(
    file: &str,
    insertion: &mut Insertion,
    write: bool,
    policy: &FixPolicy,
    context: &PreflightContext<'_>,
) -> bool {
    if !finding_allowed(&insertion.finding, policy) {
        return false;
    }

    insertion.preflight = preflight::run_insertion_preflight(file, insertion, context);
    insertion.auto_apply =
        should_auto_apply(insertion.safety_tier, insertion.preflight.as_ref(), write);
    insertion.blocked_reason = if insertion.auto_apply {
        None
    } else {
        Some(blocked_reason(
            insertion.safety_tier,
            insertion.preflight.as_ref(),
        ))
    };

    true
}

fn annotate_new_file_for_policy(
    new_file: &mut NewFile,
    write: bool,
    policy: &FixPolicy,
    context: &PreflightContext<'_>,
) -> bool {
    if !finding_allowed(&new_file.finding, policy) {
        return false;
    }

    new_file.preflight = preflight::run_new_file_preflight(new_file, context);
    new_file.auto_apply =
        should_auto_apply(new_file.safety_tier, new_file.preflight.as_ref(), write);
    new_file.blocked_reason = if new_file.auto_apply {
        None
    } else {
        Some(blocked_reason(
            new_file.safety_tier,
            new_file.preflight.as_ref(),
        ))
    };

    true
}

pub fn apply_fix_policy(
    result: &mut FixResult,
    write: bool,
    policy: &FixPolicy,
    context: &PreflightContext<'_>,
) -> PolicySummary {
    let mut summary = PolicySummary::default();

    result.fixes = result
        .fixes
        .drain(..)
        .filter_map(|mut fix| {
            fix.insertions.retain_mut(|insertion| {
                annotate_insertion_for_policy(&fix.file, insertion, write, policy, context)
            });

            preflight::run_fix_preflight(&mut fix, context, write);

            // Re-evaluate auto_apply after fix-level preflight
            for insertion in &mut fix.insertions {
                insertion.auto_apply =
                    should_auto_apply(insertion.safety_tier, insertion.preflight.as_ref(), write);
                insertion.blocked_reason = if insertion.auto_apply {
                    None
                } else {
                    Some(blocked_reason(
                        insertion.safety_tier,
                        insertion.preflight.as_ref(),
                    ))
                };

                summary.visible_insertions += 1;
                if insertion.auto_apply {
                    summary.auto_apply_insertions += 1;
                } else {
                    summary.blocked_insertions += 1;
                    if insertion
                        .preflight
                        .as_ref()
                        .is_some_and(|report| report.status == PreflightStatus::Failed)
                    {
                        summary.preflight_failures += 1;
                    }
                }
            }

            if fix.insertions.is_empty() {
                return None;
            }

            // In write mode, drop fixes where no insertion is auto-applicable.
            // PlanOnly fixes are useful in dry-run/preview mode but waste time
            // in CI autofix pipelines — they go through chunk verification and
            // JSON output without ever being written.
            if write && !fix.insertions.iter().any(|ins| ins.auto_apply) {
                summary.dropped_plan_only += 1;
                return None;
            }

            Some(fix)
        })
        .collect();

    result.new_files = result
        .new_files
        .drain(..)
        .filter_map(|mut pending| {
            if !annotate_new_file_for_policy(&mut pending, write, policy, context) {
                return None;
            }

            summary.visible_new_files += 1;
            if pending.auto_apply {
                summary.auto_apply_new_files += 1;
            } else {
                summary.blocked_new_files += 1;
                if pending
                    .preflight
                    .as_ref()
                    .is_some_and(|report| report.status == PreflightStatus::Failed)
                {
                    summary.preflight_failures += 1;
                }

                // In write mode, drop non-auto-applicable new files.
                if write {
                    summary.dropped_plan_only += 1;
                    return None;
                }
            }

            Some(pending)
        })
        .collect();

    // Filter decompose plans by policy — retain plans whose source finding
    // matches the --only filter, or isn't in the --exclude list.
    if let Some(ref only) = policy.only {
        result
            .decompose_plans
            .retain(|p| only.contains(&p.source_finding));
    }
    result
        .decompose_plans
        .retain(|p| !policy.exclude.contains(&p.source_finding));

    result.total_insertions = summary.visible_insertions + summary.visible_new_files;
    summary
}
