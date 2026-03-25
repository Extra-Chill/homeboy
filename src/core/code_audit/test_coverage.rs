//! Structural test coverage gap detection — identify source files and methods
//! that lack corresponding tests.
//!
//! Plugs into the audit pipeline as Phase 4f. Uses the extension's
//! `TestMappingConfig` to understand how source files map to test files
//! and how source methods map to test methods.
//!
//! Performs four checks:
//! 1. Missing test files — source files with no corresponding test file
//! 2. Missing test methods — source methods with no corresponding test method
//! 3. Orphaned tests — test files with no corresponding source file
//! 4. Orphaned test methods — test methods whose source method no longer exists

mod extract_test_methods;
mod helpers;
mod methods;

pub use extract_test_methods::*;
pub use helpers::*;
pub use methods::*;


use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::test_mapping::{
    build_source_name_index, partition_fingerprints, source_to_test_path, test_to_source_path,
};
use crate::extension::TestMappingConfig;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;

    fn make_config() -> TestMappingConfig {
        TestMappingConfig {
            source_dirs: vec!["src".to_string()],
            test_dirs: vec!["tests".to_string()],
            test_file_pattern: "tests/{dir}/{name}_test.{ext}".to_string(),
            method_prefix: "test_".to_string(),
            inline_tests: false,
            critical_patterns: vec!["core/".to_string()],
            skip_test_patterns: vec![],
        }
    }

    fn make_rust_config() -> TestMappingConfig {
        TestMappingConfig {
            source_dirs: vec!["src".to_string()],
            test_dirs: vec!["tests".to_string()],
            test_file_pattern: "tests/{dir}/{name}_test.{ext}".to_string(),
            method_prefix: "test_".to_string(),
            inline_tests: true,
            critical_patterns: vec!["core/".to_string()],
            skip_test_patterns: vec![],
        }
    }

    fn make_fp(path: &str, methods: Vec<&str>) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn source_to_test_path_basic() {
        let config = make_config();
        assert_eq!(
            source_to_test_path("src/core/audit.rs", &config),
            Some("tests/core/audit_test.rs".to_string())
        );
    }

    #[test]
    fn source_to_test_path_top_level() {
        let config = make_config();
        assert_eq!(
            source_to_test_path("src/main.rs", &config),
            Some("tests/main_test.rs".to_string())
        );
    }

    #[test]
    fn test_to_source_path_basic() {
        let config = make_config();
        assert_eq!(
            test_to_source_path("tests/core/audit_test.rs", &config),
            Some("src/core/audit.rs".to_string())
        );
    }

    #[test]
    fn missing_test_file_detected() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_missing_file");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        let source = make_fp("src/core/audit.rs", vec!["run_audit", "build_report"]);

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::MissingTestFile);
        assert_eq!(findings[0].severity, Severity::Warning); // core/ is critical
        assert!(findings[0].description.contains("audit_test.rs"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_test_method_detected() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_missing_method");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        let source = make_fp("src/parser.rs", vec!["parse", "validate", "transform"]);
        let test = make_fp("tests/parser_test.rs", vec!["test_parse", "test_validate"]);

        let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

        let missing_methods: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::MissingTestMethod)
            .collect();
        assert_eq!(missing_methods.len(), 1);
        assert!(missing_methods[0].description.contains("transform"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphaned_test_detected() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_orphaned");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        // Test file exists but source file doesn't
        let test = make_fp("tests/old_module_test.rs", vec!["test_something"]);

        let findings = analyze_test_coverage(&dir, &[&test], &config);

        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::OrphanedTest)
            .collect();
        assert_eq!(orphaned.len(), 1);
        assert!(orphaned[0].description.contains("old_module"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_tests_satisfy_coverage() {
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_inline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        // Source file with inline test methods
        let source = make_fp(
            "src/utils.rs",
            vec!["helper", "compute", "test_helper", "test_compute"],
        );

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        // All methods have inline tests — no findings
        let missing: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::MissingTestFile || f.kind == AuditFinding::MissingTestMethod
            })
            .collect();
        assert!(missing.is_empty(), "Inline tests should satisfy coverage");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_tests_partial_coverage() {
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_inline_partial");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core")).unwrap();

        // Source file with only some inline tests
        let source = make_fp(
            "src/core/engine.rs",
            vec!["start", "stop", "reset", "test_start"],
        );

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        let missing_methods: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::MissingTestMethod)
            .collect();
        // "stop" and "reset" should be missing tests
        assert_eq!(missing_methods.len(), 2);
        // Critical path (core/) should produce Warning severity
        assert!(missing_methods
            .iter()
            .all(|f| f.severity == Severity::Warning));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trivial_methods_not_flagged() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_trivial");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        // Source file with only trivial methods
        let source = make_fp("src/types.rs", vec!["new", "default", "clone", "fmt"]);
        // Empty test file (exists but no test methods)
        let test = make_fp("tests/types_test.rs", vec![]);

        let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

        let missing_methods: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::MissingTestMethod)
            .collect();
        assert!(
            missing_methods.is_empty(),
            "Trivial methods should not be flagged"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_critical_paths_get_info_severity() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_non_critical");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/utils")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        // utils/ is not in critical_patterns
        let source = make_fp("src/utils/helpers.rs", vec!["format_output"]);

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fully_tested_source_no_findings() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_full");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        let source = make_fp("src/parser.rs", vec!["parse", "validate"]);
        let test = make_fp("tests/parser_test.rs", vec!["test_parse", "test_validate"]);

        let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

        let coverage_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::MissingTestFile || f.kind == AuditFinding::MissingTestMethod
            })
            .collect();
        assert!(
            coverage_findings.is_empty(),
            "Fully tested source should have no coverage findings"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn php_test_mapping() {
        let config = TestMappingConfig {
            source_dirs: vec!["inc".to_string()],
            test_dirs: vec!["tests/Unit".to_string()],
            test_file_pattern: "tests/Unit/{dir}/{name}Test.{ext}".to_string(),
            method_prefix: "test_".to_string(),
            inline_tests: false,
            critical_patterns: vec!["Abilities/".to_string()],
            skip_test_patterns: vec![],
        };

        assert_eq!(
            source_to_test_path("inc/Abilities/Flow/CreateFlow.php", &config),
            Some("tests/Unit/Abilities/Flow/CreateFlowTest.php".to_string())
        );

        assert_eq!(
            test_to_source_path("tests/Unit/Abilities/Flow/CreateFlowTest.php", &config),
            Some("inc/Abilities/Flow/CreateFlow.php".to_string())
        );
    }

    #[test]
    fn rust_inline_uses_disk_test_methods_when_test_file_not_fingerprinted() {
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_disk_methods");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core/refactor")).unwrap();
        std::fs::create_dir_all(dir.join("tests/core/refactor")).unwrap();

        std::fs::write(
            dir.join("tests/core/refactor/decompose_test.rs"),
            "#[test]\nfn test_build_plan() {}\n#[test]\nfn test_apply_plan_skeletons() {}\n",
        )
        .unwrap();

        let source = make_fp(
            "src/core/refactor/decompose.rs",
            vec!["build_plan", "apply_plan_skeletons"],
        );

        // Intentionally do not include test fingerprint in `fingerprints` to mimic
        // singleton test-file directories excluded from convention grouping.
        let findings = analyze_test_coverage(&dir, &[&source], &config);

        assert!(findings.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_methods_not_flagged() {
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_visibility");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core")).unwrap();

        let mut source = make_fp(
            "src/core/engine.rs",
            vec!["run", "helper_fn", "internal_parse", "test_run"],
        );
        // run is public, helper_fn and internal_parse are private
        source
            .visibility
            .insert("run".to_string(), "public".to_string());
        source
            .visibility
            .insert("helper_fn".to_string(), "private".to_string());
        source
            .visibility
            .insert("internal_parse".to_string(), "private".to_string());

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        // Only "run" should be covered (by test_run), private methods skipped entirely.
        // No findings expected since run has test_run.
        let missing_methods: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::MissingTestMethod)
            .collect();
        assert!(
            missing_methods.is_empty(),
            "Private methods should not be flagged: {:?}",
            missing_methods
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skip_test_patterns_excludes_files() {
        let mut config = make_rust_config();
        config.skip_test_patterns = vec!["commands/".to_string()];

        let dir = std::env::temp_dir().join("homeboy_test_coverage_skip_patterns");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/commands")).unwrap();
        std::fs::create_dir_all(dir.join("src/core")).unwrap();

        let cmd_source = make_fp("src/commands/deploy.rs", vec!["run_deploy"]);
        let core_source = make_fp("src/core/deploy.rs", vec!["execute_deploy"]);

        let findings = analyze_test_coverage(&dir, &[&cmd_source, &core_source], &config);

        // commands/deploy.rs should be skipped, core/deploy.rs should NOT
        let flagged_files: Vec<&str> = findings.iter().map(|f| f.file.as_str()).collect();
        assert!(
            !flagged_files.contains(&"src/commands/deploy.rs"),
            "commands/ should be skipped"
        );
        assert!(
            flagged_files.contains(&"src/core/deploy.rs"),
            "core/ should NOT be skipped"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trivial_getters_not_flagged() {
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_getters");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        // File with only getter/accessor methods
        let source = make_fp(
            "src/config.rs",
            vec!["get_name", "is_enabled", "has_value", "as_str", "len"],
        );
        let test = make_fp("tests/config_test.rs", vec![]);

        let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

        let missing_methods: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::MissingTestMethod)
            .collect();
        assert!(
            missing_methods.is_empty(),
            "Trivial getters should not be flagged: {:?}",
            missing_methods
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ========================================================================
    // Orphaned test method detection (Check 4)
    // ========================================================================

    #[test]
    fn orphaned_test_method_inline_detected() {
        // Source has discover_from_portable and has_portable_config.
        // Tests: test_discover_from_portable (valid — exact match),
        //        test_discover_stale_data (orphaned — 3+ segments, first word "discover"
        //          matches source method "discover_from_portable", but "discover_stale_data"
        //          doesn't match any source method by exact or prefix match),
        //        test_load_config (orphaned — "load_config" is 2 segments, no exact/prefix match).
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_orphaned_inline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core")).unwrap();

        let source = make_fp(
            "src/core/component.rs",
            vec![
                "discover_from_portable",
                "has_portable_config",
                "test_discover_from_portable", // valid — source method exists
                "test_discover_stale_data",    // orphaned — first word matches but no prefix match
                "test_load_config",            // orphaned — short name, no match at all
            ],
        );

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
            })
            .collect();
        assert_eq!(
            orphaned.len(),
            2,
            "Should detect 2 orphaned test methods, found: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
        assert!(orphaned
            .iter()
            .any(|f| f.description.contains("discover_stale_data")));
        assert!(orphaned
            .iter()
            .any(|f| f.description.contains("load_config")));
        assert!(orphaned.iter().all(|f| f.severity == Severity::Warning));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphaned_test_method_external_file_detected() {
        // A test file has test_old_function but the source no longer has old_function
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_orphaned_external");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        let source = make_fp("src/parser.rs", vec!["parse", "validate"]);
        let test = make_fp(
            "tests/parser_test.rs",
            vec![
                "test_parse",        // valid
                "test_validate",     // valid
                "test_old_function", // orphaned — old_function was deleted
            ],
        );

        let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
            })
            .collect();
        assert_eq!(orphaned.len(), 1);
        assert!(orphaned[0].description.contains("old_function"));
        assert!(orphaned[0].file.contains("parser_test.rs"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphaned_test_method_not_flagged_when_source_exists() {
        // All test methods map to existing source methods — no findings
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_no_orphaned");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let source = make_fp(
            "src/utils.rs",
            vec!["helper", "compute", "test_helper", "test_compute"],
        );

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
            })
            .collect();
        assert!(
            orphaned.is_empty(),
            "No orphaned test methods expected: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphaned_test_method_mixed_valid_and_orphaned() {
        // Some tests valid, some orphaned — only orphaned should be flagged
        let config = make_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_mixed_orphaned");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();

        let source = make_fp("src/engine.rs", vec!["start", "stop"]);
        let test = make_fp(
            "tests/engine_test.rs",
            vec![
                "test_start",  // valid
                "test_stop",   // valid
                "test_pause",  // orphaned
                "test_resume", // orphaned
            ],
        );

        let findings = analyze_test_coverage(&dir, &[&source, &test], &config);

        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
            })
            .collect();
        assert_eq!(orphaned.len(), 2);

        let orphaned_names: Vec<&str> = orphaned.iter().map(|f| f.description.as_str()).collect();
        assert!(orphaned_names.iter().any(|d| d.contains("pause")));
        assert!(orphaned_names.iter().any(|d| d.contains("resume")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_analyze_test_coverage_test_file_exists_has_inline_tests() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: !test_file_exists && !has_inline_tests");
    }

    #[test]
    fn test_analyze_test_coverage_if_let_some_source_method_method_strip_prefix_config_method_() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(source_method) = method.strip_prefix(&config.method_prefix) {{");
    }

    #[test]
    fn test_analyze_test_coverage_if_let_some_test_fingerprint_test_fp() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(test_fingerprint) = test_fp {{");
    }

    #[test]
    fn test_analyze_test_coverage_let_some_test_fingerprint_test_fp() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(test_fingerprint) = test_fp");
    }

    #[test]
    fn test_analyze_test_coverage_let_some_source_method_method_strip_prefix_config_method_pre() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(source_method) = method.strip_prefix(&config.method_prefix)");
    }

    #[test]
    fn test_analyze_test_coverage_let_some_test_methods_disk_test_methods() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: let Some(test_methods) = &disk_test_methods");
    }

    #[test]
    fn test_analyze_test_coverage_if_let_some_test_fingerprint_test_fp_2() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(test_fingerprint) = test_fp {{");
    }

    #[test]
    fn test_analyze_test_coverage_else_if_let_some_test_methods_disk_test_methods() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: }} else if let Some(test_methods) = &disk_test_methods {{");
    }

    #[test]
    fn test_analyze_test_coverage_test_file_exists() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: !test_file_exists");
    }

    #[test]
    fn test_analyze_test_coverage_let_test_methods_vec_string_if_let_some_test_fingerprint_tes() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: let test_methods: Vec<String> = if let Some(test_fingerprint) = test_fp {{");
    }

    #[test]
    fn test_analyze_test_coverage_if_let_some_ref_source_path_expected_source_path() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(ref source_path) = expected_source_path {{");
    }

    #[test]
    fn test_analyze_test_coverage_if_let_some_actual_source_path_discovered() {

        let result = analyze_test_coverage();
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(actual_source_path) = discovered {{");
    }

    #[test]
    fn test_analyze_test_coverage_has_expected_effects() {
        // Expected effects: mutation

        let _ = analyze_test_coverage();
    }

}
