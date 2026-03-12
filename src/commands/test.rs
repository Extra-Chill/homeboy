use clap::Args;
use serde::Serialize;
use std::path::PathBuf;

use homeboy::component::Component;
use homeboy::extension::test as extension_test;
use homeboy::extension::test::{
    auto_fix_test_drift, detect_test_drift, CoverageOutput, DriftReport, TestAnalysis,
    TestRunWorkflowArgs, TestScopeOutput, TestSummaryOutput,
};
use homeboy::refactor::AppliedRefactor;
use homeboy::scaffold::ScaffoldConfig;
use homeboy::extension::test::{TestBaselineComparison, TestCounts};

use super::utils::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct TestArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Skip linting before running tests
    #[arg(long)]
    skip_lint: bool,

    /// Auto-fix linting issues before running tests
    #[arg(long)]
    fix: bool,

    /// Collect code coverage (requires xdebug/pcov for PHP, cargo-tarpaulin for Rust)
    #[arg(long)]
    coverage: bool,

    /// Minimum coverage percentage — fail if below this threshold (implies --coverage)
    #[arg(long, value_name = "PERCENT")]
    coverage_min: Option<f64>,

    #[command(flatten)]
    baseline_args: BaselineArgs,

    /// Auto-update baseline when test results improve (ratchet forward)
    #[arg(long)]
    ratchet: bool,

    /// Analyze test failures — cluster by root cause and suggest fixes
    #[arg(long)]
    analyze: bool,

    /// Detect test drift — cross-reference production changes with test files
    #[arg(long)]
    drift: bool,

    /// Generate test stubs for untested source files
    #[arg(long)]
    scaffold: bool,

    /// Scaffold a specific source file (relative to component root)
    #[arg(long, value_name = "FILE")]
    scaffold_file: Option<String>,

    /// Write scaffold files to disk (default: dry-run)
    #[arg(long)]
    write: bool,

    /// Git ref to compare against for drift detection (tag, commit, branch)
    #[arg(long, value_name = "REF", default_value = "HEAD~10")]
    since: String,

    /// Limit test execution to files changed since this git ref (PR impact scope)
    #[arg(long, value_name = "REF")]
    changed_since: Option<String>,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Additional arguments to pass to the test runner (must follow --)
    #[arg(last = true)]
    args: Vec<String>,

    #[command(flatten)]
    _json: HiddenJsonArgs,

    /// Print compact machine-readable summary (for CI wrappers)
    #[arg(long)]
    json_summary: bool,
}

#[derive(Serialize)]
pub struct TestOutput {
    status: String,
    component: String,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_counts: Option<TestCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<CoverageOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_comparison: Option<TestBaselineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<TestAnalysis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    autofix: Option<AppliedRefactor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    drift: Option<DriftReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scaffold: Option<ScaffoldOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_fix_drift: Option<AutoFixDriftOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_scope: Option<TestScopeOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<TestSummaryOutput>,
}

#[derive(Serialize)]
pub struct ScaffoldOutput {
    results: Vec<ScaffoldFileOutput>,
    total_stubs: usize,
    total_written: usize,
    total_skipped: usize,
}

#[derive(Serialize)]
pub struct ScaffoldFileOutput {
    source_file: String,
    test_file: String,
    stub_count: usize,
    written: bool,
    skipped: bool,
}

#[derive(Serialize)]
pub struct AutoFixDriftOutput {
    since: String,
    auto_fixable_changes: usize,
    generated_rules: usize,
    replacements: usize,
    files_modified: usize,
    written: bool,
    rerun_recommended: bool,
}

/// Filter out homeboy-owned flags from trailing args before passing to extension scripts.
///
/// Clap's `trailing_var_arg = true` + `allow_hyphen_values = true` captures all arguments
/// after the positional component arg — including flags that Clap also parsed into named
/// fields. This means `--analyze`, `--drift`, etc. end up in both `args.analyze = true`
/// AND `args.args = ["--analyze"]`. The extension test runner passes `args.args` through
/// to the underlying tool (e.g. PHPUnit), which then fails on unknown flags.
///
/// This function strips homeboy-owned flags so only genuine passthrough args (like
/// `--filter=TestName`) reach the extension script.
fn filter_homeboy_flags(args: &[String]) -> Vec<String> {
    // Homeboy-owned boolean flags that should never reach the extension runner
    const HOMEBOY_FLAGS: &[&str] = &[
        "--analyze",
        "--drift",
        "--scaffold",
        "--write",
        "--json-summary",
        "--baseline",
        "--ignore-baseline",
        "--ratchet",
        "--skip-lint",
        "--fix",
        "--coverage",
        "--json",
    ];

    // Homeboy-owned flags that take a value (--flag value or --flag=value)
    const HOMEBOY_VALUE_FLAGS: &[&str] = &[
        "--coverage-min",
        "--since",
        "--changed-since",
        "--scaffold-file",
        "--setting",
        "--path",
    ];

    let mut filtered = Vec::new();
    let mut skip_next = false;

    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }

        // Check boolean flags (exact match)
        if HOMEBOY_FLAGS.contains(&arg.as_str()) {
            continue;
        }

        // Check value flags: --flag=value (single arg) or --flag value (two args)
        let is_value_flag = HOMEBOY_VALUE_FLAGS.iter().any(|f| {
            if arg.starts_with(&format!("{}=", f)) {
                return true; // --flag=value form, skip this arg only
            }
            if arg == *f {
                skip_next = true; // --flag value form, skip this and next
                return true;
            }
            false
        });

        if is_value_flag {
            continue;
        }

        filtered.push(arg.clone());
    }

    filtered
}

pub fn run(args: TestArgs, _global: &GlobalArgs) -> CmdResult<TestOutput> {
    let source_path = args.comp.source_path()?;
    let component = args.comp.load()?;

    // Scaffold mode — generate test stubs without running tests
    if args.scaffold || args.scaffold_file.is_some() {
        return run_scaffold(
            args.comp.id(),
            &component,
            args.scaffold_file.as_deref(),
            args.write,
        );
    }

    // Drift detection mode — skip running tests, analyze git changes instead
    if args.drift {
        if args.fix {
            return run_auto_fix_drift(args.comp.id(), &component, &args.since, args.write, true);
        }
        return run_drift(args.comp.id(), &component, &args.since);
    }

    let passthrough_args = filter_homeboy_flags(&args.args);
    let workflow = extension_test::run_main_test_workflow(
        &component,
        &PathBuf::from(&source_path),
        TestRunWorkflowArgs {
            component_label: args.comp.component.clone(),
            component_id: args.comp.id().to_string(),
            path_override: args.comp.path.clone(),
            settings: args.setting_args.setting.clone(),
            skip_lint: args.skip_lint,
            fix: args.fix,
            coverage: args.coverage,
            coverage_min: args.coverage_min,
            analyze: args.analyze,
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.ratchet,
            changed_since: args.changed_since.clone(),
            json_summary: args.json_summary,
            passthrough_args: passthrough_args.clone(),
        },
    )?;

    Ok((
        TestOutput {
            status: workflow.status,
            component: workflow.component,
            exit_code: workflow.exit_code,
            test_counts: workflow.test_counts,
            coverage: workflow.coverage,
            baseline_comparison: workflow.baseline_comparison,
            analysis: workflow.analysis,
            autofix: workflow.autofix,
            hints: workflow.hints,
            drift: None,
            scaffold: None,
            auto_fix_drift: None,
            test_scope: workflow.test_scope,
            summary: workflow.summary,
        },
        workflow.exit_code,
    ))
}

/// Auto-fix test drift by generating transform rules from production changes.
///
/// This mode does NOT run tests. It inspects git changes since `since`, generates
/// find/replace transform rules for auto-fixable drift types, and applies them to
/// test files. Triggered by `homeboy test --drift --fix`.
/// Use with `--write` to persist changes; default is dry-run.
fn run_auto_fix_drift(
    component_id: &str,
    component: &Component,
    since: &str,
    write: bool,
    include_report: bool,
) -> CmdResult<TestOutput> {
    let result = auto_fix_test_drift(component_id, component, since, write, include_report)?;

    Ok((
        TestOutput {
            status: if result.output.replacements > 0 || !result.hints.is_empty() {
                if write { "fixed" } else { "planned" }.to_string()
            } else {
                "passed".to_string()
            },
            component: result.component,
            exit_code: 0,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: Some(result.hints),
            drift: result.report,
            scaffold: None,
            auto_fix_drift: Some(AutoFixDriftOutput {
                since: result.output.since,
                auto_fixable_changes: result.output.auto_fixable_changes,
                generated_rules: result.output.generated_rules,
                replacements: result.output.replacements,
                files_modified: result.output.files_modified,
                written: result.output.written,
                rerun_recommended: result.output.rerun_recommended,
            }),
            test_scope: None,
            summary: None,
        },
        0,
    ))
}

/// Run drift detection without running tests.
fn run_drift(component_id: &str, component: &Component, since: &str) -> CmdResult<TestOutput> {
    let result = detect_test_drift(component_id, component, since)?;

    Ok((
        TestOutput {
            status: "drift".to_string(),
            component: result.component,
            exit_code: result.exit_code,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            drift: Some(result.report),
            scaffold: None,
            auto_fix_drift: None,
            test_scope: None,
            summary: None,
        },
        result.exit_code,
    ))
}

/// Run scaffold mode — generate test stubs from source files.
fn run_scaffold(
    component_id: &str,
    component: &Component,
    scaffold_file: Option<&str>,
    write: bool,
) -> CmdResult<TestOutput> {
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        std::path::PathBuf::from(expanded.as_ref())
    };

    // Auto-detect language
    let config = if source_path.join("Cargo.toml").exists() {
        ScaffoldConfig::rust()
    } else {
        ScaffoldConfig::php()
    };

    let mode_label = if write { "write" } else { "dry-run" };

    if let Some(file) = scaffold_file {
        // Single file mode
        let file_path = source_path.join(file);
        homeboy::log_status!(
            "scaffold",
            "Scaffolding tests for {} ({})",
            file,
            mode_label
        );

        let result = homeboy::scaffold::scaffold_file(&file_path, &source_path, &config, write)?;

        if result.skipped {
            homeboy::log_status!(
                "scaffold",
                "Skipped — test file already exists: {}",
                result.test_file
            );
        } else if result.stub_count == 0 {
            homeboy::log_status!("scaffold", "No public methods found in {}", file);
        } else {
            homeboy::log_status!(
                "scaffold",
                "Generated {} test stub{} → {}{}",
                result.stub_count,
                if result.stub_count == 1 { "" } else { "s" },
                result.test_file,
                if write { " (written)" } else { " (dry-run)" }
            );

            if !write {
                // Show preview
                eprintln!("---");
                for line in result.content.lines().take(40) {
                    eprintln!("{}", line);
                }
                if result.content.lines().count() > 40 {
                    eprintln!("... ({} more lines)", result.content.lines().count() - 40);
                }
                eprintln!("---");
            }
        }

        let scaffold_output = ScaffoldOutput {
            results: vec![ScaffoldFileOutput {
                source_file: result.source_file.clone(),
                test_file: result.test_file.clone(),
                stub_count: result.stub_count,
                written: result.written,
                skipped: result.skipped,
            }],
            total_stubs: result.stub_count,
            total_written: if result.written { 1 } else { 0 },
            total_skipped: if result.skipped { 1 } else { 0 },
        };

        Ok((
            TestOutput {
                status: "scaffold".to_string(),
                component: component_id.to_string(),
                exit_code: 0,
                test_counts: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints: None,
                drift: None,
                scaffold: Some(scaffold_output),
                auto_fix_drift: None,
                test_scope: None,
                summary: None,
            },
            0,
        ))
    } else {
        // Batch mode — scaffold all untested files
        homeboy::log_status!(
            "scaffold",
            "Scanning {} for untested {} files ({})",
            component_id,
            config.language,
            mode_label
        );

        let batch = homeboy::scaffold::scaffold_untested(&source_path, &config, write)?;

        let files_needing_tests = batch
            .results
            .iter()
            .filter(|r| !r.skipped && r.stub_count > 0)
            .count();
        let already_tested = batch.total_skipped;

        homeboy::log_status!(
            "scaffold",
            "{} file{} need tests, {} already have tests",
            files_needing_tests,
            if files_needing_tests == 1 { "" } else { "s" },
            already_tested
        );

        if write {
            homeboy::log_status!(
                "scaffold",
                "Wrote {} test file{} with {} total stubs",
                batch.total_written,
                if batch.total_written == 1 { "" } else { "s" },
                batch.total_stubs
            );
        } else if files_needing_tests > 0 {
            // Show summary of what would be created
            for result in &batch.results {
                if !result.skipped && result.stub_count > 0 {
                    homeboy::log_status!(
                        "  new",
                        "{} → {} ({} stubs)",
                        result.source_file,
                        result.test_file,
                        result.stub_count
                    );
                }
            }
            homeboy::log_status!(
                "hint",
                "Run with --write to create test files: homeboy test {} --scaffold --write",
                component_id
            );
        }

        let scaffold_output = ScaffoldOutput {
            results: batch
                .results
                .iter()
                .map(|r| ScaffoldFileOutput {
                    source_file: r.source_file.clone(),
                    test_file: r.test_file.clone(),
                    stub_count: r.stub_count,
                    written: r.written,
                    skipped: r.skipped,
                })
                .collect(),
            total_stubs: batch.total_stubs,
            total_written: batch.total_written,
            total_skipped: batch.total_skipped,
        };

        Ok((
            TestOutput {
                status: "scaffold".to_string(),
                component: component_id.to_string(),
                exit_code: 0,
                test_counts: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints: None,
                drift: None,
                scaffold: Some(scaffold_output),
                auto_fix_drift: None,
                test_scope: None,
                summary: None,
            },
            0,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::refactor::test_refactor_request;
    use homeboy::refactor::TestSourceOptions;

    #[test]
    fn filter_strips_boolean_flags() {
        let args = vec!["--analyze".to_string(), "--filter=SomeTest".to_string()];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_multiple_boolean_flags() {
        let args = vec![
            "--analyze".to_string(),
            "--drift".to_string(),
            "--scaffold".to_string(),
            "--baseline".to_string(),
            "--ignore-baseline".to_string(),
            "--ratchet".to_string(),
            "--skip-lint".to_string(),
            "--fix".to_string(),
            "--coverage".to_string(),
            "--write".to_string(),
            "--json".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_strips_value_flags_space_separated() {
        let args = vec![
            "--since".to_string(),
            "v0.36.0".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);

        let args = vec![
            "--changed-since".to_string(),
            "origin/main".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_value_flags_equals_form() {
        let args = vec![
            "--since=v0.36.0".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_coverage_min() {
        let args = vec![
            "--coverage-min".to_string(),
            "80".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_scaffold_file() {
        let args = vec![
            "--scaffold-file".to_string(),
            "inc/Core/Foo.php".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_strips_setting() {
        let args = vec![
            "--setting".to_string(),
            "database_type=mysql".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_preserves_unknown_flags() {
        let args = vec![
            "--filter=SomeTest".to_string(),
            "--group".to_string(),
            "ajax".to_string(),
            "--verbose".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(args, result);
    }

    #[test]
    fn filter_handles_empty() {
        let result = filter_homeboy_flags(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_handles_mixed() {
        let args = vec![
            "--analyze".to_string(),
            "--skip-lint".to_string(),
            "--since".to_string(),
            "v0.35.0".to_string(),
            "--filter=FlowAbilities".to_string(),
            "--coverage-min=80".to_string(),
            "--verbose".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=FlowAbilities", "--verbose"]);
    }

    #[test]
    fn filter_strips_path_flag() {
        let args = vec![
            "--path".to_string(),
            "/tmp/checkout".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn filter_strips_json_summary_flag() {
        let args = vec![
            "--json-summary".to_string(),
            "--filter=SomeTest".to_string(),
        ];
        let result = filter_homeboy_flags(&args);
        assert_eq!(result, vec!["--filter=SomeTest"]);
    }

    #[test]
    fn test_fix_builds_canonical_refactor_request() {
        let component = Component::new(
            "demo".to_string(),
            "/tmp/demo".to_string(),
            String::new(),
            None,
        );

        let request = test_refactor_request(
            component.clone(),
            PathBuf::from("/tmp/demo"),
            vec![("runner".to_string(), "ci".to_string())],
            TestSourceOptions {
                selected_files: Some(vec!["tests/demo_test.rs".to_string()]),
                skip_lint: true,
                script_args: vec!["--filter=DemoTest".to_string()],
            },
            true,
        );

        assert_eq!(request.component.id, component.id);
        assert_eq!(request.sources, vec!["test".to_string()]);
        assert!(request.write);
        assert_eq!(request.settings.len(), 1);
        assert!(request.lint.selected_files.is_none());
        assert_eq!(request.test.selected_files.as_ref().unwrap().len(), 1);
        assert!(request.test.skip_lint);
    }
}
