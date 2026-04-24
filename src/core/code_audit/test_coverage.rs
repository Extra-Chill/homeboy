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

/// Analyze test coverage gaps given source fingerprints and a test mapping config.
///
/// `root` is the component root directory (for resolving test file existence).
/// `fingerprints` are all fingerprinted source files from the audit pipeline.
/// `config` is the extension-provided test mapping convention.
pub(crate) fn analyze_test_coverage(
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
            // The core grammar engine extracts functions with `#[test]` into the
            // fingerprint's `test_methods` list (prefix normalized on). Reading
            // the structural list rather than filtering `.methods` by name is
            // what prevents production methods named `test_*` (like
            // `ExtensionManifest::test_script`) from being classified as tests
            // — see Extra-Chill/homeboy#1471.
            let has_inline_tests = !source_fp.test_methods.is_empty();

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

            // Inline test methods — authored with `#[test]`, tracked separately
            // from production methods so prefix-string ambiguity can't leak.
            for method in &source_fp.test_methods {
                if let Some(source_method) = method.strip_prefix(&config.method_prefix) {
                    covered_methods.insert(source_method);
                }
            }

            // Methods from dedicated test file — for Rust, these are also
            // structural (top-level `#[test]` functions in `tests/`). For
            // extension-fingerprinted languages the core list is empty so we
            // fall back to the prefix filter on `.methods`.
            let dedicated_test_methods: Vec<&str> = if let Some(test_fingerprint) = test_fp {
                collect_test_method_refs(test_fingerprint, config)
            } else if let Some(test_methods) = &disk_test_methods {
                test_methods
                    .iter()
                    .filter(|m| m.starts_with(&config.method_prefix))
                    .map(|m| m.as_str())
                    .collect()
            } else {
                Vec::new()
            };
            for method in dedicated_test_methods {
                if let Some(source_method) = method.strip_prefix(&config.method_prefix) {
                    covered_methods.insert(source_method);
                }
            }

            // Build set of source method names for orphaned test detection.
            // Test methods already live in `source_fp.test_methods` — `.methods`
            // contains only non-test functions, so no prefix filter needed.
            let source_methods: HashSet<&str> =
                source_fp.methods.iter().map(|m| m.as_str()).collect();

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
            //
            // We pass `source_fp.test_methods` directly rather than filtering
            // `source_fp.methods` by prefix. The detector historically used the
            // prefix filter and emitted source-file findings for production
            // methods named `test_*` — see Extra-Chill/homeboy#1471.
            find_orphaned_test_methods(
                &mut findings,
                &source_fp.relative_path,
                &source_fp.test_methods,
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

/// Load test methods from disk for a known test file path.
///
/// Uses extension fingerprinting when available, with a lightweight regex fallback
/// so singleton test files still contribute method coverage in scoped audits.
fn load_test_methods_from_disk(
    root: &Path,
    test_path: &str,
    config: &TestMappingConfig,
) -> Option<Vec<String>> {
    let abs = root.join(test_path);
    if !abs.exists() {
        return None;
    }

    if let Some(fp) = super::fingerprint::fingerprint_file(&abs, root) {
        if !fp.methods.is_empty() {
            return Some(fp.methods);
        }
    }

    let content = std::fs::read_to_string(&abs).ok()?;
    Some(extract_test_methods_fallback(
        &content,
        test_path,
        &config.method_prefix,
    ))
}

fn extract_test_methods_fallback(
    content: &str,
    test_path: &str,
    method_prefix: &str,
) -> Vec<String> {
    let ext = Path::new(test_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let escaped = regex::escape(method_prefix);
    let pattern = match ext {
        "rs" => format!(r"(?m)^\s*fn\s+({}\w*)\s*\(", escaped),
        "php" => format!(r"(?m)^\s*(?:public\s+)?function\s+({}\w*)\s*\(", escaped),
        "js" | "jsx" | "ts" | "tsx" => {
            format!(r"(?m)^\s*(?:async\s+)?function\s+({}\w*)\s*\(", escaped)
        }
        _ => format!(r"(?m)({}\w*)", escaped),
    };

    let re = match Regex::new(&pattern) {
        Ok(re) => re,
        Err(_) => return Vec::new(),
    };

    re.captures_iter(content)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Check if a source file matches any critical pattern.
fn is_critical(path: &str, config: &TestMappingConfig) -> bool {
    config
        .critical_patterns
        .iter()
        .any(|pattern| path.contains(pattern))
}

/// Trivial methods that don't warrant individual test coverage findings.
fn is_trivial_method(name: &str) -> bool {
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
fn is_testable_visibility(method: &str, visibility: &HashMap<String, String>) -> bool {
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
fn is_skipped_path(path: &str, config: &TestMappingConfig) -> bool {
    config
        .skip_test_patterns
        .iter()
        .any(|pattern| path.contains(pattern))
}

/// Collect test method names from a fingerprint.
///
/// Prefers the structural `test_methods` list populated by the core grammar
/// engine (Rust `#[test]` functions). Falls back to filtering `.methods` by
/// the configured test prefix for extension-script fingerprints (PHP/JS/TS)
/// that don't distinguish test functions structurally.
///
/// A production method named `test_foo()` in a source file is NOT considered
/// a test method by this helper, because only the structural list is consulted
/// when it's populated.
fn collect_test_methods_from_fp(fp: &FileFingerprint, config: &TestMappingConfig) -> Vec<String> {
    if !fp.test_methods.is_empty() {
        return fp.test_methods.clone();
    }
    fp.methods
        .iter()
        .filter(|m| m.starts_with(&config.method_prefix))
        .cloned()
        .collect()
}

/// Collect test method names as borrowed `&str`s (same semantics as
/// `collect_test_methods_from_fp`, borrowed for coverage-set building).
fn collect_test_method_refs<'a>(
    fp: &'a FileFingerprint,
    config: &TestMappingConfig,
) -> Vec<&'a str> {
    if !fp.test_methods.is_empty() {
        return fp.test_methods.iter().map(|m| m.as_str()).collect();
    }
    fp.methods
        .iter()
        .filter(|m| m.starts_with(&config.method_prefix))
        .map(|m| m.as_str())
        .collect()
}

/// Check 4: Orphaned test methods — test methods whose source method no longer exists.
///
/// For each `test_X` method, checks whether `X` exists in the source file's methods.
/// This catches tests left behind when a function is deleted from the source — the kind
/// of breakage that `#[cfg(test)]` hides from normal compilation.
fn find_orphaned_test_methods(
    findings: &mut Vec<Finding>,
    file_path: &str,
    test_methods: &[String],
    source_methods: &HashSet<&str>,
    config: &TestMappingConfig,
) {
    for test_method in test_methods {
        let Some(expected_source) = test_method.strip_prefix(&config.method_prefix) else {
            continue;
        };

        // Skip if the test doesn't follow the naming convention (no source method implied)
        if expected_source.is_empty() {
            continue;
        }

        // If the source method exists (exact match), this test is valid
        if source_methods.contains(expected_source) {
            continue;
        }

        // Prefix match: test_compare_detects_new_drift should match source method
        // "compare" because it's testing a specific scenario of that method.
        // We require the prefix to be followed by '_' to prevent false matches
        // (e.g., test_parse_this should not match source method "par").
        let has_prefix_match = source_methods.iter().any(|source_method| {
            let prefix = format!("{}_", source_method);
            expected_source.starts_with(&prefix)
        });
        if has_prefix_match {
            continue;
        }

        // Behavior-driven test names: test_detects_exact_duplicate describes a
        // behavior, not a method reference. Short names (1-2 segments like
        // "old_function" or "pause") are likely real method references. But
        // longer compound names (3+ segments like "detects_exact_duplicate" or
        // "apply_replace_text") are usually behavioral/scenario descriptions.
        //
        // For 3+ segment names, we skip (treat as behavioral) UNLESS the
        // expected source name is a direct prefix of a source method. This
        // prevents false positives where common verbs like "apply", "resolve",
        // "build" happen to match source methods (e.g., "apply" matching
        // "apply_edit_ops" would falsely flag "apply_replace_text" as orphaned).
        let segment_count = expected_source.split('_').count();
        if segment_count >= 3 {
            // Only flag if the expected source is itself a prefix of some source
            // method (suggesting it really names a method that was truncated or
            // renamed). A single first-word match is too loose.
            let is_direct_prefix_of_source = source_methods.iter().any(|m| {
                // "apply_edit_ops" would match source "apply_edit_ops_to_content"
                m.starts_with(&format!("{}_", expected_source))
            });
            if !is_direct_prefix_of_source {
                continue;
            }
        }

        findings.push(Finding {
            convention: "test_coverage".to_string(),
            severity: Severity::Warning,
            file: file_path.to_string(),
            description: format!(
                "Test method '{}' references '{}' which no longer exists in the source",
                test_method, expected_source
            ),
            suggestion: format!(
                "Remove the orphaned test '{}' or rename it to match an existing method",
                test_method
            ),
            kind: AuditFinding::OrphanedTest,
        });
    }
}

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

    /// Build a fingerprint for tests.
    ///
    /// For **source-file paths** (not under `tests/`), methods that start
    /// with the conventional test prefix are split into `test_methods`,
    /// mirroring what the core grammar engine does with `#[test]` functions.
    /// This preserves backwards-compatibility with existing fixtures that mix
    /// source methods and inline test methods into a single vec.
    ///
    /// For **test-file paths** (under `tests/` / `__tests__/` / matching
    /// language-specific test suffixes), all methods stay in `.methods`. This
    /// mirrors the extension-script fingerprint protocol (PHP/JS/TS) which
    /// does not distinguish `#[test]`-attributed functions.
    ///
    /// For tests that need to model the specific case "production method with
    /// a `test_` prefixed name" (e.g. `ExtensionManifest::test_script`), use
    /// `make_fp_split` and pass an empty `test_methods` vec.
    fn make_fp(path: &str, methods: Vec<&str>) -> FileFingerprint {
        let mut fp = FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            ..Default::default()
        };
        let is_test_file = crate::core::code_audit::walker::is_test_path(path);
        for m in methods {
            if !is_test_file && m.starts_with("test_") {
                fp.test_methods.push(m.to_string());
            } else {
                fp.methods.push(m.to_string());
            }
        }
        fp
    }

    /// Build a fingerprint with explicit `methods` and `test_methods` splits.
    fn make_fp_split(path: &str, methods: Vec<&str>, test_methods: Vec<&str>) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.into_iter().map(String::from).collect(),
            test_methods: test_methods.into_iter().map(String::from).collect(),
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
        //        test_discover_stale_data (behavioral — 3+ segments, "discover_stale_data"
        //          is not a direct prefix of any source method → treated as behavioral),
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
                "test_discover_stale_data",    // behavioral — 3+ segments, not a direct prefix
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
            1,
            "Should detect 1 orphaned test method (load_config), found: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
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
    fn behavioral_test_names_not_flagged_as_orphaned() {
        // Regression: test_helpers_without_test_attr_not_counted_as_test_methods
        // was flagged as orphaned. The behavior-driven heuristic should skip
        // test names with 3+ segments where the first word doesn't match any
        // source method.
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_behavioral");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        let source = make_fp(
            "src/core/engine.rs",
            vec![
                "fingerprint_from_grammar",
                "extract_functions",
                "exact_hash",
                // Behavioral test names — these should NOT be flagged
                "test_helpers_without_test_attr_not_counted_as_test_methods",
                "test_replace_string_literals",
                "test_exact_hash_deterministic",
            ],
        );

        let findings = analyze_test_coverage(&dir, &[&source], &config);

        let orphaned: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::OrphanedTest && f.description.contains("no longer exists")
            })
            .collect();

        // test_replace_string_literals → "replace_string_literals" — 3 segments,
        // not a direct prefix of any source method → skip (behavioral)
        //
        // test_exact_hash_deterministic → "exact_hash_deterministic" — 3 segments,
        // not a direct prefix of any source method → skip (behavioral).
        // Even though "exact_hash" is a source method, "exact_hash_deterministic"
        // is a scenario description, not a method reference.
        //
        // test_helpers_without_test_attr_not_counted_as_test_methods → 9 segments,
        // not a direct prefix of any source method → skip (behavioral)

        let orphaned_names: Vec<String> = orphaned.iter().map(|f| f.description.clone()).collect();
        assert!(
            !orphaned_names.iter().any(|d| d.contains("helpers_without")),
            "Behavioral test name should NOT be flagged as orphaned. Orphaned: {:?}",
            orphaned_names
        );
        assert!(
            !orphaned_names.iter().any(|d| d.contains("replace_string")),
            "Behavioral test name should NOT be flagged as orphaned. Orphaned: {:?}",
            orphaned_names
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scenario_test_names_not_flagged_as_orphaned() {
        // Regression for #1120 / PR #1119: tests like "apply_replace_text" were
        // flagged as orphaned because the first word "apply" matched source
        // method "apply_edit_ops". These are scenario/behavioral tests for
        // apply_edit_ops_to_content(), not references to a deleted method.
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_scenario");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core/engine")).unwrap();

        let source = make_fp(
            "src/core/engine/edit_op_apply.rs",
            vec![
                "resolve_anchor",
                "apply_edit_ops_to_content",
                "apply_edit_ops",
                "remove_from_reexport_block",
                // Scenario tests — none of these should be flagged
                "test_apply_replace_text",
                "test_apply_replace_text_not_found_errors",
                "test_apply_replace_text_line_out_of_range",
                "test_apply_remove_lines",
                "test_apply_insert_lines_at_line",
                "test_apply_insert_lines_after_imports",
                "test_apply_insert_lines_file_end",
                "test_apply_reexport_removal",
                "test_apply_multiple_ops_same_file",
                "test_apply_multiple_removals_bottom_to_top",
                "test_apply_combined_remove_and_insert",
                "test_resolve_anchor_at_line",
                "test_resolve_anchor_file_top",
                "test_resolve_anchor_after_imports_rust",
            ],
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
            "Scenario test names should NOT be flagged as orphaned. Flagged: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn production_method_with_test_prefix_not_flagged_orphaned() {
        // Regression for Extra-Chill/homeboy#1471: `ExtensionManifest::test_script()`
        // and `test_mapping()` are production accessors on a manifest struct —
        // public methods whose names happen to start with `test_`. They are
        // NOT `#[test]` functions. The detector used to flag them as orphaned
        // because `collect_test_methods_from_fp` filtered `.methods` by name
        // prefix, ignoring the structural `has_test_attr` signal. The
        // generator then auto-deleted them. Bug occurred three times in 26
        // hours (#1176 → #1183 → bench PR #1385 force-push reverts) until
        // this fix split test methods into their own `test_methods` vec.
        let config = make_rust_config();
        let dir = std::env::temp_dir().join("homeboy_test_coverage_prod_test_prefix");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/core/extension")).unwrap();

        // Model ExtensionManifest: production methods with `test_` names, no
        // inline tests. `test_methods` is empty (these are NOT #[test]).
        let source = make_fp_split(
            "src/core/extension/manifest.rs",
            vec![
                "lint_script",
                "build_script",
                "test_script",  // production accessor, looks like a test prefix
                "test_mapping", // production accessor, looks like a test prefix
                "autofix_verify",
            ],
            vec![], // no inline #[test] functions
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
            "Production methods named test_* must not be flagged as orphaned tests. \
             Flagged: {:?}",
            orphaned.iter().map(|f| &f.description).collect::<Vec<_>>()
        );

        // They should also not show up as missing-test findings for the
        // *nonexistent* source methods `script` / `mapping`.
        let missing_methods_referencing_stub: Vec<&Finding> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::MissingTestMethod
                    && (f.description.contains("'script'") || f.description.contains("'mapping'"))
            })
            .collect();
        assert!(
            missing_methods_referencing_stub.is_empty(),
            "test_script and test_mapping must not be interpreted as covering \
             source methods named 'script' / 'mapping'. Found: {:?}",
            missing_methods_referencing_stub
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
