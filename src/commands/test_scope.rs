use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy::component::Component;
use homeboy::git;
use homeboy::test_drift::{self, DriftOptions};

#[derive(Clone, Serialize)]
pub struct TestScopeOutput {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
    pub selected_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected_files: Vec<String>,
}

pub fn compute_changed_test_scope(
    component: &Component,
    git_ref: &str,
) -> homeboy::error::Result<TestScopeOutput> {
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        PathBuf::from(expanded.as_ref())
    };

    let changed_files = git::get_files_changed_since(&source_path.to_string_lossy(), git_ref)?;

    let opts = if source_path.join("Cargo.toml").exists() {
        DriftOptions::rust(&source_path, git_ref)
    } else {
        DriftOptions::php(&source_path, git_ref)
    };

    let report = test_drift::detect_drift(&component.id, &opts)?;

    let mut selected: BTreeSet<String> = BTreeSet::new();

    // Include directly changed test files
    for file in &changed_files {
        if is_test_path(file) {
            selected.insert(file.clone());
        }
    }

    // Include drift-detected impacted test files
    for drifted in &report.drifted_tests {
        selected.insert(drifted.test_file.clone());
    }

    let selected_files: Vec<String> = selected.into_iter().collect();

    Ok(TestScopeOutput {
        mode: "changed".to_string(),
        changed_since: Some(git_ref.to_string()),
        selected_count: selected_files.len(),
        selected_files,
    })
}

pub fn build_phpunit_filter_regex(selected_files: &[String]) -> String {
    // Build a regex that matches test class names derived from selected file basenames.
    // Example: tests/Unit/Foo/BarBazTest.php -> BarBazTest
    let mut classes: Vec<String> = selected_files
        .iter()
        .filter_map(|f| {
            if !f.ends_with(".php") {
                return None;
            }
            Path::new(f)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .filter(|stem| !stem.is_empty())
        .map(|stem| regex::escape(&stem))
        .collect();

    classes.sort();
    classes.dedup();

    if classes.is_empty() {
        // No PHP class-based test files selected. Use a non-matching regex
        // to avoid accidentally running the full suite.
        return "^$".to_string();
    }

    format!("({})", classes.join("|"))
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/") || path.ends_with("Test.php") || path.ends_with("_test.rs")
}
