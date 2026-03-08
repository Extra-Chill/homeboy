use clap::Args;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy::component::Component;
use homeboy::git;
use homeboy::test_drift::{self, DriftOptions};

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct TestScopeArgs {
    /// Git ref to compare against when computing changed test scope.
    #[arg(long, value_name = "REF", default_value = "HEAD~10")]
    pub since: String,
}

#[derive(Serialize)]
#[allow(dead_code)]
pub struct TestScopeCommandOutput {
    pub status: String,
    pub changed_since: String,
}

#[allow(dead_code)]
pub fn run(args: TestScopeArgs, _global: &GlobalArgs) -> CmdResult<TestScopeCommandOutput> {
    Ok((
        TestScopeCommandOutput {
            status: "ready".to_string(),
            changed_since: args.since,
        },
        0,
    ))
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::component::Component;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn test_run() {
        let (output, exit_code) = run(
            TestScopeArgs {
                since: "HEAD~5".to_string(),
            },
            &GlobalArgs {},
        )
        .expect("run should succeed");

        assert_eq!(exit_code, 0);
        assert_eq!(output.status, "ready");
        assert_eq!(output.changed_since, "HEAD~5");
    }

    #[test]
    fn test_build_phpunit_filter_regex() {
        let selected = vec![
            "tests/Unit/Foo/BarBazTest.php".to_string(),
            "tests/Unit/Foo/BatTest.php".to_string(),
            "tests/core/thing_test.rs".to_string(),
        ];

        let regex = build_phpunit_filter_regex(&selected);
        assert_eq!(regex, "(BarBazTest|BatTest)");
    }

    #[test]
    fn test_is_test_path() {
        assert!(is_test_path("tests/unit/foo_test.rs"));
        assert!(is_test_path("plugin/tests/FooTest.php"));
        assert!(!is_test_path("src/core/component.rs"));
    }

    #[test]
    fn test_compute_changed_test_scope() {
        let dir = TempDir::new().expect("temp dir should be created");
        let root = dir.path();

        fs::create_dir_all(root.join("src")).expect("src dir should be created");
        fs::create_dir_all(root.join("tests")).expect("tests dir should be created");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='scope-test'\nversion='0.1.0'\n",
        )
        .expect("Cargo.toml should be written");
        fs::write(root.join("src/lib.rs"), "pub fn thing() {}\n").expect("lib should be written");

        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init should run");
        Command::new("git")
            .args(["config", "user.email", "tests@example.com"])
            .current_dir(root)
            .output()
            .expect("git config email should run");
        Command::new("git")
            .args(["config", "user.name", "Tests"])
            .current_dir(root)
            .output()
            .expect("git config name should run");
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .output()
            .expect("git add should run");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("git commit should run");

        fs::write(root.join("tests/scope_test.rs"), "#[test]\nfn smoke(){}\n")
            .expect("test file should be written");
        Command::new("git")
            .args(["add", "tests/scope_test.rs"])
            .current_dir(root)
            .output()
            .expect("git add test file should run");
        Command::new("git")
            .args(["commit", "-m", "add test"])
            .current_dir(root)
            .output()
            .expect("git commit test file should run");

        let component = Component::new(
            "scope-test".to_string(),
            root.to_string_lossy().to_string(),
            "/tmp/remote".to_string(),
            None,
        );

        let output = compute_changed_test_scope(&component, "HEAD~1")
            .expect("scope computation should succeed");

        assert_eq!(output.mode, "changed");
        assert_eq!(output.changed_since, Some("HEAD~1".to_string()));
        assert!(
            output
                .selected_files
                .iter()
                .any(|f| f.ends_with("tests/scope_test.rs")),
            "expected changed test file to be included"
        );
    }
}
