use crate::component::Component;
use crate::extension::test::TestScopeOutput;
use crate::refactor::AppliedRefactor;
use crate::refactor::{
    self,
    auto::{self, AutofixMode},
    TransformSet,
};
use crate::test_analyze::TestAnalysis;
use crate::test_baseline::{TestBaselineComparison, TestCounts};
use crate::test_drift::{self, DriftOptions, DriftReport};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AutoFixDriftOutput {
    pub since: String,
    pub auto_fixable_changes: usize,
    pub generated_rules: usize,
    pub replacements: usize,
    pub files_modified: usize,
    pub written: bool,
    pub rerun_recommended: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriftWorkflowResult {
    pub component: String,
    pub report: DriftReport,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AutoFixDriftWorkflowResult {
    pub component: String,
    pub output: AutoFixDriftOutput,
    pub hints: Vec<String>,
    pub report: Option<DriftReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MainTestWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub test_counts: Option<TestCounts>,
    pub coverage: Option<serde_json::Value>,
    pub baseline_comparison: Option<TestBaselineComparison>,
    pub analysis: Option<TestAnalysis>,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub test_scope: Option<TestScopeOutput>,
    pub summary: Option<serde_json::Value>,
}

pub fn detect_test_drift(
    component_id: &str,
    component: &Component,
    since: &str,
) -> Result<DriftWorkflowResult, crate::Error> {
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        std::path::PathBuf::from(expanded.as_ref())
    };

    crate::log_status!(
        "drift",
        "Detecting test drift since {} in {}",
        since,
        component_id
    );

    let opts = if source_path.join("Cargo.toml").exists() {
        DriftOptions::rust(&source_path, since)
    } else {
        DriftOptions::php(&source_path, since)
    };

    let report = test_drift::detect_drift(component_id, &opts)?;

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
                test_drift::ChangeType::MethodRename => "method rename",
                test_drift::ChangeType::MethodRemoved => "method removed",
                test_drift::ChangeType::ClassRename => "class rename",
                test_drift::ChangeType::ClassRemoved => "class removed",
                test_drift::ChangeType::ErrorCodeChange => "error code change",
                test_drift::ChangeType::ReturnTypeChange => "return type change",
                test_drift::ChangeType::SignatureChange => "signature change",
                test_drift::ChangeType::FileMove => "file moved",
                test_drift::ChangeType::StringChange => "string changed",
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

    let opts = if source_path.join("Cargo.toml").exists() {
        DriftOptions::rust(&source_path, since)
    } else {
        DriftOptions::php(&source_path, since)
    };

    crate::log_status!(
        "test",
        "Auto-fixing drift since {} in {} ({})",
        since,
        component_id,
        if write { "write" } else { "dry-run" }
    );

    let drift_report = test_drift::detect_drift(component_id, &opts)?;
    let rules = test_drift::generate_transform_rules(&drift_report);

    let output = if rules.is_empty() {
        crate::log_status!("test", "No auto-fixable drift detected. Nothing to apply.");

        AutoFixDriftOutput {
            since: since.to_string(),
            auto_fixable_changes: drift_report.auto_fixable,
            generated_rules: 0,
            replacements: 0,
            files_modified: 0,
            written: write,
            rerun_recommended: false,
        }
    } else {
        let set = TransformSet {
            description: format!(
                "Auto-generated drift fixes for {} since {}",
                component_id, since
            ),
            rules,
        };

        let result =
            refactor::apply_transforms(&source_path, "test_auto_fix_drift", &set, write, None)?;

        crate::log_status!(
            "test",
            "Applied {} replacement{} across {} file{}",
            result.total_replacements,
            if result.total_replacements == 1 {
                ""
            } else {
                "s"
            },
            result.total_files,
            if result.total_files == 1 { "" } else { "s" },
        );

        if !write {
            crate::log_status!(
                "hint",
                "Dry-run only. Re-run with --write to apply generated fixes."
            );
        } else if result.total_replacements > 0 {
            crate::log_status!(
                "hint",
                "Re-run tests: homeboy test {} --analyze",
                component_id
            );
        }

        AutoFixDriftOutput {
            since: since.to_string(),
            auto_fixable_changes: drift_report.auto_fixable,
            generated_rules: set.rules.len(),
            replacements: result.total_replacements,
            files_modified: result.total_files,
            written: write,
            rerun_recommended: write && result.total_replacements > 0,
        }
    };

    let outcome = auto::standard_outcome(
        if write {
            AutofixMode::Write
        } else {
            AutofixMode::DryRun
        },
        output.replacements,
        Some(format!("homeboy test {} --analyze", component_id)),
        vec![format!(
            "Use --since <ref> to target a drift window (current: {})",
            since
        )],
    );

    Ok(AutoFixDriftWorkflowResult {
        component: component_id.to_string(),
        output: AutoFixDriftOutput {
            rerun_recommended: outcome.rerun_recommended,
            ..output
        },
        hints: outcome.hints,
        report: if include_report {
            Some(drift_report)
        } else {
            None
        },
    })
}
