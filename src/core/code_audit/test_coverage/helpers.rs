//! helpers — extracted from test_coverage.rs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use super::super::conventions::AuditFinding;
use super::super::findings::{Finding, Severity};
use super::super::fingerprint::FileFingerprint;
use super::super::test_mapping::{
    build_source_name_index, partition_fingerprints, source_to_test_path, test_to_source_path,
};
use crate::extension::TestMappingConfig;
use regex::Regex;
use super::find_orphaned_test_methods;
use super::collect_test_methods_from_fp;


/// Analyze test coverage gaps given source fingerprints and a test mapping config.
///
/// `root` is the component root directory (for resolving test file existence).
/// `fingerprints` are all fingerprinted source files from the audit pipeline.
/// `config` is the extension-provided test mapping convention.
pub fn analyze_test_coverage(
    root: &Path,
    fingerprints: &[&FileFingerprint],
    config: &TestMappingConfig,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Partition fingerprints into source files and test files based on path prefixes
    let (source_fps, test_fps) = partition_fingerprints(fingerprints, config);

    // Build a map of test file relative paths -> their fingerprints
    let test_file_map: HashMap<&str, &FileFingerprint> = test_fps
        .iter()
        .map(|fp| (fp.relative_path.as_str(), *fp))
        .collect();

    // Check 1 & 2: For each source file, check for corresponding test file and methods
    for source_fp in &source_fps {
        // Skip files matching skip_test_patterns (e.g. CLI wrappers, pure type defs)
        if is_skipped_path(&source_fp.relative_path, config) {
            continue;
        }

        let expected_test_path = source_to_test_path(&source_fp.relative_path, config);

        let severity = if is_critical(&source_fp.relative_path, config) {
            Severity::Warning
        } else {
            Severity::Info
        };

        // Check if the test file exists (either fingerprinted or on disk)
        let test_fp = expected_test_path
            .as_deref()
            .and_then(|p| test_file_map.get(p).copied());
        let disk_test_methods = expected_test_path
            .as_deref()
            .filter(|_| test_fp.is_none())
            .and_then(|p| load_test_methods_from_disk(root, p, config));
        let test_file_exists = test_fp.is_some()
            || expected_test_path
                .as_deref()
                .map(|p| root.join(p).exists())
                .unwrap_or(false);

        // For inline test languages (Rust), check for #[cfg(test)] in the source file itself
        if config.inline_tests {
            // Rust convention: tests can be inline in the same file.
            // The fingerprint script extracts test methods from #[cfg(test)] modules
            // and includes them in the methods list with their original names.
            // Test methods matching method_prefix (e.g., "test_") indicate inline tests.
            let has_inline_tests = source_fp
                .methods
                .iter()
                .any(|m| m.starts_with(&config.method_prefix));

            if !test_file_exists && !has_inline_tests {
                if let Some(ref test_path) = expected_test_path {
                    findings.push(Finding {
                        convention: "test_coverage".to_string(),
                        severity: severity.clone(),
                        file: source_fp.relative_path.clone(),
                        description: format!(
                            "No test file found (expected '{}') and no inline tests",
                            test_path
                        ),
                        suggestion: format!(
                            "Add tests in '{}' or add #[cfg(test)] inline tests",
                            test_path
                        ),
                        kind: AuditFinding::MissingTestFile,
                    });
                }
                continue; // No tests at all — skip method-level checks
            }

            // Check method coverage: combine inline test methods + dedicated test file methods
            let mut covered_methods: HashSet<&str> = HashSet::new();

            // Inline test methods in the source file
            for method in &source_fp.methods {
                if let Some(source_method) = method.strip_prefix(&config.method_prefix) {
                    covered_methods.insert(source_method);
                }
            }

            // Methods from dedicated test file
            if let Some(test_fingerprint) = test_fp {
                for method in &test_fingerprint.methods {
                    if let Some(source_method) = method.strip_prefix(&config.method_prefix) {
                        covered_methods.insert(source_method);
                    }
                }
            } else if let Some(test_methods) = &disk_test_methods {
                for method in test_methods {
                    if let Some(source_method) = method.strip_prefix(&config.method_prefix) {
                        covered_methods.insert(source_method);
                    }
                }
            }

            // Build set of non-test source method names for orphaned test detection
            let source_methods: HashSet<&str> = source_fp
                .methods
                .iter()
                .filter(|m| !m.starts_with(&config.method_prefix))
                .map(|m| m.as_str())
                .collect();

            // Find source methods without tests (Check 2: MissingTestMethod)
            for method in &source_methods {
                if is_trivial_method(method) {
                    continue;
                }
                if !is_testable_visibility(method, &source_fp.visibility) {
                    continue; // Skip private helpers — tested transitively
                }
                if !covered_methods.contains(method) {
                    findings.push(Finding {
                        convention: "test_coverage".to_string(),
                        severity: severity.clone(),
                        file: source_fp.relative_path.clone(),
                        description: format!(
                            "Method '{}' has no corresponding test (expected '{}{}')",
                            method, config.method_prefix, method
                        ),
                        suggestion: format!(
                            "Add a test method '{}{}' for '{}'",
                            config.method_prefix, method, method
                        ),
                        kind: AuditFinding::MissingTestMethod,
                    });
                }
            }

            // Check 4a: Orphaned test methods (inline) — test methods whose
            // source method no longer exists. This catches tests left behind
            // when a function is deleted from the source.
            find_orphaned_test_methods(
                &mut findings,
                &source_fp.relative_path,
                &collect_test_methods_from_fp(source_fp, config),
                &source_methods,
                config,
            );

            // Also check dedicated test file methods against this source
            if let Some(test_fingerprint) = test_fp {
                find_orphaned_test_methods(
                    &mut findings,
                    &test_fingerprint.relative_path,
                    &collect_test_methods_from_fp(test_fingerprint, config),
                    &source_methods,
                    config,
                );
            } else if let Some(test_methods) = &disk_test_methods {
                let test_path = expected_test_path.as_deref().unwrap_or("test file");
                find_orphaned_test_methods(
                    &mut findings,
                    test_path,
                    test_methods,
                    &source_methods,
                    config,
                );
            }
        } else {
            // Non-inline test languages (PHP, JS, etc.)
            if !test_file_exists {
                if let Some(ref test_path) = expected_test_path {
                    findings.push(Finding {
                        convention: "test_coverage".to_string(),
                        severity: severity.clone(),
                        file: source_fp.relative_path.clone(),
                        description: format!("No test file found (expected '{}')", test_path),
                        suggestion: format!("Create test file '{}'", test_path),
                        kind: AuditFinding::MissingTestFile,
                    });
                }
                continue; // No test file — skip method-level checks
            }

            // Check method coverage from the test file
            let test_methods: Vec<String> = if let Some(test_fingerprint) = test_fp {
                test_fingerprint.methods.clone()
            } else {
                disk_test_methods.unwrap_or_default()
            };

            let source_methods: HashSet<&str> =
                source_fp.methods.iter().map(|m| m.as_str()).collect();

            if !test_methods.is_empty() {
                let covered_methods: HashSet<&str> = test_methods
                    .iter()
                    .filter_map(|m| m.strip_prefix(&config.method_prefix))
                    .collect();

                let test_file_label = test_fp
                    .map(|fp| fp.relative_path.clone())
                    .or(expected_test_path.clone())
                    .unwrap_or_else(|| "test file".to_string());

                for method in &source_fp.methods {
                    if is_trivial_method(method) {
                        continue;
                    }
                    if !is_testable_visibility(method, &source_fp.visibility) {
                        continue; // Skip private helpers — tested transitively
                    }
                    if !covered_methods.contains(method.as_str()) {
                        findings.push(Finding {
                            convention: "test_coverage".to_string(),
                            severity: severity.clone(),
                            file: source_fp.relative_path.clone(),
                            description: format!(
                                "Method '{}' has no corresponding test in '{}'",
                                method, test_file_label
                            ),
                            suggestion: format!(
                                "Add test method '{}{}' to '{}'",
                                config.method_prefix, method, test_file_label
                            ),
                            kind: AuditFinding::MissingTestMethod,
                        });
                    }
                }

                // Check 4b: Orphaned test methods (external file)
                find_orphaned_test_methods(
                    &mut findings,
                    &test_file_label,
                    &test_methods,
                    &source_methods,
                    config,
                );
            }
        }
    }

    // Check 3: Orphaned tests — test files with no corresponding source file.
    //
    // Uses two-tier matching:
    // - Tier 1: template-based path matching (existing behavior)
    // - Tier 2: name-based auto-discovery (finds source files that moved)
    //
    // When a test file's source is found by name at a different path, the test
    // is "misplaced" not "orphaned" — the suggestion is to move it.
    let source_paths: HashSet<&str> = source_fps
        .iter()
        .map(|fp| fp.relative_path.as_str())
        .collect();

    let source_name_index = build_source_name_index(&source_fps);

    for test_fp in &test_fps {
        let expected_source_path = test_to_source_path(&test_fp.relative_path, config);

        if let Some(ref source_path) = expected_source_path {
            let source_exists =
                source_paths.contains(source_path.as_str()) || root.join(source_path).exists();

            if !source_exists {
                // Tier 2: Try name-based discovery — maybe the source moved
                let discovered = super::test_mapping::discover_source_file(
                    &test_fp.relative_path,
                    config,
                    &source_name_index,
                );

                if let Some(actual_source_path) = discovered {
                    // Source exists at a different path — this is a misplaced test, not orphaned.
                    // Compute where the test SHOULD be based on the actual source location.
                    let correct_test_path =
                        source_to_test_path(actual_source_path, config).unwrap_or_default();

                    findings.push(Finding {
                        convention: "test_coverage".to_string(),
                        severity: Severity::Info,
                        file: test_fp.relative_path.clone(),
                        description: format!(
                            "Test file is misplaced — source moved to '{}' (expected test at '{}')",
                            actual_source_path, correct_test_path
                        ),
                        suggestion: format!(
                            "Move test file to '{}' to match source structure",
                            correct_test_path
                        ),
                        kind: AuditFinding::OrphanedTest,
                    });
                } else {
                    // Truly orphaned — no source found anywhere
                    findings.push(Finding {
                        convention: "test_coverage".to_string(),
                        severity: Severity::Info,
                        file: test_fp.relative_path.clone(),
                        description: format!(
                            "Test file has no corresponding source file (expected '{}')",
                            source_path
                        ),
                        suggestion: "Remove the orphaned test or create the source file"
                            .to_string(),
                        kind: AuditFinding::OrphanedTest,
                    });
                }
            }
        }
    }

    // Sort by file path for deterministic output
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

/// Check if a source file matches any critical pattern.
pub(crate) fn is_critical(path: &str, config: &TestMappingConfig) -> bool {
    config
        .critical_patterns
        .iter()
        .any(|pattern| path.contains(pattern))
}

/// Trivial methods that don't warrant individual test coverage findings.
pub(crate) fn is_trivial_method(name: &str) -> bool {
    let trivial = [
        // Rust core trait methods
        "new",
        "default",
        "from",
        "into",
        "clone",
        "fmt",
        "display",
        "eq",
        "hash",
        "drop",
        // Rust common conversions
        "as_str",
        "as_ref",
        "as_mut",
        "to_string",
        "to_str",
        "to_owned",
        // Rust common accessors
        "is_empty",
        "len",
        "iter",
        // Serde
        "serialize",
        "deserialize",
        // Builder pattern
        "build",
        "builder",
        // PHP magic methods
        "__construct",
        "__destruct",
        "__toString",
        "__clone",
        "get_instance",
        "getInstance",
        // Test lifecycle methods (PHPUnit / WP_UnitTestCase)
        // These are optional overrides inherited from the base test class —
        // not every test class needs to define them.
        "set_up",
        "tear_down",
        "set_up_before_class",
        "tear_down_after_class",
        "setUp",
        "tearDown",
        "setUpBeforeClass",
        "tearDownAfterClass",
    ];
    if trivial.contains(&name) {
        return true;
    }
    // Prefix-based rules: simple getters/accessors/predicates
    if name.starts_with("get_") || name.starts_with("is_") || name.starts_with("has_") {
        return true;
    }
    false
}

/// Check if a method should be flagged based on its visibility.
/// Only public and pub(crate) methods warrant test coverage findings —
/// private helpers get tested transitively through their public callers.
pub(crate) fn is_testable_visibility(method: &str, visibility: &HashMap<String, String>) -> bool {
    match visibility.get(method).map(|s| s.as_str()) {
        Some("public") | Some("pub(crate)") | Some("pub(super)") => true,
        Some("private") => false,
        // If visibility is unknown (not in the map), assume testable
        None => true,
        Some(_) => true,
    }
}

/// Check if a source file should be excluded from test coverage checks
/// based on the skip_patterns config.
pub(crate) fn is_skipped_path(path: &str, config: &TestMappingConfig) -> bool {
    config
        .skip_test_patterns
        .iter()
        .any(|pattern| path.contains(pattern))
}
