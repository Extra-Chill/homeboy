//! Test-path classification primitive.
//!
//! `is_test_path` heuristically classifies whether a relative path points to a
//! test file (test directories, `*_test.rs` / `*Test.php` / `*.spec.ts`
//! filenames, etc.). It is a pure, language-agnostic string classifier with no
//! dependencies, consumed by both `code_audit` and `extension` — so it lives in
//! the primitives crate to keep those two feature layers off each other.

/// Check if a relative path points to a test file using heuristic patterns.
///
/// Used to separate test files from production code during convention discovery,
/// preventing test methods (set_up, tear_down) from contaminating production
/// conventions and preventing production conventions from generating false
/// positives in test files.
///
/// Matches common test file patterns across languages:
/// - Paths under `tests/`, `Tests/`, `test/`, `__tests__/` directories
/// - Files named `*_test.rs`, `*Test.php`, `*.test.js`, `*.spec.ts`, etc.
pub fn is_test_path(relative_path: &str) -> bool {
    // Directory-based detection
    let path_lower = relative_path.to_lowercase();
    if path_lower.starts_with("tests/")
        || path_lower.starts_with("test/")
        || path_lower.starts_with("__tests__/")
        || path_lower.contains("/tests/")
        || path_lower.contains("/test/")
        || path_lower.contains("/__tests__/")
    {
        return true;
    }

    // Filename-based detection (case-sensitive for precision)
    let file_name = relative_path.rsplit('/').next().unwrap_or(relative_path);

    // Rust: foo_test.rs, foo_tests.rs, and bare test.rs / tests.rs modules
    // (conventionally wired as `#[cfg(test)] mod tests;`). Also cover shared
    // test-fixture modules — `*_fixture(s).rs` (e.g. test_fixture.rs) — which are
    // conventionally `#[cfg(test)] mod` fixtures consumed by sibling `*_tests.rs`
    // files, not production code.
    if file_name.ends_with("_test.rs")
        || file_name.ends_with("_tests.rs")
        || file_name.ends_with("_fixture.rs")
        || file_name.ends_with("_fixtures.rs")
        || file_name == "test.rs"
        || file_name == "tests.rs"
    {
        return true;
    }
    // PHP: FooTest.php
    if file_name.ends_with("Test.php") {
        return true;
    }
    // JS/TS: foo.test.js, foo.spec.js, foo.test.ts, foo.spec.ts (and jsx/tsx)
    for ext in &[
        ".test.js",
        ".test.jsx",
        ".test.ts",
        ".test.tsx",
        ".test.mjs",
        ".spec.js",
        ".spec.jsx",
        ".spec.ts",
        ".spec.tsx",
        ".spec.mjs",
    ] {
        if file_name.ends_with(ext) {
            return true;
        }
    }
    // Python: test_foo.py
    if file_name.starts_with("test_") && file_name.ends_with(".py") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_path_directory_patterns() {
        assert!(is_test_path("tests/core/audit.rs"));
        assert!(is_test_path("tests/Unit/FooTest.php"));
        assert!(is_test_path("test/helpers.js"));
        assert!(is_test_path("src/__tests__/foo.test.ts"));
        assert!(is_test_path("inc/Tests/Abilities/FooTest.php"));
        assert!(is_test_path("some/deep/path/tests/unit/bar.rs"));
    }

    #[test]
    fn test_is_test_path_filename_patterns() {
        assert!(is_test_path("src/core/audit_test.rs"));
        assert!(is_test_path("src/core/audit_tests.rs"));
        assert!(is_test_path("inc/Abilities/SystemAbilitiesTest.php"));
        assert!(is_test_path("src/components/Button.test.tsx"));
        assert!(is_test_path("src/utils/parse.spec.ts"));
        assert!(is_test_path("lib/test_runner.py"));
        assert!(is_test_path("src/commands/bench/tests.rs"));
        assert!(is_test_path("src/core/triage/test.rs"));
        assert!(is_test_path("src/commands/trace/test_fixture.rs"));
        assert!(is_test_path("src/core/runner/exec_fixtures.rs"));
    }

    #[test]
    fn test_is_test_path_negative() {
        assert!(!is_test_path("src/core/audit.rs"));
        assert!(!is_test_path("inc/Abilities/SystemAbilities.php"));
        assert!(!is_test_path("src/components/Button.tsx"));
        assert!(!is_test_path("src/utils/test_helpers.rs"));
        assert!(!is_test_path("src/testing/framework.rs"));
    }
}
