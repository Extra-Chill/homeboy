use crate::component::Component;
use crate::extension::test::drift::{detect_drift, generate_transform_rules, DriftReport};
use crate::extension::test::resolve_drift_options;
use crate::extension::test::TestScopeOutput;
use crate::extension::test::{ChangeType, TestAnalysis};
use crate::extension::test::{TestBaselineComparison, TestCounts};
pub use homeboy_extension_contract::test_results::{
    AutoFixDriftWorkflowResult, DriftWorkflowResult, MainTestWorkflowResult,
};
pub use homeboy_extension_contract::test_workflow::AutoFixDriftOutput;
use homeboy_refactor_contract::{AppliedRefactor, TransformSet};
use serde::Serialize;

pub fn detect_test_drift(
    component_id: &str,
    component: &Component,
    since: &str,
) -> Result<DriftWorkflowResult, crate::Error> {
    crate::log_status!(
        "drift",
        "Detecting test drift since {} in {}",
        since,
        component_id
    );

    let opts = resolve_drift_options(component, since)?;

    let report = detect_drift(component_id, &opts)?;

    if report.production_changes.is_empty() {
        crate::log_status!("drift", "No production changes detected since {}", since);
    } else {
        crate::log_status!(
            "drift",
            "{} production change{} detected",
            report.production_changes.len(),
            if report.production_changes.len() == 1 {
                ""
            } else {
                "s"
            }
        );

        for change in &report.production_changes {
            let label = match change.change_type {
                ChangeType::MethodRename => "method rename",
                ChangeType::MethodRemoved => "method removed",
                ChangeType::ClassRename => "class rename",
                ChangeType::ClassRemoved => "class removed",
                ChangeType::ErrorCodeChange => "error code change",
                ChangeType::ReturnTypeChange => "return type change",
                ChangeType::SignatureChange => "signature change",
                ChangeType::FileMove => "file moved",
                ChangeType::StringChange => "string changed",
            };

            if let Some(ref new) = change.new_symbol {
                crate::log_status!(
                    "  change",
                    "{}: {} → {} ({})",
                    label,
                    change.old_symbol,
                    new,
                    change.file
                );
            } else {
                crate::log_status!(
                    "  change",
                    "{}: {} ({})",
                    label,
                    change.old_symbol,
                    change.file
                );
            }
        }

        if !report.drifted_tests.is_empty() {
            crate::log_status!(
                "drift",
                "{} drifted reference{} in {} test file{}",
                report.drifted_tests.len(),
                if report.drifted_tests.len() == 1 {
                    ""
                } else {
                    "s"
                },
                report.total_drifted_files,
                if report.total_drifted_files == 1 {
                    ""
                } else {
                    "s"
                },
            );

            for drift in report.drifted_tests.iter().take(20) {
                let change = &report.production_changes[drift.change_index];
                crate::log_status!(
                    "  ref",
                    "{}:{} references '{}' ({})",
                    drift.test_file,
                    drift.line,
                    change.old_symbol,
                    format!("{:?}", change.change_type).to_lowercase()
                );
            }

            if report.drifted_tests.len() > 20 {
                crate::log_status!(
                    "info",
                    "... and {} more (use --json for full list)",
                    report.drifted_tests.len() - 20
                );
            }
        }

        if report.auto_fixable > 0 {
            crate::log_status!(
                "hint",
                "{} change{} auto-fixable with refactor transform",
                report.auto_fixable,
                if report.auto_fixable == 1 { "" } else { "s" }
            );
        }
    }

    let exit_code = if report.drifted_tests.is_empty() {
        0
    } else {
        1
    };

    Ok(DriftWorkflowResult {
        component: component_id.to_string(),
        report,
        exit_code,
    })
}

pub fn auto_fix_test_drift(
    component_id: &str,
    component: &Component,
    since: &str,
    write: bool,
    include_report: bool,
) -> Result<AutoFixDriftWorkflowResult, crate::Error> {
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        std::path::PathBuf::from(expanded.as_ref())
    };

    let opts = resolve_drift_options(component, since)?;

    crate::log_status!(
        "test",
        "Auto-fixing drift since {} in {} ({})",
        since,
        component_id,
        if write { "write" } else { "dry-run" }
    );

    let drift_report = detect_drift(component_id, &opts)?;
    let rules = generate_transform_rules(&drift_report);

    let output = if rules.is_empty() {
        crate::log_status!("test", "No auto-fixable drift detected. Nothing to apply.");

        (
            AutoFixDriftOutput {
                since: since.to_string(),
                auto_fixable_changes: drift_report.auto_fixable,
                generated_rules: 0,
                replacements: 0,
                files_modified: 0,
                written: write,
                rerun_recommended: false,
            },
            Vec::new(),
        )
    } else {
        let set = TransformSet {
            description: format!(
                "Auto-generated drift fixes for {} since {}",
                component_id, since
            ),
            rules,
        };
        let generated_rules = set.rules.len();

        // Applying transforms + formatting the autofix outcome is refactor-engine
        // behavior, inverted behind the refactor transform provider hook so the
        // extension does not depend on the refactor feature layer.
        let summary = crate::refactor_transform_provider::apply_transform_set(
            &source_path,
            "test_auto_fix_drift",
            &set,
            write,
            Some(format!("homeboy review test {} --analyze", component_id)),
            vec![format!(
                "Use --since <ref> to target a drift window (current: {})",
                since
            )],
        )?;

        crate::log_status!(
            "test",
            "Applied {} replacement{} across {} file{}",
            summary.total_replacements,
            if summary.total_replacements == 1 {
                ""
            } else {
                "s"
            },
            summary.total_files,
            if summary.total_files == 1 { "" } else { "s" },
        );

        if !write {
            crate::log_status!(
                "hint",
                "Dry-run only. Re-run with --write to apply generated fixes."
            );
        } else if summary.total_replacements > 0 {
            crate::log_status!(
                "hint",
                "Re-run tests: homeboy review test {} --analyze",
                component_id
            );
        }

        (
            AutoFixDriftOutput {
                since: since.to_string(),
                auto_fixable_changes: drift_report.auto_fixable,
                generated_rules,
                replacements: summary.total_replacements,
                files_modified: summary.total_files,
                written: write,
                rerun_recommended: summary.rerun_recommended,
            },
            summary.hints,
        )
    };
    let (output, outcome_hints) = output;

    Ok(AutoFixDriftWorkflowResult {
        component: component_id.to_string(),
        output,
        hints: outcome_hints,
        report: if include_report {
            Some(drift_report)
        } else {
            None
        },
    })
}
