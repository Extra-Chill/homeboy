//! Code audit system for convention detection, drift analysis, and structural complexity.
//!
//! Scans source code to discover structural conventions, detect outliers,
//! report architectural drift, and flag structural issues (god files, high item counts).
//! Works by:
//!
//! 1. Fingerprinting source files (extract methods, registrations, types)
//! 2. Grouping files by directory and language
//! 3. Discovering conventions (patterns most files follow)
//! 4. Checking all files against discovered conventions
//! 5. Producing actionable findings for outliers
//! 6. Analyzing structural complexity (god files, high item counts)

pub mod baseline;
pub mod baseline_merge;
mod checks;
pub mod codebase_map;
mod comment_blocks;
mod comment_hygiene;
pub mod compare;
mod compiler_warnings;
mod convention_membership;
pub(crate) mod conventions;
pub(crate) mod core_fingerprint;
mod dead_code;
mod descriptor_runtime;
mod detectors;
mod discovery;
pub mod docs_audit;
mod duplication;
mod execution_plan;
mod findings;
pub mod fingerprint;
mod idiomatic;
pub(crate) mod impact;
pub(crate) mod import_matching;
pub(crate) mod naming;
pub mod report;
mod requirements;
pub mod run;
mod shadow_modules;
mod signatures;
mod source_locations;
mod structural;
pub(crate) mod test_mapping;
pub(crate) mod walker;

mod doc_drift;
mod engine;
mod entry;
mod reference;
mod types;

#[cfg(test)]
pub(crate) mod test_helpers;

// ============================================================================
// Re-exports — the code_audit public API surface. The implementation lives in
// focused submodules (`types`, `entry`, `engine`, `doc_drift`, `reference`)
// plus the pre-existing detector/analysis modules above; this root re-exports
// them so external `crate::core::code_audit::X` paths keep working unchanged.
// ============================================================================

pub use baseline_merge::{merge_baseline_only_conflict, BaselineMergeError, BaselineMergeResult};
pub use checks::{CheckResult, CheckStatus};
pub use compare::{
    finding_fingerprint, score_delta, weighted_finding_score_with, AuditConvergenceScoring,
};
pub use conventions::{AuditFinding, Convention, Deviation, Language, Outlier};
pub use duplication::DuplicateGroup;
pub use execution_plan::AuditProfile;
pub(crate) use execution_plan::{
    AuditExecutionPlan, DetectorDescriptor, DetectorRuntime, FingerprintDetectorRunner,
    GenericDetectorRunner, RootDetectorRunner,
};
pub use findings::{homeboy_finding_from_audit, Finding, FindingConfidence, Severity};
pub use fingerprint::FileFingerprint;
pub use report::AuditCommandOutput;
pub use run::{run_main_audit_workflow, AuditRunWorkflowArgs, AuditRunWorkflowResult};
pub use walker::is_test_path;

pub use entry::{
    audit_component, audit_path, audit_path_scoped, audit_path_with_id,
    source_policy_findings_for_path,
};
pub(crate) use entry::{
    audit_path_scoped_with_plan_and_analysis, audit_path_with_id_with_plan_and_analysis,
};
pub(crate) use types::{time_audit_detector, AuditAnalysisContext, AuditWithAnalysis};
pub use types::{
    AuditSummary, AuditTiming, AuditTimingSpan, CodeAuditResult, ConventionReport,
    DirectoryConvention, DirectoryOutlier,
};

#[cfg(test)]
mod tests {
    use super::entry::audit_config_for;
    use super::reference::fingerprint_component_reference_files;
    use super::types::{time_audit_detector, ScopedAuditExecution};
    use super::*;
    use crate::core::component::AuditConfig;
    use std::fs;

    #[test]
    fn audit_nonexistent_path_returns_error() {
        let result = audit_path("/nonexistent/path/that/does/not/exist");
        assert!(result.is_err());
    }

    #[test]
    fn audit_empty_directory_returns_clean() {
        let dir = std::env::temp_dir().join("homeboy_audit_test_empty");
        let _ = fs::create_dir_all(&dir);

        let result = audit_path(dir.to_str().unwrap()).unwrap();
        assert_eq!(result.summary.files_scanned, 0);
        assert!(result.summary.alignment_score.is_none());
        assert!(result.conventions.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_vacuous_test_keeps_all_vacuous_detector_families_enabled() {
        let plan = AuditExecutionPlan::from_filters(&[AuditFinding::VacuousTest], &[]);

        assert!(
            plan.detector_enabled("test_coverage"),
            "coverage detector also emits vacuous_test for mapped tests"
        );
        assert!(
            plan.detector_enabled("test_topology"),
            "test topology/test quality detector emits standalone vacuous_test findings"
        );
    }

    #[test]
    fn detector_timing_records_successful_span() {
        let mut timing = AuditTiming::default();
        let value = time_audit_detector(&mut timing, "detector.structural", true, || 42, || 0);

        assert_eq!(value, 42);
        assert_eq!(timing.spans.len(), 1);
        assert_eq!(timing.spans[0].id, "detector.structural");
        assert_eq!(timing.spans[0].status, "ok");
        assert!(timing.spans[0].duration_ms.is_some());
    }

    #[test]
    fn detector_timing_records_disabled_span_as_skipped() {
        let mut timing = AuditTiming::default();
        let value = time_audit_detector(
            &mut timing,
            "detector.duplication.exact",
            false,
            || 42,
            || 0,
        );

        assert_eq!(value, 0);
        assert_eq!(timing.spans.len(), 1);
        assert_eq!(timing.spans[0].id, "detector.duplication.exact");
        assert_eq!(timing.spans[0].status, "skipped");
        assert!(timing.spans[0].duration_ms.is_none());
    }

    #[test]
    fn scoped_audit_execution_caches_changed_file_set() {
        let changed = vec![
            "src/core/code_audit/mod.rs".to_string(),
            "src/core/code_audit/mod.rs".to_string(),
            "src/core/code_audit/run.rs".to_string(),
        ];

        let scoped = ScopedAuditExecution::new(Some(&changed), Some("origin/main"));

        assert!(scoped.is_scoped());
        assert!(scoped.impact_tracing_enabled());
        assert_eq!(scoped.changed_file_count(), 2);
        assert!(scoped.changed_files.contains("src/core/code_audit/mod.rs"));
        assert!(scoped.changed_files.contains("src/core/code_audit/run.rs"));
    }

    #[test]
    fn audit_config_includes_explicit_extension_overrides() {
        crate::test_support::with_isolated_home(|home| {
            let mut manifest: crate::core::extension::ExtensionManifest =
                serde_json::from_value(serde_json::json!({
                    "name": "Fixture",
                    "version": "0.0.0",
                    "audit": {
                        "detector_rules": {
                            "convention_exception_globs": ["scripts/lint/fixer-helpers.fixture"]
                        }
                    }
                }))
                .expect("manifest");
            manifest.id = "fixture".to_string();
            crate::core::extension::save_manifest(&manifest).expect("save fixture extension");

            let config = audit_config_for(
                "component-without-extension",
                home.path(),
                &["fixture".to_string()],
            );

            assert!(
                config
                    .convention_exception_globs
                    .contains(&"scripts/lint/fixer-helpers.fixture".to_string()),
                "explicit --extension audit rules should feed audit detector config"
            );
        });
    }

    #[test]
    fn test_analyze_layer_ownership() {
        let dir = std::env::temp_dir().join("homeboy_audit_layer_test");
        let _ = fs::create_dir_all(dir.join("inc/Core/Steps"));

        fs::write(
            dir.join("homeboy.json"),
            r#"{
              "audit_rules": {
                "layer_rules": [
                  {
                    "name": "engine-owns-terminal-status",
                    "forbid": {
                      "glob": "inc/Core/Steps/**/*.php",
                      "patterns": ["JobStatus::"]
                    },
                    "allow": {"glob": "inc/Abilities/Engine/**/*.php"}
                  }
                ]
              }
            }"#,
        )
        .unwrap();

        fs::write(
            dir.join("inc/Core/Steps/agent_ping.php"),
            "<?php\n$status = JobStatus::FAILED;\n",
        )
        .unwrap();

        let findings = detectors::layer_ownership::run(&dir);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].convention, "layer_ownership");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_code_reference_fingerprints_include_rust_singleton_and_index_files() {
        let dir =
            std::env::temp_dir().join(format!("homeboy_audit_index_refs_{}", std::process::id()));
        let src = dir.join("src");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&src).unwrap();

        fs::write(src.join("foo.rs"), "pub fn helper() -> bool { true }\n").unwrap();
        fs::write(
            src.join("main.rs"),
            "mod foo;\nfn main() { let _ = foo::helper(); }\n",
        )
        .unwrap();

        let regular_snapshot = walker::walk_source_files_snapshot(&dir);
        let owned_fingerprints: Vec<_> = regular_snapshot
            .iter()
            .filter_map(|(path, content)| fingerprint::fingerprint_content(path, &dir, content))
            .collect();
        let component_ref_fingerprints = fingerprint_component_reference_files(&dir);

        let owned_refs: Vec<_> = owned_fingerprints.iter().collect();
        let component_refs: Vec<_> = component_ref_fingerprints.iter().collect();
        let findings = dead_code::analyze_dead_code_with_config(
            &owned_refs,
            &component_refs,
            &AuditConfig::default(),
        );
        let unreferenced: Vec<_> = findings
            .iter()
            .filter(|finding| finding.kind == AuditFinding::UnreferencedExport)
            .collect();

        assert!(
            unreferenced.is_empty(),
            "singleton and index files should contribute reference-only calls for dead-code analysis, got: {:?}",
            unreferenced
                .iter()
                .map(|finding| &finding.description)
                .collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "Requires PHP extension with fingerprint script installed"]
    fn audit_directory_with_convention() {
        let dir = std::env::temp_dir().join("homeboy_audit_test_conv");
        let steps = dir.join("steps");
        let _ = fs::create_dir_all(&steps);

        // Create 3 files: 2 follow pattern, 1 is an outlier
        fs::write(
            steps.join("step_a.php"),
            r#"<?php
class StepA {
    public function register() {}
    public function validate($input) {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        fs::write(
            steps.join("step_b.php"),
            r#"<?php
class StepB {
    public function register() {}
    public function validate($input) {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        fs::write(
            steps.join("step_c.php"),
            r#"<?php
class StepC {
    public function register() {}
    public function execute($ctx) {}
}
"#,
        )
        .unwrap();

        let result = audit_path(dir.to_str().unwrap()).unwrap();

        assert_eq!(result.summary.files_scanned, 3);
        assert!(result.summary.conventions_detected >= 1);
        assert!(result.summary.outliers_found >= 1);
        assert!(result.summary.alignment_score.unwrap() < 1.0);

        // Find the steps convention
        let steps_conv = result
            .conventions
            .iter()
            .find(|c| c.name == "Steps")
            .expect("Should find Steps convention");

        assert_eq!(steps_conv.total_files, 3);
        assert!(steps_conv
            .expected_methods
            .contains(&"register".to_string()));
        assert!(steps_conv.expected_methods.contains(&"execute".to_string()));
        assert_eq!(steps_conv.outliers.len(), 1);
        assert!(steps_conv.outliers[0].file.contains("step_c"));

        // Should have findings for the outlier
        assert!(!result.findings.is_empty());
        assert!(result
            .findings
            .iter()
            .any(|f| f.file.contains("step_c") && f.description.contains("validate")));

        let _ = fs::remove_dir_all(&dir);
    }
}
