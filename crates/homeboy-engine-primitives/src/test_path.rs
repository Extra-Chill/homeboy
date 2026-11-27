//! Test-path classification primitive.
//!
//! `is_test_path` heuristically classifies whether a relative path points to a
//! test file (test directories, `*_test.rs` / `*Test.php` / `*.spec.ts`
//! filenames, etc.). It is a pure, language-agnostic string classifier with no
//! dependencies, consumed by changed-test selection, so it lives in the
//! primitives crate.

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
    fn is_test_path_matches_test_directories_and_filenames() {
        for (path, expected) in [
            // Directory patterns.
            ("tests/core/audit.rs", true),
            ("test/helpers.js", true),
            ("src/__tests__/foo.test.ts", true),
            ("inc/Tests/Abilities/FooTest.php", true),
            ("some/deep/path/tests/unit/bar.rs", true),
            // Filename patterns.
            ("src/core/audit_test.rs", true),
            ("src/core/audit_tests.rs", true),
            ("inc/Abilities/SystemAbilitiesTest.php", true),
            ("src/components/Button.test.tsx", true),
            ("src/utils/parse.spec.ts", true),
            ("lib/test_runner.py", true),
            ("src/commands/bench/tests.rs", true),
            ("src/core/triage/test.rs", true),
            ("src/commands/trace/test_fixture.rs", true),
            ("src/core/runner/exec_fixtures.rs", true),
            // Production paths.
            ("src/core/audit.rs", false),
            ("inc/Abilities/SystemAbilities.php", false),
            ("src/components/Button.tsx", false),
            ("src/utils/test_helpers.rs", false),
            ("src/testing/framework.rs", false),
        ] {
            assert_eq!(is_test_path(path), expected, "{path}");
        }
    }
}
