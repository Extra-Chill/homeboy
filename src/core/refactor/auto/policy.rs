use crate::code_audit::fixer::first_failed_detail;
use crate::code_audit::AuditFinding;
use crate::refactor::auto::preflight;
use crate::refactor::auto::{
    FixPolicy, FixResult, FixSafetyTier, Insertion, NewFile, PolicySummary, PreflightContext,
    PreflightStatus,
};

fn finding_allowed(finding: &AuditFinding, policy: &FixPolicy) -> bool {
    let included = policy
        .only
        .as_ref()
        .is_none_or(|only| only.contains(finding));

    included && !policy.exclude.contains(finding)
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

    insertion.auto_apply = if !write {
        true
    } else {
        match insertion.safety_tier {
            FixSafetyTier::SafeAuto => true,
            FixSafetyTier::SafeWithChecks => insertion.preflight.as_ref().is_some_and(|report| {
                matches!(
                    report.status,
                    PreflightStatus::Passed | PreflightStatus::NotApplicable
                )
            }),
            FixSafetyTier::PlanOnly => false,
        }
    };

    insertion.blocked_reason = if insertion.auto_apply {
        None
    } else {
        Some(match insertion.safety_tier {
            FixSafetyTier::SafeAuto => "Blocked by current write policy".to_string(),
            FixSafetyTier::SafeWithChecks => insertion
                .preflight
                .as_ref()
                .and_then(first_failed_detail)
                .unwrap_or_else(|| {
                    "Blocked: requires preflight validation before auto-write".to_string()
                }),
            FixSafetyTier::PlanOnly => {
                "Blocked: plan-only fix, not eligible for auto-write".to_string()
            }
        })
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

    new_file.auto_apply = if !write {
        true
    } else {
        match new_file.safety_tier {
            FixSafetyTier::SafeAuto => true,
            FixSafetyTier::SafeWithChecks => new_file.preflight.as_ref().is_some_and(|report| {
                matches!(
                    report.status,
                    PreflightStatus::Passed | PreflightStatus::NotApplicable
                )
            }),
            FixSafetyTier::PlanOnly => false,
        }
    };

    new_file.blocked_reason = if new_file.auto_apply {
        None
    } else {
        Some(match new_file.safety_tier {
            FixSafetyTier::SafeAuto => "Blocked by current write policy".to_string(),
            FixSafetyTier::SafeWithChecks => new_file
                .preflight
                .as_ref()
                .and_then(first_failed_detail)
                .unwrap_or_else(|| {
                    "Blocked: requires preflight validation before auto-write".to_string()
                }),
            FixSafetyTier::PlanOnly => {
                "Blocked: plan-only fix, not eligible for auto-write".to_string()
            }
        })
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

            for insertion in &mut fix.insertions {
                insertion.auto_apply = if !write {
                    true
                } else {
                    match insertion.safety_tier {
                        FixSafetyTier::SafeAuto => true,
                        FixSafetyTier::SafeWithChecks => {
                            insertion.preflight.as_ref().is_some_and(|report| {
                                matches!(
                                    report.status,
                                    PreflightStatus::Passed | PreflightStatus::NotApplicable
                                )
                            })
                        }
                        FixSafetyTier::PlanOnly => false,
                    }
                };

                insertion.blocked_reason = if insertion.auto_apply {
                    None
                } else {
                    Some(match insertion.safety_tier {
                        FixSafetyTier::SafeAuto => "Blocked by current write policy".to_string(),
                        FixSafetyTier::SafeWithChecks => insertion
                            .preflight
                            .as_ref()
                            .and_then(first_failed_detail)
                            .unwrap_or_else(|| {
                                "Blocked: requires preflight validation before auto-write"
                                    .to_string()
                            }),
                        FixSafetyTier::PlanOnly => {
                            "Blocked: plan-only fix, not eligible for auto-write".to_string()
                        }
                    })
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
                None
            } else {
                Some(fix)
            }
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
            }

            Some(pending)
        })
        .collect();

    if let Some(ref only) = policy.only {
        if !only.contains(&AuditFinding::GodFile) {
            result.decompose_plans.clear();
        }
    }
    if policy.exclude.contains(&AuditFinding::GodFile) {
        result.decompose_plans.clear();
    }

    result.total_insertions = summary.visible_insertions + summary.visible_new_files;
    summary
}
