use clap::Args;
use serde::Serialize;

use homeboy::component::Component;
use homeboy::error::Error;
use homeboy::extension::{self, ExtensionRunner};
use homeboy::refactor::{self, TransformSet};
use homeboy::test_analyze::{self, TestAnalysis, TestAnalysisInput};
use homeboy::test_baseline::{self, TestBaselineComparison, TestCounts};
use homeboy::test_drift::{self, DriftOptions, DriftReport};
use homeboy::test_scaffold::{self, ScaffoldConfig};
use homeboy::utils::autofix::{self, AutofixMode, FixResultsSummary};

use super::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::test_scope::{compute_changed_test_scope, TestScopeOutput};
use super::{CmdResult, GlobalArgs};

mod parsing;

pub use parsing::CoverageOutput;
use parsing::{build_test_summary, TestSummaryOutput};

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
    autofix: Option<TestAutofixOutput>,
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
pub struct TestAutofixOutput {
    files_modified: usize,
    rerun_recommended: bool,
    /// Structured summary of what the extension fixed (populated when the
    /// extension writes to `HOMEBOY_FIX_RESULTS_FILE`).
    #[serde(skip_serializing_if = "Option::is_none")]
    fix_summary: Option<FixResultsSummary>,
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

/// Attempt to auto-detect the extension for a component based on contextual clues.
fn auto_detect_extension(component: &Component) -> Option<String> {
    // Check build_command for extension references (e.g., "extensions/wordpress/scripts/build")
    if let Some(ref cmd) = component.build_command {
        if cmd.contains("extensions/wordpress") {
            return Some("wordpress".to_string());
        }
    }

    // Check for composer.json in local_path (indicates WordPress/PHP component)
    let expanded = shellexpand::tilde(&component.local_path);
    let composer_path = std::path::Path::new(expanded.as_ref()).join("composer.json");
    if composer_path.exists() {
        return Some("wordpress".to_string());
    }

    // Check for Cargo.toml in local_path (indicates Rust component)
    let cargo_path = std::path::Path::new(expanded.as_ref()).join("Cargo.toml");
    if cargo_path.exists() {
        return Some("rust".to_string());
    }

    None
}

fn no_extensions_error(component: &Component) -> Error {
    Error::validation_invalid_argument(
        "component",
        format!(
            "Component '{}' has no extensions configured and none could be auto-detected",
            component.id
        ),
        None,
        None,
    )
    .with_hint(format!(
        "Add a extension: homeboy component set {} --extension wordpress",
        component.id
    ))
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

pub(crate) fn resolve_test_script(component: &Component) -> homeboy::error::Result<String> {
    let extension_id_owned: String;
    let extension_id: &str = if let Some(ref extensions) = component.extensions {
        if extensions.contains_key("wordpress") {
            "wordpress"
        } else if let Some(key) = extensions.keys().next() {
            key.as_str()
        } else if let Some(detected) = auto_detect_extension(component) {
            extension_id_owned = detected;
            &extension_id_owned
        } else {
            return Err(no_extensions_error(component));
        }
    } else if let Some(detected) = auto_detect_extension(component) {
        extension_id_owned = detected;
        &extension_id_owned
    } else {
        return Err(no_extensions_error(component));
    };

    let manifest = extension::load_extension(extension_id)?;

    manifest
        .test_script()
        .map(|s| s.to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension '{}' does not have test infrastructure configured (missing test.extension_script)",
                    extension_id
                ),
                None,
                None,
            )
        })
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

    // Compute optional PR/impact-scoped test selection
    let changed_scope = if let Some(ref git_ref) = args.changed_since {
        Some(compute_changed_test_scope(&component, git_ref)?)
    } else {
        None
    };
    let script_path = resolve_test_script(&component)?;

    // Coverage is enabled by --coverage or --coverage-min
    let coverage_enabled = args.coverage || args.coverage_min.is_some();

    // Create temp file for coverage output
    let coverage_file = if coverage_enabled {
        Some(std::env::temp_dir().join(format!("homeboy-coverage-{}.json", std::process::id())))
    } else {
        None
    };

    // Create temp file for test results output
    let results_file =
        std::env::temp_dir().join(format!("homeboy-test-results-{}.json", std::process::id()));

    // Create temp file for test failures output (for --analyze)
    let failures_file = if args.analyze {
        Some(
            std::env::temp_dir().join(format!("homeboy-test-failures-{}.json", std::process::id())),
        )
    } else {
        None
    };

    let fix_results_file = autofix::fix_results_temp_path();
    let before_fix_files = if args.fix {
        Some(autofix::changed_file_set(&component.local_path)?)
    } else {
        None
    };

    let mut runner = ExtensionRunner::new(args.comp.id(), &script_path)
        .component(component.clone())
        .path_override(args.comp.path.clone())
        .settings(&args.setting_args.setting)
        .env_if(args.skip_lint, "HOMEBOY_SKIP_LINT", "1")
        .env_if(args.fix, "HOMEBOY_AUTO_FIX", "1")
        .env_if(coverage_enabled, "HOMEBOY_COVERAGE", "1")
        .env("HOMEBOY_TEST_RESULTS_FILE", &results_file.to_string_lossy())
        .env_if(
            args.fix,
            "HOMEBOY_FIX_RESULTS_FILE",
            &fix_results_file.to_string_lossy(),
        );

    if let Some(ref file) = coverage_file {
        runner = runner.env("HOMEBOY_COVERAGE_FILE", &file.to_string_lossy());
    }

    if let Some(ref file) = failures_file {
        runner = runner.env("HOMEBOY_TEST_FAILURES_FILE", &file.to_string_lossy());
    }

    if let Some(min) = args.coverage_min {
        runner = runner.env("HOMEBOY_COVERAGE_MIN", &format!("{}", min));
    }

    let passthrough_args = filter_homeboy_flags(&args.args);

    if let Some(ref scope) = changed_scope {
        if scope.selected_files.is_empty() {
            homeboy::log_status!(
                "test",
                "No changed-scope tests found since {}. Skipping test runner.",
                scope.changed_since.as_deref().unwrap_or("unknown")
            );

            let hints = Some(vec![
                format!(
                    "No impacted tests found for --changed-since {}",
                    scope.changed_since.as_deref().unwrap_or("unknown")
                ),
                format!("Run full suite if needed: homeboy test {}", args.comp.id()),
            ]);

            return Ok((
                TestOutput {
                    status: "passed".to_string(),
                    component: args.comp.component.clone(),
                    exit_code: 0,
                    test_counts: None,
                    coverage: None,
                    baseline_comparison: None,
                    analysis: None,
                    autofix: None,
                    hints,
                    drift: None,
                    scaffold: None,
                    auto_fix_drift: None,
                    test_scope: Some(scope.clone()),
                    summary: if args.json_summary {
                        Some(build_test_summary(None, None, 0))
                    } else {
                        None
                    },
                },
                0,
            ));
        }

        // Pass changed test files to the extension via env var.
        // The extension's test runner decides how to scope (e.g., PHPUnit
        // uses --filter, Cargo uses positional test names, Jest uses
        // --testPathPattern). Core does not generate runner-specific args.
        runner = runner.env(
            "HOMEBOY_CHANGED_TEST_FILES",
            &scope.selected_files.join("\n"),
        );

        homeboy::log_status!(
            "test",
            "Scoped test run: {} selected file(s) since {}",
            scope.selected_count,
            scope.changed_since.as_deref().unwrap_or("unknown")
        );
    }

    let output = runner.script_args(&passthrough_args).run()?;

    // Read test results if available
    let test_counts = parsing::parse_test_results_file(&results_file)
        .or_else(|| parsing::parse_test_results_text(&output.stdout));

    // Clean up test results temp file
    let _ = std::fs::remove_file(&results_file);

    // Read structured fix results from extension sidecar (if written).
    let test_autofix = if args.fix {
        let after_fix_files = autofix::changed_file_set(&component.local_path)?;
        let files_modified = before_fix_files
            .as_ref()
            .map(|before| autofix::count_newly_changed(before, &after_fix_files))
            .unwrap_or(0);

        let fix_results = autofix::parse_fix_results_file(&fix_results_file);
        let _ = std::fs::remove_file(&fix_results_file);

        let fix_summary = if fix_results.is_empty() {
            None
        } else {
            Some(autofix::summarize_fix_results(&fix_results))
        };

        Some(TestAutofixOutput {
            files_modified,
            rerun_recommended: files_modified > 0,
            fix_summary,
        })
    } else {
        None
    };

    // Determine actual test status: when parsed results show 0 failures,
    // treat as passed even if the runner script exited non-zero (e.g., lint
    // failures, deprecation notices, or PHPUnit warnings that don't indicate
    // actual test failures).
    let status = if let Some(ref counts) = test_counts {
        if counts.failed == 0 {
            "passed"
        } else {
            "failed"
        }
    } else if output.success {
        "passed"
    } else {
        "failed"
    };

    // Read coverage results if available
    let coverage = coverage_file
        .as_ref()
        .and_then(|f| parsing::parse_coverage_file(f).ok());

    // Clean up coverage temp file
    if let Some(ref f) = coverage_file {
        let _ = std::fs::remove_file(f);
    }

    // Read and analyze test failures if --analyze
    let analysis = if args.analyze {
        let analysis_input = failures_file
            .as_ref()
            .and_then(|f| parsing::parse_failures_file(f))
            .unwrap_or_else(|| TestAnalysisInput {
                failures: Vec::new(),
                total: test_counts.as_ref().map(|c| c.total).unwrap_or(0),
                passed: test_counts.as_ref().map(|c| c.passed).unwrap_or(0),
            });

        // Clean up failures temp file
        if let Some(ref f) = failures_file {
            let _ = std::fs::remove_file(f);
        }

        let result = test_analyze::analyze(args.comp.id(), &analysis_input);

        if !result.clusters.is_empty() {
            eprintln!(
                "[test] Analysis: {} failure(s) in {} cluster(s)",
                result.total_failures,
                result.clusters.len(),
            );
            for (i, cluster) in result.clusters.iter().enumerate().take(5) {
                eprintln!(
                    "[test]   {}. {} ({} failures) — {:?}",
                    i + 1,
                    cluster.pattern,
                    cluster.count,
                    cluster.category,
                );
            }
        }

        Some(result)
    } else {
        // Clean up failures temp file (if somehow set)
        if let Some(ref f) = failures_file {
            let _ = std::fs::remove_file(f);
        }
        None
    };

    // --baseline: save current state
    if args.baseline_args.baseline {
        if let Some(ref counts) = test_counts {
            let saved = test_baseline::save_baseline(&source_path, args.comp.id(), counts)?;
            eprintln!(
                "[test] Baseline saved to {} ({} passed, {} failed, {} total)",
                saved.display(),
                counts.passed,
                counts.failed,
                counts.total,
            );
        } else {
            eprintln!(
                "[test] Cannot save baseline: no test results available. \
                 Ensure the extension writes HOMEBOY_TEST_RESULTS_FILE."
            );
        }
    }

    // Baseline comparison
    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_args.baseline && !args.baseline_args.ignore_baseline {
        if let Some(ref counts) = test_counts {
            // Try explicit baseline first, then differential from git ref
            let resolved_baseline = test_baseline::load_baseline(&source_path).or_else(|| {
                args.changed_since.as_ref().and_then(|git_ref| {
                    let bl = test_baseline::load_baseline_from_ref(
                        &source_path.to_string_lossy(),
                        git_ref,
                    );
                    if bl.is_some() {
                        eprintln!(
                            "[test] Using baseline from {} for differential comparison",
                            git_ref
                        );
                    }
                    bl
                })
            });

            if let Some(existing_baseline) = resolved_baseline {
                let comparison = test_baseline::compare(counts, &existing_baseline);

                if comparison.regression {
                    eprintln!("[test] REGRESSION: {}", comparison.reasons.join("; "));
                    baseline_exit_override = Some(1);
                } else if comparison.passed_delta > 0 || comparison.failed_delta < 0 {
                    eprintln!(
                        "[test] Improvement: passed {} ({:+}), failed {} ({:+})",
                        counts.passed,
                        comparison.passed_delta,
                        counts.failed,
                        comparison.failed_delta,
                    );

                    // Auto-ratchet: update baseline when results improve
                    if args.ratchet {
                        let _ = test_baseline::save_baseline(&source_path, args.comp.id(), counts);
                        eprintln!("[test] Baseline ratcheted forward");
                    }
                } else {
                    eprintln!(
                        "[test] No regression: passed {} (same), failed {} (same)",
                        counts.passed, counts.failed,
                    );
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let mut hints = Vec::new();

    let comp_id = args.comp.id();

    // Filter hint when tests fail and no passthrough args were used
    if status == "failed" && passthrough_args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy test {} -- --filter=TestName",
            comp_id
        ));
    }

    // Fix hint when lint is enabled (default) and --fix not used
    if !args.skip_lint && !args.fix {
        hints.push(format!(
            "Auto-fix lint issues: homeboy test {} --fix",
            comp_id
        ));
    }

    // Coverage hint when not using coverage
    if !coverage_enabled {
        hints.push(format!(
            "Collect coverage: homeboy test {} --coverage",
            comp_id
        ));
    }

    // Baseline hints
    if test_counts.is_some() && !args.baseline_args.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save test baseline: homeboy test {} --baseline",
            comp_id
        ));
    }

    // Ratchet hint when baseline exists but --ratchet not used
    if baseline_comparison.is_some() && !args.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: homeboy test {} --ratchet",
            comp_id
        ));
    }

    // Analyze hint when tests fail and --analyze not used
    if status == "failed" && !args.analyze {
        hints.push(format!(
            "Analyze failures: homeboy test {} --analyze",
            comp_id
        ));
    }

    // Capability hint when not using passthrough args
    if args.args.is_empty() {
        hints.push("Pass args to test runner: homeboy test <component> -- [args]".to_string());
    }

    // Always include docs reference
    hints.push("Full options: homeboy docs commands/test".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };

    // Exit code: when parsed test results show 0 failures, force exit code 0
    // even if the runner script exited non-zero (lint failures, deprecation notices).
    // Baseline regression still overrides.
    let test_exit_code = if status == "passed" {
        0
    } else {
        output.exit_code
    };
    let exit_code = baseline_exit_override.unwrap_or(test_exit_code);
    let summary = if args.json_summary {
        Some(build_test_summary(
            test_counts.as_ref(),
            analysis.as_ref(),
            exit_code,
        ))
    } else {
        None
    };

    Ok((
        TestOutput {
            status: status.to_string(),
            component: args.comp.component.clone(),
            exit_code,
            test_counts,
            coverage,
            baseline_comparison,
            analysis,
            autofix: test_autofix,
            hints,
            drift: None,
            scaffold: None,
            auto_fix_drift: None,
            test_scope: changed_scope,
            summary,
        },
        exit_code,
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
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        std::path::PathBuf::from(expanded.as_ref())
    };

    let opts = if source_path.join("Cargo.toml").exists() {
        DriftOptions::rust(&source_path, since)
    } else {
        DriftOptions::php(&source_path, since)
    };

    homeboy::log_status!(
        "test",
        "Auto-fixing drift since {} in {} ({})",
        since,
        component_id,
        if write { "write" } else { "dry-run" }
    );

    let drift_report = test_drift::detect_drift(component_id, &opts)?;
    let rules = test_drift::generate_transform_rules(&drift_report);

    let output = if rules.is_empty() {
        homeboy::log_status!("test", "No auto-fixable drift detected. Nothing to apply.");

        AutoFixDriftOutput {
            since: since.to_string(),
            auto_fixable_changes: drift_report.auto_fixable,
            generated_rules: 0,
            replacements: 0,
            files_modified: 0,
            written: write,
            rerun_recommended: false,
        }
    } else {
        let set = TransformSet {
            description: format!(
                "Auto-generated drift fixes for {} since {}",
                component_id, since
            ),
            rules,
        };

        let result =
            refactor::apply_transforms(&source_path, "test_auto_fix_drift", &set, write, None)?;

        homeboy::log_status!(
            "test",
            "Applied {} replacement{} across {} file{}",
            result.total_replacements,
            if result.total_replacements == 1 {
                ""
            } else {
                "s"
            },
            result.total_files,
            if result.total_files == 1 { "" } else { "s" },
        );

        if !write {
            homeboy::log_status!(
                "hint",
                "Dry-run only. Re-run with --write to apply generated fixes."
            );
        } else if result.total_replacements > 0 {
            homeboy::log_status!(
                "hint",
                "Re-run tests: homeboy test {} --analyze",
                component_id
            );
        }

        AutoFixDriftOutput {
            since: since.to_string(),
            auto_fixable_changes: drift_report.auto_fixable,
            generated_rules: set.rules.len(),
            replacements: result.total_replacements,
            files_modified: result.total_files,
            written: write,
            rerun_recommended: write && result.total_replacements > 0,
        }
    };

    let outcome = autofix::standard_outcome(
        if write {
            AutofixMode::Write
        } else {
            AutofixMode::DryRun
        },
        output.replacements,
        Some(format!("homeboy test {} --analyze", component_id)),
        vec![format!(
            "Use --since <ref> to target a drift window (current: {})",
            since
        )],
    );

    Ok((
        TestOutput {
            status: outcome.status,
            component: component_id.to_string(),
            exit_code: 0,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: Some(outcome.hints),
            drift: if include_report {
                Some(drift_report)
            } else {
                None
            },
            scaffold: None,
            auto_fix_drift: Some(AutoFixDriftOutput {
                rerun_recommended: outcome.rerun_recommended,
                ..output
            }),
            test_scope: None,
            summary: None,
        },
        0,
    ))
}

/// Run drift detection without running tests.
fn run_drift(component_id: &str, component: &Component, since: &str) -> CmdResult<TestOutput> {
    let source_path = {
        let expanded = shellexpand::tilde(&component.local_path);
        std::path::PathBuf::from(expanded.as_ref())
    };

    homeboy::log_status!(
        "drift",
        "Detecting test drift since {} in {}",
        since,
        component_id
    );

    // Auto-detect language from extension
    let opts = if source_path.join("Cargo.toml").exists() {
        DriftOptions::rust(&source_path, since)
    } else {
        DriftOptions::php(&source_path, since)
    };

    let report = test_drift::detect_drift(component_id, &opts)?;

    // Report to stderr
    if report.production_changes.is_empty() {
        homeboy::log_status!("drift", "No production changes detected since {}", since);
    } else {
        homeboy::log_status!(
            "drift",
            "{} production change{} detected",
            report.production_changes.len(),
            if report.production_changes.len() == 1 {
                ""
            } else {
                "s"
            }
        );

        for change in &report.production_changes {
            let label = match change.change_type {
                test_drift::ChangeType::MethodRename => "method rename",
                test_drift::ChangeType::MethodRemoved => "method removed",
                test_drift::ChangeType::ClassRename => "class rename",
                test_drift::ChangeType::ClassRemoved => "class removed",
                test_drift::ChangeType::ErrorCodeChange => "error code change",
                test_drift::ChangeType::ReturnTypeChange => "return type change",
                test_drift::ChangeType::SignatureChange => "signature change",
                test_drift::ChangeType::FileMove => "file moved",
                test_drift::ChangeType::StringChange => "string changed",
            };

            if let Some(ref new) = change.new_symbol {
                homeboy::log_status!(
                    "  change",
                    "{}: {} → {} ({})",
                    label,
                    change.old_symbol,
                    new,
                    change.file
                );
            } else {
                homeboy::log_status!(
                    "  change",
                    "{}: {} ({})",
                    label,
                    change.old_symbol,
                    change.file
                );
            }
        }

        if !report.drifted_tests.is_empty() {
            homeboy::log_status!(
                "drift",
                "{} drifted reference{} in {} test file{}",
                report.drifted_tests.len(),
                if report.drifted_tests.len() == 1 {
                    ""
                } else {
                    "s"
                },
                report.total_drifted_files,
                if report.total_drifted_files == 1 {
                    ""
                } else {
                    "s"
                },
            );

            for dt in report.drifted_tests.iter().take(20) {
                let change = &report.production_changes[dt.change_index];
                homeboy::log_status!(
                    "  ref",
                    "{}:{} references '{}' ({})",
                    dt.test_file,
                    dt.line,
                    change.old_symbol,
                    format!("{:?}", change.change_type).to_lowercase()
                );
            }

            if report.drifted_tests.len() > 20 {
                homeboy::log_status!(
                    "info",
                    "... and {} more (use --json for full list)",
                    report.drifted_tests.len() - 20
                );
            }
        }

        if report.auto_fixable > 0 {
            homeboy::log_status!(
                "hint",
                "{} change{} auto-fixable with refactor transform",
                report.auto_fixable,
                if report.auto_fixable == 1 { "" } else { "s" }
            );
        }
    }

    let exit_code = if report.drifted_tests.is_empty() {
        0
    } else {
        1
    };

    Ok((
        TestOutput {
            status: "drift".to_string(),
            component: component_id.to_string(),
            exit_code,
            test_counts: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            drift: Some(report),
            scaffold: None,
            auto_fix_drift: None,
            test_scope: None,
            summary: None,
        },
        exit_code,
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

        let result = test_scaffold::scaffold_file(&file_path, &source_path, &config, write)?;

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

        let batch = test_scaffold::scaffold_untested(&source_path, &config, write)?;

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
}
