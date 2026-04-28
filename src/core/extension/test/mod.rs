pub mod analyze;
pub mod baseline;
pub mod drift;
pub mod parsing;
pub mod report;
pub mod run;
pub mod workflow;

use crate::component::Component;
use crate::extension::test::drift::DriftOptions;
use crate::extension::{ExtensionCapability, ExtensionExecutionContext, ExtensionRunner};
use crate::git;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct TestScopeOutput {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
    pub selected_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub selected_files: Vec<String>,
}

pub use analyze::{FailureCategory, FailureCluster, TestAnalysis, TestAnalysisInput, TestFailure};
pub use baseline::{
    compare as compare_baseline, load_baseline, load_baseline_from_ref, save_baseline,
    TestBaseline, TestBaselineComparison, TestCounts,
};
pub use drift::{ChangeType, DriftReport, DriftedTest, ProductionChange};
pub use parsing::{
    build_test_summary, parse_coverage_file, parse_failures_file, parse_test_results_file,
    parse_test_results_text, parse_test_results_text_with_spec, CoverageOutput, TestSummaryOutput,
};
pub use report::{FailedTest, TestCommandOutput};
pub use run::{
    run_main_test_workflow, run_self_check_test_workflow, RawTestOutput, TestRunWorkflowArgs,
    TestRunWorkflowResult,
};
pub use workflow::{
    auto_fix_test_drift, detect_test_drift, AutoFixDriftOutput, AutoFixDriftWorkflowResult,
    DriftWorkflowResult, MainTestWorkflowResult,
};

pub fn resolve_test_command(
    component: &Component,
) -> crate::error::Result<ExtensionExecutionContext> {
    crate::extension::resolve_execution_context(component, ExtensionCapability::Test)
}

#[allow(clippy::too_many_arguments)]
pub fn build_test_runner(
    component: &Component,
    path_override: Option<String>,
    settings: &[(String, String)],
    skip_lint: bool,
    coverage_enabled: bool,
    coverage_min: Option<f64>,
    changed_test_files: Option<&[String]>,
    run_dir: &crate::engine::run_dir::RunDir,
) -> crate::Result<ExtensionRunner> {
    let resolved = resolve_test_command(component)?;

    let mut runner = ExtensionRunner::for_context(resolved)
        .component(component.clone())
        .path_override(path_override)
        .settings(settings)
        .with_run_dir(run_dir)
        .env_if(skip_lint, "HOMEBOY_SKIP_LINT", "1")
        .env_if(coverage_enabled, "HOMEBOY_COVERAGE", "1");

    if let Some(min) = coverage_min {
        runner = runner.env("HOMEBOY_COVERAGE_MIN", &format!("{}", min));
    }

    if let Some(files) = changed_test_files {
        runner = runner.env("HOMEBOY_CHANGED_TEST_FILES", &files.join("\n"));
    }

    Ok(runner)
}

fn component_source_path(component: &Component) -> PathBuf {
    let expanded = shellexpand::tilde(&component.local_path);
    PathBuf::from(expanded.as_ref())
}

/// Resolve drift detection options from the component's linked test extension.
///
/// `test.drift` is the primary contract. Installed extensions that only have
/// the older `audit.test_mapping` shape still work through the manifest
/// accessor fallback.
pub fn resolve_drift_options(
    component: &Component,
    since: &str,
) -> crate::error::Result<DriftOptions> {
    let source_path = component_source_path(component);

    if let Some(extensions) = &component.extensions {
        for extension_id in extensions.keys() {
            let manifest = crate::extension::load_extension(extension_id)?;
            if let Some(config) = manifest.test_drift() {
                return Ok(DriftOptions::from_config(
                    &source_path,
                    since,
                    &config,
                    manifest.provided_file_extensions(),
                ));
            }
        }
    }

    Ok(DriftOptions::php(&source_path, since))
}

/// Compute which test files are impacted by changes since a git ref.
///
/// Combines two sources: (1) changed files that are test paths, and
/// (2) test files flagged by drift detection as needing re-runs.
/// This is the single source of truth — used by test scope, refactor
/// planning, and verification smoke tests.
pub fn compute_changed_test_files(
    component: &Component,
    git_ref: &str,
) -> crate::error::Result<Vec<String>> {
    let source_path = component_source_path(component);

    let changed_files = git::get_files_changed_since(&source_path.to_string_lossy(), git_ref)?;

    let opts = resolve_drift_options(component, git_ref)?;

    let report = drift::detect_drift(&component.id, &opts)?;
    let mut selected: BTreeSet<String> = BTreeSet::new();

    for file in &changed_files {
        if crate::code_audit::is_test_path(file) {
            selected.insert(file.clone());
        }
    }

    for drifted in &report.drifted_tests {
        selected.insert(drifted.test_file.clone());
    }

    Ok(selected.into_iter().collect())
}

/// Compute changed test scope with metadata for command-layer output.
///
/// Wraps [`compute_changed_test_files`] with the `TestScopeOutput` envelope
/// that the test command uses for JSON output.
pub fn compute_changed_test_scope(
    component: &Component,
    git_ref: &str,
) -> crate::error::Result<TestScopeOutput> {
    let selected_files = compute_changed_test_files(component, git_ref)?;

    Ok(TestScopeOutput {
        mode: "changed".to_string(),
        changed_since: Some(git_ref.to_string()),
        selected_count: selected_files.len(),
        selected_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::TestDriftConfig;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn drift_options_use_extension_drift_config() {
        let dir = TempDir::new().expect("temp dir should be created");
        let config = TestDriftConfig {
            source_dirs: vec!["src".to_string(), "inc".to_string()],
            test_dirs: vec!["tests".to_string()],
            file_extensions: vec!["php".to_string()],
            inline_tests: false,
        };

        let opts = DriftOptions::from_config(dir.path(), "HEAD~1", &config, &["rs".to_string()]);

        assert_eq!(opts.source_patterns, vec!["src/**/*.php", "inc/**/*.php"]);
        assert_eq!(opts.test_patterns, vec!["tests/**/*.php"]);
    }

    #[test]
    fn drift_options_fall_back_to_extension_file_extensions() {
        let dir = TempDir::new().expect("temp dir should be created");
        let config = TestDriftConfig {
            source_dirs: vec!["src".to_string()],
            test_dirs: vec!["tests".to_string()],
            file_extensions: Vec::new(),
            inline_tests: true,
        };

        let opts = DriftOptions::from_config(dir.path(), "HEAD~1", &config, &["rs".to_string()]);

        assert_eq!(opts.source_patterns, vec!["src/**/*.rs"]);
        assert_eq!(opts.test_patterns, vec!["tests/**/*.rs"]);
    }

    #[test]
    fn compute_changed_test_scope_detects_new_test_file() {
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
