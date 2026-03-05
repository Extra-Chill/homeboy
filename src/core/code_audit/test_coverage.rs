//! Structural test coverage gap detection — identify source files and methods
//! that lack corresponding tests.
//!
//! Plugs into the audit pipeline as Phase 4f. Uses the extension's
//! `TestMappingConfig` to understand how source files map to test files
//! and how source methods map to test methods.
//!
//! Performs three checks:
//! 1. Missing test files — source files with no corresponding test file
//! 2. Missing test methods — source methods with no corresponding test method
//! 3. Orphaned tests — test files/methods with no corresponding source

use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::Regex;

use super::conventions::DeviationKind;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use crate::extension::TestMappingConfig;

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
                        kind: DeviationKind::MissingTestFile,
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

            // Find source methods without tests
            for method in &source_fp.methods {
                if method.starts_with(&config.method_prefix) {
                    continue; // Skip test methods themselves
                }
                if is_trivial_method(method) {
                    continue;
                }
                if !covered_methods.contains(method.as_str()) {
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
                        kind: DeviationKind::MissingTestMethod,
                    });
                }
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
                        kind: DeviationKind::MissingTestFile,
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
                            kind: DeviationKind::MissingTestMethod,
                        });
                    }
                }
            }
        }
    }

    // Check 3: Orphaned tests — test files with no corresponding source file
    let source_paths: HashSet<&str> = source_fps
        .iter()
        .map(|fp| fp.relative_path.as_str())
        .collect();

    for test_fp in &test_fps {
        let expected_source_path = test_to_source_path(&test_fp.relative_path, config);

        if let Some(ref source_path) = expected_source_path {
            let source_exists =
                source_paths.contains(source_path.as_str()) || root.join(source_path).exists();

            if !source_exists {
                findings.push(Finding {
                    convention: "test_coverage".to_string(),
                    severity: Severity::Info,
                    file: test_fp.relative_path.clone(),
                    description: format!(
                        "Test file has no corresponding source file (expected '{}')",
                        source_path
                    ),
                    suggestion: "Remove the orphaned test or create the source file".to_string(),
                    kind: DeviationKind::OrphanedTest,
                });
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

/// Partition fingerprints into source files and test files based on the config.
fn partition_fingerprints<'a>(
    fingerprints: &[&'a FileFingerprint],
    config: &TestMappingConfig,
) -> (Vec<&'a FileFingerprint>, Vec<&'a FileFingerprint>) {
    let mut source = Vec::new();
    let mut test = Vec::new();

    for fp in fingerprints {
        if is_test_file(&fp.relative_path, config) {
            test.push(*fp);
        } else if is_source_file(&fp.relative_path, config) {
            source.push(*fp);
        }
    }

    (source, test)
}

/// Check if a file path is within one of the configured source directories.
fn is_source_file(path: &str, config: &TestMappingConfig) -> bool {
    config.source_dirs.iter().any(|dir| path.starts_with(dir)) || path.ends_with(".inc")
}

/// Check if a file path is within one of the configured test directories.
fn is_test_file(path: &str, config: &TestMappingConfig) -> bool {
    config.test_dirs.iter().any(|dir| path.starts_with(dir))
}

/// Convert a source file path to its expected test file path using the template.
///
/// Template variables: `{dir}` (relative dir within source_dir), `{name}` (stem), `{ext}` (extension).
fn source_to_test_path(source_path: &str, config: &TestMappingConfig) -> Option<String> {
    if source_path.ends_with(".inc") {
        return None;
    }

    // Find which source_dir this file is in
    let source_dir = config
        .source_dirs
        .iter()
        .find(|dir| source_path.starts_with(dir.as_str()))?;

    let relative = source_path.strip_prefix(source_dir)?;
    let relative = relative.strip_prefix('/').unwrap_or(relative);

    let path = Path::new(relative);
    let name = path.file_stem()?.to_str()?;
    let ext = path.extension()?.to_str()?;
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let test_path = config
        .test_file_pattern
        .replace("{dir}", &dir)
        .replace("{name}", name)
        .replace("{ext}", ext);

    // Clean up double slashes from empty {dir}
    let test_path = test_path.replace("//", "/");

    Some(test_path)
}

/// Convert a test file path back to its expected source file path.
/// This is the reverse of `source_to_test_path` — used for orphaned test detection.
fn test_to_source_path(test_path: &str, config: &TestMappingConfig) -> Option<String> {
    // Try each source_dir to see which one produces this test path
    // We use a heuristic: parse the test_file_pattern to understand the structure
    // and reverse-engineer the source path.

    // Parse the pattern to extract prefix, suffix around {name}
    let pattern = &config.test_file_pattern;

    // Find the test_dir prefix in the pattern
    let test_dir = config.test_dirs.first()?;

    // Strip the test dir from the test path
    let relative_in_test = if test_path.starts_with(test_dir.as_str()) {
        let stripped = test_path.strip_prefix(test_dir.as_str())?;
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        return None;
    };

    // Extract {dir} prefix from the pattern (everything before {name} in the test_dir-relative part)
    // e.g., pattern "tests/{dir}/{name}_test.{ext}" -> after "tests/" it's "{dir}/{name}_test.{ext}"
    let pattern_after_test_dir = if pattern.starts_with(test_dir.as_str()) {
        let stripped = pattern.strip_prefix(test_dir.as_str())?;
        stripped.strip_prefix('/').unwrap_or(stripped)
    } else {
        // Pattern might start with the test dir implicitly
        pattern.as_str()
    };

    // Find the {name} position in the pattern to figure out the test file naming convention
    let name_pos = pattern_after_test_dir.find("{name}")?;
    let _dir_part = &pattern_after_test_dir[..name_pos];
    let after_name = &pattern_after_test_dir[name_pos + 6..]; // skip "{name}"

    // after_name should be something like "_test.{ext}" or "Test.{ext}"
    // Replace {ext} with the actual extension
    let test_file_path = Path::new(relative_in_test);
    let test_ext = test_file_path.extension()?.to_str()?;
    let test_stem = test_file_path.file_stem()?.to_str()?;
    let test_dir_part = test_file_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Figure out the suffix that was appended to the source name
    // e.g., if after_name is "_test.{ext}", the suffix before .{ext} is "_test"
    let suffix_before_ext = after_name.strip_suffix(".{ext}").unwrap_or("");

    // Strip the suffix from the test stem to recover the source name
    let source_name = test_stem.strip_suffix(suffix_before_ext)?;

    // Reconstruct source path
    let source_dir = config.source_dirs.first()?;
    let source_path = if test_dir_part.is_empty() {
        format!("{}/{}.{}", source_dir, source_name, test_ext)
    } else {
        format!(
            "{}/{}/{}.{}",
            source_dir, test_dir_part, source_name, test_ext
        )
    };

    Some(source_path)
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
        "__construct",
        "__destruct",
        "__toString",
        "__clone",
        "get_instance",
        "getInstance",
    ];
    trivial.contains(&name)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::Language;
    use std::collections::HashMap;

    fn make_config() -> TestMappingConfig {
        TestMappingConfig {
            source_dirs: vec!["src".to_string()],
            test_dirs: vec!["tests".to_string()],
            test_file_pattern: "tests/{dir}/{name}_test.{ext}".to_string(),
            method_prefix: "test_".to_string(),
            inline_tests: false,
            critical_patterns: vec!["core/".to_string()],
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
        }
    }

    fn make_fp(path: &str, methods: Vec<&str>) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            methods: methods.into_iter().map(String::from).collect(),
            registrations: vec![],
            type_name: None,
            extends: None,
            implements: vec![],
            namespace: None,
            imports: vec![],
            content: String::new(),
            method_hashes: HashMap::new(),
            structural_hashes: HashMap::new(),
            visibility: HashMap::new(),
            properties: vec![],
            hooks: vec![],
            unused_parameters: vec![],
            dead_code_markers: vec![],
            internal_calls: vec![],
            public_api: vec![],
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
    fn include_fragment_is_source_but_has_no_direct_test_path() {
        let config = make_config();
        assert!(is_source_file("src/core/deploy/types.inc", &config));
        assert_eq!(
            source_to_test_path("src/core/deploy/types.inc", &config),
            None
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
        assert_eq!(findings[0].kind, DeviationKind::MissingTestFile);
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
            .filter(|f| f.kind == DeviationKind::MissingTestMethod)
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
            .filter(|f| f.kind == DeviationKind::OrphanedTest)
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
                f.kind == DeviationKind::MissingTestFile
                    || f.kind == DeviationKind::MissingTestMethod
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
            .filter(|f| f.kind == DeviationKind::MissingTestMethod)
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
            .filter(|f| f.kind == DeviationKind::MissingTestMethod)
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
                f.kind == DeviationKind::MissingTestFile
                    || f.kind == DeviationKind::MissingTestMethod
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
}
