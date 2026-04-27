use std::fs;
use std::path::{Path, PathBuf};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};

pub(crate) fn run(root: &Path) -> Vec<Finding> {
    let tests_dir = root.join("tests");
    if !tests_dir.is_dir() {
        return Vec::new();
    }

    let source_text = collect_source_text(&root.join("src"));
    let mut findings = Vec::new();

    for path in collect_rs_files(&tests_dir) {
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = normalize_path(relative);

        if !is_nested_rust_test_file(&relative) || is_wired(&relative, &source_text) {
            continue;
        }

        findings.push(Finding {
            convention: "Rust test discovery".to_string(),
            severity: Severity::Warning,
            file: relative.clone(),
            description: format!(
                "Nested Rust test file `{}` is not wired into Cargo's test harness",
                relative
            ),
            suggestion: format!(
                "Wire `{}` from the covered source module with `#[cfg(test)] #[path = \"...\"] mod ...;`, or move it to top-level `tests/*.rs` if it should be a Cargo integration test.",
                relative
            ),
            kind: AuditFinding::UnwiredNestedRustTest,
        });
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file));
    findings
}

fn is_nested_rust_test_file(relative: &str) -> bool {
    if !relative.starts_with("tests/") || !relative.ends_with("_test.rs") {
        return false;
    }

    // Cargo auto-discovers only direct children like `tests/foo_test.rs`.
    // Nested files need an explicit `#[path = "..."] mod ...;` include.
    relative.trim_start_matches("tests/").contains('/')
}

fn is_wired(relative: &str, source_text: &str) -> bool {
    // Existing Homeboy convention wires nested tests by embedding the relative
    // path in a `#[path = "../../../tests/..._test.rs"]` attribute.
    source_text.contains(relative)
}

fn collect_source_text(src_dir: &Path) -> String {
    let mut text = String::new();
    for path in collect_rs_files(src_dir) {
        if let Ok(content) = fs::read_to_string(path) {
            text.push_str(&content);
            text.push('\n');
        }
    }
    text
}

fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rs_files_into(dir, &mut files);
    files.sort();
    files
}

fn collect_rs_files_into(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_into(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write fixture");
    }

    #[test]
    fn flags_nested_test_file_not_referenced_from_src() {
        let dir = TempDir::new().expect("tempdir");
        write(&dir.path().join("src/core/foo.rs"), "pub fn foo() {}\n");
        write(
            &dir.path().join("tests/core/foo_test.rs"),
            "#[test] fn works() {}\n",
        );

        let findings = run(dir.path());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "tests/core/foo_test.rs");
        assert_eq!(findings[0].kind, AuditFinding::UnwiredNestedRustTest);
    }

    #[test]
    fn accepts_nested_test_file_referenced_from_path_attribute() {
        let dir = TempDir::new().expect("tempdir");
        write(
            &dir.path().join("src/core/foo.rs"),
            "#[cfg(test)]\n#[path = \"../../tests/core/foo_test.rs\"]\nmod foo_test;\n",
        );
        write(
            &dir.path().join("tests/core/foo_test.rs"),
            "#[test] fn works() {}\n",
        );

        let findings = run(dir.path());

        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_top_level_cargo_integration_tests_and_support_modules() {
        let dir = TempDir::new().expect("tempdir");
        write(&dir.path().join("src/lib.rs"), "pub fn foo() {}\n");
        write(
            &dir.path().join("tests/api_jobs_test.rs"),
            "#[test] fn works() {}\n",
        );
        write(
            &dir.path().join("tests/core/support.rs"),
            "pub fn helper() {}\n",
        );

        let findings = run(dir.path());

        assert!(findings.is_empty());
    }
}
