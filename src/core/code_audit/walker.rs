//! walker — extracted from conventions.rs.

use std::path::Path;

/// Extension index/entry-point filenames that should be excluded from convention
/// sibling detection. These files organize other files rather than being
/// peers — including them produces false "missing method" findings.
pub(crate) const INDEX_FILES: &[&str] = &[
    "mod.rs",
    "lib.rs",
    "main.rs",
    "index.js",
    "index.jsx",
    "index.ts",
    "index.tsx",
    "index.mjs",
    "__init__.py",
];

/// Returns true if the filename is a extension index/entry-point file.
pub(crate) fn is_index_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| INDEX_FILES.contains(&name))
        .unwrap_or(false)
}

/// Walk source files under a root, skipping common non-source directories
/// and extension index files.
/// Collect all file extensions that installed extension extensions can handle.
pub(crate) fn extension_provided_file_extensions() -> Vec<String> {
    crate::extension::load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|m| m.provided_file_extensions().to_vec())
        .collect()
}

pub(crate) fn walk_source_files(root: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let skip_dirs = [
        "node_modules",
        "vendor",
        ".git",
        "build",
        "dist",
        "target",
        ".svn",
        ".hg",
        "cache",
        "tmp",
    ];
    let dynamic_extensions = extension_provided_file_extensions();
    let source_extensions: Vec<&str> = dynamic_extensions.iter().map(|s| s.as_str()).collect();

    let mut files = Vec::new();
    walk_recursive(root, &skip_dirs, &source_extensions, &mut files)?;

    // Exclude extension index files from convention sibling detection
    files.retain(|f| !is_index_file(f));

    Ok(files)
}

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
pub(crate) fn is_test_path(relative_path: &str) -> bool {
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

    // Rust: foo_test.rs, foo_tests.rs
    if file_name.ends_with("_test.rs") || file_name.ends_with("_tests.rs") {
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

/// Known source file extensions that may be present even if no extension claims them.
const COMMON_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "php", "js", "ts", "py", "go", "java", "rb", "swift", "kt", "c", "cpp", "h",
];

/// Count source files that exist in the tree but aren't claimed by any extension.
/// Used to warn when no extension provides fingerprinting for the dominant language.
pub(crate) fn count_unclaimed_source_files(root: &Path) -> usize {
    let skip_dirs = [
        "node_modules",
        "vendor",
        ".git",
        "build",
        "dist",
        "target",
        ".svn",
        ".hg",
        "cache",
        "tmp",
    ];
    let claimed = extension_provided_file_extensions();

    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if !skip_dirs.contains(&name.as_str()) {
                        stack.push(path);
                    }
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if COMMON_SOURCE_EXTENSIONS.contains(&ext) && !claimed.iter().any(|c| c == ext)
                    {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

pub(crate) fn walk_recursive(
    dir: &Path,
    skip_dirs: &[&str],
    extensions: &[&str],
    files: &mut Vec<std::path::PathBuf>,
) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !skip_dirs.contains(&name.as_str()) {
                walk_recursive(&path, skip_dirs, extensions, files)?;
            }
        } else if let Some(ext) = path.extension() {
            if extensions.contains(&ext.to_str().unwrap_or("")) {
                files.push(path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_path_directory_patterns() {
        // Directory-based detection
        assert!(is_test_path("tests/core/audit.rs"));
        assert!(is_test_path("tests/Unit/FooTest.php"));
        assert!(is_test_path("test/helpers.js"));
        assert!(is_test_path("src/__tests__/foo.test.ts"));
        assert!(is_test_path("inc/Tests/Abilities/FooTest.php"));
        assert!(is_test_path("some/deep/path/tests/unit/bar.rs"));
    }

    #[test]
    fn test_is_test_path_filename_patterns() {
        // Filename-based detection
        assert!(is_test_path("src/core/audit_test.rs"));
        assert!(is_test_path("src/core/audit_tests.rs"));
        assert!(is_test_path("inc/Abilities/SystemAbilitiesTest.php"));
        assert!(is_test_path("src/components/Button.test.tsx"));
        assert!(is_test_path("src/utils/parse.spec.ts"));
        assert!(is_test_path("lib/test_runner.py"));
    }

    #[test]
    fn test_is_test_path_negative() {
        // These should NOT be detected as test files
        assert!(!is_test_path("src/core/audit.rs"));
        assert!(!is_test_path("inc/Abilities/SystemAbilities.php"));
        assert!(!is_test_path("src/components/Button.tsx"));
        assert!(!is_test_path("src/utils/test_helpers.rs")); // helper, not a test
        assert!(!is_test_path("src/testing/framework.rs")); // "testing" != "tests"
    }
}
