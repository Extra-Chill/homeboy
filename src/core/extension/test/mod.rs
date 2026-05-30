pub mod analyze;
pub mod baseline;
pub mod drift;
pub mod parsing;
pub mod report;
pub mod run;
pub mod workflow;

use crate::core::component::Component;
use crate::core::extension::test::drift::DriftOptions;
use crate::core::extension::{
    ExtensionCapability, ExtensionExecutionContext, ExtensionRunner, TestChangedFileExclusiveEnv,
    TestChangedFileRouting, TestChangedFileRoutingStrategy,
};
use crate::core::git;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
) -> crate::core::error::Result<ExtensionExecutionContext> {
    crate::core::extension::resolve_execution_context(component, ExtensionCapability::Test)
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
    run_dir: &crate::core::engine::run_dir::RunDir,
) -> crate::core::Result<ExtensionRunner> {
    let resolved = resolve_test_command(component)?;
    let extension_id = resolved.extension_id.clone();

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

        if let Some(routing) = test_changed_file_routing(&extension_id)? {
            for (name, value) in build_changed_test_routing_env(component, files, &routing) {
                runner = runner.env(&name, &value);
            }
        }
    }

    Ok(runner)
}

fn test_changed_file_routing(
    extension_id: &str,
) -> crate::core::error::Result<Option<TestChangedFileRouting>> {
    let manifest = crate::core::extension::load_extension(extension_id)?;
    Ok(manifest
        .test
        .and_then(|test| test.changed_file_routing.clone()))
}

fn build_changed_test_routing_env(
    component: &Component,
    files: &[String],
    routing: &TestChangedFileRouting,
) -> Vec<(String, String)> {
    let mut env = vec![(
        "HOMEBOY_TEST_SCOPE_STRATEGY".to_string(),
        changed_test_strategy_name(&routing.strategy).to_string(),
    )];

    match routing.strategy {
        TestChangedFileRoutingStrategy::FileArgs => {
            env.push(("HOMEBOY_TEST_SCOPE_KIND".to_string(), "args".to_string()));
            env.push(("HOMEBOY_TEST_RUNNER_ARGS".to_string(), files.join("\n")));
        }
        TestChangedFileRoutingStrategy::RustCargo => {
            env.extend(rust_cargo_changed_test_env(component, files));
        }
        TestChangedFileRoutingStrategy::ExclusiveEnv => {
            if let Some(exclusive_env) = routing.exclusive_env.as_ref() {
                env.extend(exclusive_changed_test_env(files, exclusive_env));
            }
        }
    }

    env
}

fn changed_test_strategy_name(strategy: &TestChangedFileRoutingStrategy) -> &'static str {
    match strategy {
        TestChangedFileRoutingStrategy::FileArgs => "file_args",
        TestChangedFileRoutingStrategy::RustCargo => "rust_cargo",
        TestChangedFileRoutingStrategy::ExclusiveEnv => "exclusive_env",
    }
}

fn rust_cargo_changed_test_env(component: &Component, files: &[String]) -> Vec<(String, String)> {
    let mut filter_args = Vec::new();
    let mut integration_args = Vec::new();
    let mut fallback_message = None;

    for test_file in files {
        if test_file.starts_with("tests/") && test_file.ends_with(".rs") {
            let rest = test_file.trim_start_matches("tests/");
            if !rest.contains('/') {
                integration_args.push(rest.trim_end_matches(".rs").to_string());
                continue;
            }
        }

        let mut module_path = test_file
            .strip_prefix("src/")
            .unwrap_or(test_file)
            .strip_prefix("tests/")
            .unwrap_or_else(|| test_file.strip_prefix("src/").unwrap_or(test_file))
            .trim_end_matches(".rs")
            .trim_end_matches("/mod")
            .to_string();

        if test_file.starts_with("tests/") && test_file.ends_with("_test.rs") {
            let test_module = module_path
                .rsplit('/')
                .next()
                .unwrap_or(&module_path)
                .to_string();
            let source_module = module_path.trim_end_matches("_test");
            let source_file =
                component_source_path(component).join(format!("src/{source_module}.rs"));
            let source_mod =
                component_source_path(component).join(format!("src/{source_module}/mod.rs"));

            if source_file.is_file() || source_mod.is_file() {
                module_path = format!("{source_module}::{test_module}");
            } else if test_file.starts_with("tests/") && test_file["tests/".len()..].contains('/') {
                fallback_message = Some(
                    "Changed files include nested tests without a direct target; running the full test command."
                        .to_string(),
                );
                continue;
            }
        } else if test_file.starts_with("tests/") && test_file["tests/".len()..].contains('/') {
            fallback_message = Some(
                "Changed files include nested tests without a direct target; running the full test command."
                    .to_string(),
            );
            continue;
        }

        module_path = module_path.replace('/', "::");
        if !module_path.is_empty() {
            filter_args.push(module_path);
        }
    }

    if let Some(message) = fallback_message {
        return vec![
            ("HOMEBOY_TEST_SCOPE_KIND".to_string(), "full".to_string()),
            ("HOMEBOY_TEST_SCOPE_MESSAGE".to_string(), message),
        ];
    }

    if !integration_args.is_empty() && !filter_args.is_empty() {
        return vec![
            ("HOMEBOY_TEST_SCOPE_KIND".to_string(), "full".to_string()),
            (
                "HOMEBOY_TEST_SCOPE_MESSAGE".to_string(),
                "Changed files include integration and inline tests; running the full test command."
                    .to_string(),
            ),
        ];
    }

    if !integration_args.is_empty() {
        let args = integration_args
            .iter()
            .flat_map(|target| ["--test".to_string(), target.clone()])
            .collect::<Vec<_>>();
        return vec![
            (
                "HOMEBOY_TEST_SCOPE_KIND".to_string(),
                "rust_integration".to_string(),
            ),
            ("HOMEBOY_TEST_RUNNER_ARGS".to_string(), args.join("\n")),
            (
                "HOMEBOY_TEST_SCOPE_MESSAGE".to_string(),
                format!(
                    "Scoped to changed integration tests: {}",
                    integration_args.join(" ")
                ),
            ),
        ];
    }

    if filter_args.len() == 1 {
        return vec![
            (
                "HOMEBOY_TEST_SCOPE_KIND".to_string(),
                "rust_filter".to_string(),
            ),
            (
                "HOMEBOY_TEST_RUNNER_ARGS".to_string(),
                format!("--\n{}", filter_args[0]),
            ),
            (
                "HOMEBOY_TEST_SCOPE_MESSAGE".to_string(),
                format!("Scoped to changed files: {}", filter_args[0]),
            ),
        ];
    }

    if filter_args.len() > 1 {
        return vec![
            ("HOMEBOY_TEST_SCOPE_KIND".to_string(), "full".to_string()),
            (
                "HOMEBOY_TEST_SCOPE_MESSAGE".to_string(),
                "Changed files include multiple inline test modules; running the full test command."
                    .to_string(),
            ),
        ];
    }

    Vec::new()
}

fn exclusive_changed_test_env(
    files: &[String],
    config: &TestChangedFileExclusiveEnv,
) -> Vec<(String, String)> {
    if files.is_empty()
        || files
            .iter()
            .any(|file| !exclusive_route_matches(config, file))
    {
        return Vec::new();
    }

    vec![
        (
            "HOMEBOY_TEST_SCOPE_KIND".to_string(),
            "exclusive_env".to_string(),
        ),
        (
            "HOMEBOY_TEST_SCOPE_ENV_NAME".to_string(),
            config.name.clone(),
        ),
        ("HOMEBOY_TEST_SCOPE_ENV_VALUE".to_string(), files.join("\n")),
    ]
}

fn exclusive_route_matches(config: &TestChangedFileExclusiveEnv, file: &str) -> bool {
    if !config.extensions.is_empty() && has_extension(file, &config.extensions) {
        return true;
    }

    config
        .globs
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, file))
}

fn has_extension(file: &str, extensions: &[String]) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extensions.iter().any(|expected| expected == extension))
}

fn component_source_path(component: &Component) -> PathBuf {
    let expanded = shellexpand::tilde(&component.local_path);
    PathBuf::from(expanded.as_ref())
}

/// Resolve drift detection options from the component's linked test extension.
///
/// `test.drift` is the manifest contract for extension-owned drift behavior.
pub fn resolve_drift_options(
    component: &Component,
    since: &str,
) -> crate::core::error::Result<DriftOptions> {
    let source_path = component_source_path(component);

    if let Some(extensions) = &component.extensions {
        for extension_id in extensions.keys() {
            let manifest = crate::core::extension::load_extension(extension_id)?;
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
) -> crate::core::error::Result<Vec<String>> {
    let source_path = component_source_path(component);

    let changed_files = git::get_files_changed_since(&source_path.to_string_lossy(), git_ref)?;

    let opts = resolve_drift_options(component, git_ref)?;

    let report = drift::detect_drift(&component.id, &opts)?;
    let mut selected: BTreeSet<String> = BTreeSet::new();

    for file in &changed_files {
        if let Some(test_file) = changed_test_file_for_path(file) {
            selected.insert(test_file);
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
) -> crate::core::error::Result<TestScopeOutput> {
    let selected_files = compute_changed_test_files(component, git_ref)?;

    Ok(TestScopeOutput {
        mode: "changed".to_string(),
        changed_since: Some(git_ref.to_string()),
        selected_count: selected_files.len(),
        selected_files,
    })
}

fn changed_test_file_for_path(file: &str) -> Option<String> {
    if file == "tests/fixtures/refactor_transform_no_match.json" {
        return Some("tests/refactor_transform_test.rs".to_string());
    }

    if file.starts_with("tests/fixtures/output_contracts/errors/") {
        return Some("tests/output_errors.rs".to_string());
    }

    if file.starts_with("tests/fixtures/output_contracts/quality/") {
        return Some("tests/output_contracts_test.rs".to_string());
    }

    if file.starts_with("tests/fixtures/golden_json_contracts/") {
        return Some("src/commands/golden_contract_tests.rs".to_string());
    }

    if is_direct_changed_test_path(file) && !file.starts_with("tests/fixtures/") {
        return Some(file.to_string());
    }

    None
}

fn is_direct_changed_test_path(file: &str) -> bool {
    let path_lower = file.to_lowercase();
    if path_lower.starts_with("tests/")
        || path_lower.starts_with("test/")
        || path_lower.starts_with("__tests__/")
        || path_lower.contains("/tests/")
        || path_lower.contains("/__tests__/")
    {
        return true;
    }

    let file_name = file.rsplit('/').next().unwrap_or(file);
    file_name.ends_with("_test.rs")
        || file_name.ends_with("_tests.rs")
        || file_name.ends_with("Test.php")
        || file_name.ends_with(".test.js")
        || file_name.ends_with(".test.jsx")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".test.tsx")
        || file_name.ends_with(".test.mjs")
        || file_name.ends_with(".spec.js")
        || file_name.ends_with(".spec.jsx")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with(".spec.tsx")
        || file_name.ends_with(".spec.mjs")
        || (file_name.starts_with("test_") && file_name.ends_with(".py"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::TestDriftConfig;
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
    fn changed_scope_maps_refactor_transform_fixture_to_regression_test() {
        assert_eq!(
            changed_test_file_for_path("tests/fixtures/refactor_transform_no_match.json"),
            Some("tests/refactor_transform_test.rs".to_string())
        );
        assert_eq!(
            changed_test_file_for_path(
                "tests/fixtures/output_contracts/errors/runner-unsupported.json"
            ),
            Some("tests/output_errors.rs".to_string())
        );
        assert_eq!(
            changed_test_file_for_path(
                "tests/fixtures/output_contracts/quality/audit-summary.json"
            ),
            Some("tests/output_contracts_test.rs".to_string())
        );
        assert_eq!(
            changed_test_file_for_path("tests/fixtures/golden_json_contracts/runs_contract.json"),
            Some("src/commands/golden_contract_tests.rs".to_string())
        );
        assert_eq!(
            changed_test_file_for_path("tests/fixtures/other.json"),
            None
        );
        assert_eq!(
            changed_test_file_for_path("src/core/extension/test/mod.rs"),
            None
        );
        assert_eq!(
            changed_test_file_for_path("src/core/audit_test.rs"),
            Some("src/core/audit_test.rs".to_string())
        );
    }

    #[test]
    fn rust_cargo_changed_routing_emits_integration_args() {
        let dir = TempDir::new().expect("temp dir should be created");
        let component = Component::new(
            "fixture-component".to_string(),
            dir.path().to_string_lossy().to_string(),
            "/tmp/remote".to_string(),
            None,
        );

        let env = rust_cargo_changed_test_env(
            &component,
            &[
                "tests/integration_scope.rs".to_string(),
                "tests/another_scope.rs".to_string(),
            ],
        );

        assert!(env.contains(&(
            "HOMEBOY_TEST_SCOPE_KIND".to_string(),
            format!("{}_{}", "rust", "integration")
        )));
        assert!(env.contains(&(
            "HOMEBOY_TEST_RUNNER_ARGS".to_string(),
            "--test\nintegration_scope\n--test\nanother_scope".to_string()
        )));
    }

    #[test]
    fn rust_cargo_changed_routing_emits_inline_filter() {
        let dir = TempDir::new().expect("temp dir should be created");
        let component = Component::new(
            "fixture-component".to_string(),
            dir.path().to_string_lossy().to_string(),
            "/tmp/remote".to_string(),
            None,
        );

        let env = rust_cargo_changed_test_env(&component, &["src/core/daemon.rs".to_string()]);

        assert!(env.contains(&(
            "HOMEBOY_TEST_SCOPE_KIND".to_string(),
            format!("{}_{}", "rust", "filter")
        )));
        assert!(env.contains(&(
            "HOMEBOY_TEST_RUNNER_ARGS".to_string(),
            "--\ncore::daemon".to_string()
        )));
    }

    #[test]
    fn exclusive_changed_routing_only_sets_env_when_all_files_match() {
        let config = TestChangedFileExclusiveEnv {
            name: "HOMEBOY_FIXTURE_HOST_SMOKE_FILES".to_string(),
            globs: vec!["tests/**/*-smoke.php".to_string()],
            extensions: Vec::new(),
        };

        let env = exclusive_changed_test_env(
            &[
                "tests/import-agent-ability-smoke.php".to_string(),
                "tests/nested/queue-routing-smoke.php".to_string(),
            ],
            &config,
        );

        assert!(env.contains(&(
            "HOMEBOY_TEST_SCOPE_KIND".to_string(),
            "exclusive_env".to_string()
        )));
        assert!(env.contains(&(
            "HOMEBOY_TEST_SCOPE_ENV_VALUE".to_string(),
            "tests/import-agent-ability-smoke.php\ntests/nested/queue-routing-smoke.php"
                .to_string()
        )));

        assert!(exclusive_changed_test_env(
            &[
                "tests/import-agent-ability-smoke.php".to_string(),
                "tests/Unit/FooTest.php".to_string(),
            ],
            &config,
        )
        .is_empty());
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
