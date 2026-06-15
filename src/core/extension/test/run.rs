use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::local_files;
use crate::core::engine::output_parse::ParseSpec;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, ErrorCode};
use crate::core::extension::test::analyze::{analyze, TestAnalysis, TestAnalysisInput};
use crate::core::extension::test::baseline::{self, TestBaselineComparison, TestCounts};
use crate::core::extension::test::{
    build_test_runner, build_test_summary, compute_changed_test_scope,
    normalize_test_passthrough_args, parse_coverage_file, parse_failures_file,
    parse_test_results_file_with_spec, parse_test_results_text, parse_test_results_text_with_spec,
    CoverageOutput, TestScopeOutput, TestSummaryOutput,
};
use crate::core::extension::{self, ExtensionCapability, ExtensionPhaseTiming};
use crate::core::finding::HomeboyFinding;
use crate::core::observation::homeboy_findings_from_test_analysis_input;
use crate::core::refactor::AppliedRefactor;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TestRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub skip_lint: bool,
    pub coverage: bool,
    pub coverage_min: Option<f64>,
    pub analyze: bool,
    pub baseline_flags: BaselineFlags,
    pub changed_since: Option<String>,
    pub json_summary: bool,
    pub ci_env: Vec<(String, String)>,
    pub passthrough_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub test_counts: Option<TestCounts>,
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip)]
    pub failure_analysis_input: Option<TestAnalysisInput>,
    pub coverage: Option<CoverageOutput>,
    pub baseline_comparison: Option<TestBaselineComparison>,
    pub analysis: Option<TestAnalysis>,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub test_scope: Option<TestScopeOutput>,
    pub summary: Option<TestSummaryOutput>,
    /// Tail of the runner's stdout/stderr, surfaced when tests fail so users
    /// can see runner output (bootstrap errors, stack traces) without
    /// having to re-run with a different flag. (#1143)
    pub raw_output: Option<RawTestOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

/// Captured tail of a test runner's stdout/stderr.
///
/// Surfaced on failure so the actual tool output
/// is visible in the structured JSON response. The tail is bounded by
/// `RAW_OUTPUT_TAIL_LINES` to keep JSON payloads small while still showing
/// the last error / stack frame, which is almost always the relevant part
/// for bootstrap failures. (#1143)
#[derive(Debug, Clone, Serialize)]
pub struct RawTestOutput {
    /// Last N lines of stdout. Empty string if the runner emitted no stdout.
    pub stdout_tail: String,
    /// Last N lines of stderr. Empty string if the runner emitted no stderr.
    pub stderr_tail: String,
    /// Whether either tail was truncated from the original output.
    pub truncated: bool,
    /// Whether stdout capture itself was bounded before the line tail was built.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub stdout_truncated: bool,
    /// Whether stderr capture itself was bounded before the line tail was built.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub stderr_truncated: bool,
    /// Total stdout bytes observed before bounded capture retained its tail.
    #[serde(skip_serializing_if = "crate::is_zero", default)]
    pub stdout_seen_bytes: usize,
    /// Stdout bytes retained in this structured raw-output excerpt.
    #[serde(skip_serializing_if = "crate::is_zero", default)]
    pub stdout_retained_bytes: usize,
    /// Total stderr bytes observed before bounded capture retained its tail.
    #[serde(skip_serializing_if = "crate::is_zero", default)]
    pub stderr_seen_bytes: usize,
    /// Stderr bytes retained in this structured raw-output excerpt.
    #[serde(skip_serializing_if = "crate::is_zero", default)]
    pub stderr_retained_bytes: usize,
    /// Maximum stdout bytes retained by the self-check capture buffer.
    #[serde(skip_serializing_if = "crate::is_zero", default)]
    pub stdout_limit_bytes: usize,
    /// Maximum stderr bytes retained by the self-check capture buffer.
    #[serde(skip_serializing_if = "crate::is_zero", default)]
    pub stderr_limit_bytes: usize,
}

const RAW_OUTPUT_TAIL_LINES: usize = 80;
const PHPUNIT_NO_DISCOVERY_MARKER: &str = "NO PHPUNIT TEST FILES DISCOVERED";
const REQUIRE_PHPUNIT_TESTS_SETTING: &str = "require_phpunit_tests";

fn tail_lines(s: &str, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= max_lines {
        (s.to_string(), false)
    } else {
        let start = lines.len() - max_lines;
        (lines[start..].join("\n"), true)
    }
}

fn test_run_status(
    runner_success: bool,
    test_counts: Option<&TestCounts>,
    phpunit_no_discovery: bool,
    require_phpunit_tests: bool,
) -> &'static str {
    if !runner_success {
        return "failed";
    }

    if phpunit_no_discovery {
        return if require_phpunit_tests {
            "failed"
        } else {
            "skipped"
        };
    }

    if test_counts.map(|counts| counts.failed == 0).unwrap_or(true) {
        "passed"
    } else {
        "failed"
    }
}

fn phpunit_no_discovery(stdout: &str, stderr: &str) -> bool {
    stdout.contains(PHPUNIT_NO_DISCOVERY_MARKER) || stderr.contains(PHPUNIT_NO_DISCOVERY_MARKER)
}

fn setting_truthy(settings: &[(String, String)], key: &str) -> bool {
    settings.iter().any(|(setting_key, value)| {
        setting_key == key
            && matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
    })
}

pub fn run_main_test_workflow(
    component: &Component,
    source_path: &PathBuf,
    args: TestRunWorkflowArgs,
    run_dir: &RunDir,
) -> crate::core::Result<TestRunWorkflowResult> {
    let changed_scope = if let Some(ref git_ref) = args.changed_since {
        Some(compute_changed_test_scope(component, git_ref)?)
    } else {
        None
    };

    let coverage_enabled = args.coverage || args.coverage_min.is_some();
    let results_file = run_dir.step_file(run_dir::files::TEST_RESULTS);
    let coverage_file = if coverage_enabled {
        Some(run_dir.step_file(run_dir::files::COVERAGE))
    } else {
        None
    };
    let failures_file = run_dir.step_file(run_dir::files::TEST_FAILURES);

    let changed_test_files = changed_scope
        .as_ref()
        .map(|scope| scope.selected_files.as_slice());

    if let Some(ref scope) = changed_scope {
        if scope.selected_files.is_empty() {
            let hints = Some(vec![
                format!(
                    "No impacted tests found for --changed-since {}",
                    scope.changed_since.as_deref().unwrap_or("unknown")
                ),
                format!(
                    "Run full suite if needed: homeboy test {}",
                    args.component_id
                ),
            ]);

            return Ok(TestRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                test_counts: None,
                findings: None,
                failure_analysis_input: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints,
                test_scope: Some(scope.clone()),
                summary: if args.json_summary {
                    Some(build_test_summary(None, None, 0))
                } else {
                    None
                },
                raw_output: None,
                extension_phase_timings: Vec::new(),
            });
        }
    }

    let test_context = crate::core::extension::test::resolve_test_command(component).ok();
    let result_parse = test_context
        .as_ref()
        .and_then(|context| crate::core::extension::load_extension(&context.extension_id).ok())
        .and_then(|extension| extension.test.and_then(|test| test.result_parse));

    let runner = build_test_runner(
        component,
        args.path_override.clone(),
        &args.settings,
        args.skip_lint,
        coverage_enabled,
        args.coverage_min,
        changed_test_files,
        run_dir,
    )?;
    let runner = args
        .ci_env
        .iter()
        .fold(runner, |runner, (key, value)| runner.env(key, value));
    let passthrough_args = normalize_test_passthrough_args(component, &args.passthrough_args)?;
    let output = runner
        .env_if(args.changed_since.is_some(), "SCOPE_MODE", "changed")
        .env_if(
            args.changed_since.is_some(),
            "HOMEBOY_CHANGED_SINCE",
            args.changed_since.as_deref().unwrap_or_default(),
        )
        .env_if(
            args.changed_since.is_some(),
            "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES",
            "1",
        )
        .script_args(&passthrough_args)
        .run()?;

    if let (Some(context), Some(spec)) = (test_context.as_ref(), result_parse.as_ref()) {
        run_declared_result_parser(component, context, spec, &output.stdout, run_dir)?;
    }

    let mut test_counts = parse_test_results_file_with_spec(&results_file, result_parse.as_ref())
        .or_else(|| {
            result_parse
                .as_ref()
                .and_then(|spec| parse_test_results_text_with_spec(&output.stdout, spec))
                .or_else(|| parse_test_results_text(&output.stdout))
        });
    let phpunit_no_discovery = phpunit_no_discovery(&output.stdout, &output.stderr);
    if phpunit_no_discovery && test_counts.is_none() {
        test_counts = Some(TestCounts::new(0, 0, 0, 0));
    }
    let require_phpunit_tests = setting_truthy(&args.settings, REQUIRE_PHPUNIT_TESTS_SETTING);

    // Autofix is owned by `refactor --from test --write`; the test command is read-only.
    let test_autofix: Option<AppliedRefactor> = None;

    let status = test_run_status(
        output.success,
        test_counts.as_ref(),
        phpunit_no_discovery,
        require_phpunit_tests,
    );

    let coverage = coverage_file
        .as_ref()
        .and_then(|file| parse_coverage_file(file).ok());

    let failure_analysis_input = parse_failures_file(&failures_file);
    let findings = failure_analysis_input
        .as_ref()
        .and_then(homeboy_findings_from_test_analysis_input);

    let analysis = if args.analyze {
        let analysis_input = failure_analysis_input
            .clone()
            .unwrap_or_else(|| TestAnalysisInput {
                failures: Vec::new(),
                total: test_counts.as_ref().map(|counts| counts.total).unwrap_or(0),
                passed: test_counts
                    .as_ref()
                    .map(|counts| counts.passed)
                    .unwrap_or(0),
            });

        Some(analyze(&args.component_id, &analysis_input))
    } else {
        None
    };

    if args.baseline_flags.baseline && !phpunit_no_discovery {
        if let Some(ref counts) = test_counts {
            let _ = baseline::save_baseline(source_path, &args.component_id, counts)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_flags.baseline
        && !args.baseline_flags.ignore_baseline
        && !phpunit_no_discovery
    {
        if let Some(ref counts) = test_counts {
            let resolved_baseline = baseline::load_baseline(source_path).or_else(|| {
                args.changed_since.as_ref().and_then(|git_ref| {
                    baseline::load_baseline_from_ref(&source_path.to_string_lossy(), git_ref)
                })
            });

            if let Some(existing_baseline) = resolved_baseline {
                let comparison = baseline::compare(counts, &existing_baseline);

                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if (comparison.passed_delta > 0 || comparison.failed_delta < 0)
                    && args.baseline_flags.ratchet
                {
                    let _ = baseline::save_baseline(source_path, &args.component_id, counts);
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let mut hints = Vec::new();

    if status == "failed" && args.passthrough_args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy test {} -- --filter=TestName",
            args.component_id
        ));
    }

    if phpunit_no_discovery {
        if require_phpunit_tests {
            hints.push(format!(
                "PHPUnit discovery is required by {}=true, but no PHPUnit test files were found.",
                REQUIRE_PHPUNIT_TESTS_SETTING
            ));
        } else {
            hints.push(format!(
                "Set --setting {}=true when this component is expected to contain PHPUnit tests.",
                REQUIRE_PHPUNIT_TESTS_SETTING
            ));
        }
    }

    if !args.skip_lint {
        hints.push(format!(
            "Auto-fix lint issues: homeboy refactor {} --from lint --write",
            args.component_id
        ));
    }

    if !coverage_enabled {
        hints.push(format!(
            "Collect coverage: homeboy test {} --coverage",
            args.component_id
        ));
    }

    if test_counts.is_some()
        && !phpunit_no_discovery
        && !args.baseline_flags.baseline
        && baseline_comparison.is_none()
    {
        hints.push(format!(
            "Save test baseline: homeboy test {} --baseline",
            args.component_id
        ));
    }

    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: homeboy test {} --ratchet",
            args.component_id
        ));
    }

    if status == "failed" && !args.analyze {
        hints.push(format!(
            "Analyze failures: homeboy test {} --analyze",
            args.component_id
        ));
    }

    if args.passthrough_args.is_empty() {
        hints.push("Pass args to test runner: homeboy test <component> -- [args]".to_string());
    }

    hints.push("Full options: homeboy docs commands/test".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };
    let test_exit_code = match status {
        "passed" | "skipped" => 0,
        "failed" if output.exit_code == 0 => 1,
        _ => output.exit_code,
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

    // When the run failed, surface a tail of the runner's stdout/stderr so the
    // user can see the actual runner output — including
    // bootstrap errors like database connection failures that produce zero
    // parsed test results. Without this, `status: failed, exit_code: 1, 0
    // tests ran` leaves the user guessing. (#1143)
    let raw_output = if status == "failed" {
        let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, RAW_OUTPUT_TAIL_LINES);
        let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, RAW_OUTPUT_TAIL_LINES);
        if stdout_tail.is_empty() && stderr_tail.is_empty() {
            None
        } else {
            Some(RawTestOutput {
                stdout_tail,
                stderr_tail,
                truncated: stdout_truncated || stderr_truncated,
                stdout_truncated,
                stderr_truncated,
                stdout_seen_bytes: output.stdout.len(),
                stdout_retained_bytes: output.stdout.len(),
                stderr_seen_bytes: output.stderr.len(),
                stderr_retained_bytes: output.stderr.len(),
                stdout_limit_bytes: 0,
                stderr_limit_bytes: 0,
            })
        }
    } else {
        None
    };

    // When tests failed with no parseable counts, surface a dedicated hint so
    // the user understands `raw_output` is the only signal about what went
    // wrong (typically a bootstrap error). (#1143)
    let mut hints_vec = hints.unwrap_or_default();
    if status == "failed" && test_counts.is_none() && raw_output.is_some() {
        hints_vec.insert(
            0,
            "No tests ran — the runner failed before producing results. \
             See raw_output.stderr_tail / raw_output.stdout_tail for the underlying error \
             (bootstrap failure, missing deps, DB connection, etc.)."
                .to_string(),
        );
    }
    let hints = if hints_vec.is_empty() {
        None
    } else {
        Some(hints_vec)
    };

    Ok(TestRunWorkflowResult {
        status: status.to_string(),
        component: args.component_label,
        exit_code,
        test_counts,
        findings,
        failure_analysis_input,
        coverage,
        baseline_comparison,
        analysis,
        autofix: test_autofix,
        hints,
        test_scope: changed_scope,
        summary,
        raw_output,
        extension_phase_timings: output.extension_phase_timings,
    })
}

fn run_declared_result_parser(
    component: &Component,
    context: &crate::core::extension::ExtensionExecutionContext,
    spec: &ParseSpec,
    stdout: &str,
    run_dir: &RunDir,
) -> crate::core::Result<()> {
    let Some(script_path) = spec.extension_script.as_deref() else {
        return Ok(());
    };
    let resolved_script = context.extension_path.join(script_path);
    if !resolved_script.is_file() {
        return Err(declared_result_parser_error(
            component,
            script_path,
            &resolved_script,
            "Declared test result parser script does not exist or is not a file".to_string(),
            None,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = resolved_script.metadata().map_err(|err| {
            declared_result_parser_error(
                component,
                script_path,
                &resolved_script,
                format!("Could not inspect declared test result parser script: {err}"),
                None,
            )
        })?;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(declared_result_parser_error(
                component,
                script_path,
                &resolved_script,
                "Declared test result parser script is not executable".to_string(),
                None,
            ));
        }
    }

    let results_file = run_dir.step_file(run_dir::files::TEST_RESULTS);
    let source_file = if results_file.is_file() {
        results_file
    } else {
        let stdout_file = run_dir.path().join("test-output.txt");
        local_files::write_file_atomic(&stdout_file, stdout, "write test runner stdout")?;
        stdout_file
    };

    let mut args = vec![source_file.to_string_lossy().to_string()];
    args.extend(spec.adapters.iter().cloned());
    let settings_json = "{}";
    let mut env_vars = crate::core::extension::execution::build_capability_env(
        &context.extension_id,
        &component.id,
        &context.extension_path,
        std::path::Path::new(&component.local_path),
        settings_json,
        &run_dir.legacy_env_vars(),
    )?;
    env_vars.push((
        "HOMEBOY_RESULT_PARSE_ADAPTERS".to_string(),
        spec.adapters.join(" "),
    ));

    let output = crate::core::extension::execution::execute_capability_script(
        &context.extension_path,
        script_path,
        &args,
        &env_vars,
        None,
        None,
        crate::core::extension::execution::CapabilityScriptOptions {
            passthrough: false,
            stderr_passthrough: false,
        },
    )?;
    if !output.success {
        let mut command =
            crate::core::engine::shell::quote_path(&resolved_script.to_string_lossy());
        if !args.is_empty() {
            command.push(' ');
            command.push_str(&crate::core::engine::shell::quote_args(&args));
        }
        return Err(declared_result_parser_error(
            component,
            script_path,
            &resolved_script,
            format!(
                "Declared test result parser script failed with exit code {}",
                output.exit_code
            ),
            Some((command, output.exit_code, &output.stdout, &output.stderr)),
        ));
    }

    Ok(())
}

fn declared_result_parser_error(
    component: &Component,
    script_path: &str,
    resolved_script: &Path,
    problem: String,
    command_output: Option<(String, i32, &str, &str)>,
) -> Error {
    let (command, exit_code, stdout_tail, stderr_tail) =
        if let Some((command, exit_code, stdout, stderr)) = command_output {
            let (stdout_tail, _) = tail_lines(stdout, RAW_OUTPUT_TAIL_LINES);
            let (stderr_tail, _) = tail_lines(stderr, RAW_OUTPUT_TAIL_LINES);
            (Some(command), Some(exit_code), stdout_tail, stderr_tail)
        } else {
            (None, None, String::new(), String::new())
        };

    Error::new(
        ErrorCode::ConfigInvalidValue,
        format!(
            "{} for component '{}' at {}",
            problem,
            component.id,
            resolved_script.display()
        ),
        serde_json::json!({
            "component": component.id,
            "script_path": script_path,
            "resolved_script": resolved_script.to_string_lossy(),
            "problem": problem,
            "command": command,
            "exit_code": exit_code,
            "stdout_tail": stdout_tail,
            "stderr_tail": stderr_tail,
        }),
    )
}

pub fn run_self_check_test_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> crate::core::Result<TestRunWorkflowResult> {
    let output = extension::self_check::run_self_checks_with_passthrough(
        component,
        ExtensionCapability::Test,
        source_path,
        !json_summary,
    )?;
    let status = if output.success { "passed" } else { "failed" }.to_string();
    let raw_output = (!output.success).then(|| {
        let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, RAW_OUTPUT_TAIL_LINES);
        let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, RAW_OUTPUT_TAIL_LINES);
        RawTestOutput {
            stdout_tail,
            stderr_tail,
            truncated: stdout_truncated
                || stderr_truncated
                || output.capture.stdout.truncated
                || output.capture.stderr.truncated,
            stdout_truncated: output.capture.stdout.truncated || stdout_truncated,
            stderr_truncated: output.capture.stderr.truncated || stderr_truncated,
            stdout_seen_bytes: output.capture.stdout.seen_bytes,
            stdout_retained_bytes: output.stdout.len(),
            stderr_seen_bytes: output.capture.stderr.seen_bytes,
            stderr_retained_bytes: output.stderr.len(),
            stdout_limit_bytes: output.capture.stdout.limit_bytes,
            stderr_limit_bytes: output.capture.stderr.limit_bytes,
        }
    });

    Ok(TestRunWorkflowResult {
        status,
        component: component_label,
        exit_code: output.exit_code,
        test_counts: None,
        findings: None,
        failure_analysis_input: None,
        coverage: None,
        baseline_comparison: None,
        analysis: None,
        autofix: None,
        hints: (!output.success).then(|| {
            vec![format!(
                "Fix the failing self-check command declared in {}'s homeboy.json scripts.test",
                component.id
            )]
        }),
        test_scope: None,
        summary: if json_summary {
            Some(build_test_summary(None, None, output.exit_code))
        } else {
            None
        },
        raw_output,
        extension_phase_timings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::ComponentScriptsConfig;
    use crate::core::extension::test::TestFailure;

    #[test]
    fn tail_lines_returns_full_text_when_under_limit() {
        let input = "line 1\nline 2\nline 3";
        let (tail, truncated) = tail_lines(input, 10);
        assert_eq!(tail, input);
        assert!(!truncated);
    }

    #[test]
    fn tail_lines_handles_empty_input() {
        let (tail, truncated) = tail_lines("", 10);
        assert_eq!(tail, "");
        assert!(!truncated);
    }

    #[test]
    fn tail_lines_at_exact_limit_is_not_truncated() {
        let input = "a\nb\nc";
        let (tail, truncated) = tail_lines(input, 3);
        assert_eq!(tail, input);
        assert!(!truncated);
    }

    #[test]
    fn test_findings_from_analysis_input_preserve_failure_details() {
        let input = TestAnalysisInput {
            failures: vec![TestFailure {
                test_name: "tests::fails".to_string(),
                test_file: "tests/fails.rs".to_string(),
                error_type: "AssertionFailed".to_string(),
                message: "expected true".to_string(),
                source_file: "src/lib.rs".to_string(),
                source_line: 42,
            }],
            total: 2,
            passed: 1,
        };

        let findings = homeboy_findings_from_test_analysis_input(&input).expect("findings");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].metadata_json()["test_name"], "tests::fails");
        assert_eq!(findings[0].message, "AssertionFailed: expected true");
        assert_eq!(findings[0].location.file.as_deref(), Some("tests/fails.rs"));
        assert_eq!(findings[0].location.line, Some(42));
    }

    #[test]
    fn status_requires_successful_runner_even_with_zero_failures() {
        let counts = TestCounts::new(3, 3, 0, 0);
        assert_eq!(
            test_run_status(false, Some(&counts), false, false),
            "failed"
        );
    }

    #[test]
    fn status_passes_successful_runner_with_zero_failures() {
        let counts = TestCounts::new(3, 3, 0, 0);
        assert_eq!(test_run_status(true, Some(&counts), false, false), "passed");
    }

    #[test]
    fn status_fails_successful_runner_with_parsed_failures() {
        let counts = TestCounts::new(3, 2, 1, 0);
        assert_eq!(test_run_status(true, Some(&counts), false, false), "failed");
    }

    #[test]
    fn status_skips_successful_phpunit_no_discovery_by_default() {
        assert_eq!(test_run_status(true, None, true, false), "skipped");
    }

    #[test]
    fn status_fails_successful_phpunit_no_discovery_when_required() {
        assert_eq!(test_run_status(true, None, true, true), "failed");
    }

    #[test]
    fn detects_phpunit_no_discovery_marker() {
        assert!(phpunit_no_discovery(
            "NO PHPUNIT TEST FILES DISCOVERED\nSkipping PHPUnit tests",
            ""
        ));
        assert!(!phpunit_no_discovery("smoke scripts passed", ""));
    }

    #[test]
    fn setting_truthy_accepts_boolean_spellings() {
        let settings = vec![(REQUIRE_PHPUNIT_TESTS_SETTING.to_string(), "yes".to_string())];
        assert!(setting_truthy(&settings, REQUIRE_PHPUNIT_TESTS_SETTING));
    }

    #[test]
    fn declared_result_parser_script_normalizes_wp_codebox_json() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let extension_dir = temp_dir.path().join("extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let parser_script = extension_dir.join("parse-results.sh");
        std::fs::write(
            &parser_script,
            r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${2:-}" != "wp-codebox-json" ]; then
    exit 7
fi
source "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS"
parsed=$(python3 - "$1" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

summary = data.get("summary") if isinstance(data.get("summary"), dict) else {}
total = int(summary.get("total") or 0)
passed = int(summary.get("passed") or 0)
failed = int(summary.get("failed") or 0)
skipped = int(summary.get("skipped") or 0)

if total == 0:
    for suite in data.get("suites") or []:
        if not isinstance(suite, dict):
            continue
        total += int(suite.get("tests") or suite.get("total") or 0)
        passed += int(suite.get("passed") or 0)
        failed += int(suite.get("failed") or 0)
        skipped += int(suite.get("skipped") or 0)

print(f"{total}\t{passed}\t{failed}\t{skipped}")
PY
)
IFS=$'\t' read -r total passed failed skipped <<EOF
$parsed
EOF
homeboy_write_test_results "$total" "$passed" "$failed" "$skipped"
"#,
        )
        .expect("parser script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parser_script, std::fs::Permissions::from_mode(0o755))
                .expect("parser script permissions");
        }

        let component = Component::new(
            "fixture".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            "fixture-extension".to_string(),
            None,
        );
        let context = crate::core::extension::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir,
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
        };
        let spec = ParseSpec {
            extension_script: Some("parse-results.sh".to_string()),
            adapters: vec!["wp-codebox-json".to_string()],
            rules: Vec::new(),
            defaults: std::collections::HashMap::new(),
            derive: Vec::new(),
        };
        let run_dir = RunDir::create().expect("run dir");

        run_declared_result_parser(
            &component,
            &context,
            &spec,
            r#"{
                "schema": "wp-codebox/test-results/v1",
                "summary": { "total": 0 },
                "suites": [
                    { "tests": 3, "passed": 2, "failed": 1 },
                    { "total": 2, "passed": 1, "skipped": 1 }
                ]
            }"#,
            &run_dir,
        )
        .expect("declared parser should run");

        let counts = parse_test_results_file_with_spec(
            &run_dir.step_file(run_dir::files::TEST_RESULTS),
            Some(&spec),
        )
        .expect("declared parser should write normalized counts");

        run_dir.cleanup();

        assert_eq!(counts.total, 5);
        assert_eq!(counts.passed, 3);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.skipped, 1);
    }

    #[test]
    fn declared_result_parser_errors_when_script_is_missing() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let extension_dir = temp_dir.path().join("extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let component = Component::new(
            "fixture".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            "fixture-extension".to_string(),
            None,
        );
        let context = crate::core::extension::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir.clone(),
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
        };
        let spec = ParseSpec {
            extension_script: Some("missing-parser.sh".to_string()),
            adapters: vec!["fixture-json".to_string()],
            rules: Vec::new(),
            defaults: std::collections::HashMap::new(),
            derive: Vec::new(),
        };
        let run_dir = RunDir::create().expect("run dir");

        let err = run_declared_result_parser(&component, &context, &spec, "{}", &run_dir)
            .expect_err("declared missing parser should fail");
        run_dir.cleanup();

        assert_eq!(err.code, ErrorCode::ConfigInvalidValue);
        assert!(err
            .message
            .contains("Declared test result parser script does not exist"));
        assert!(err.message.contains("missing-parser.sh"));
        assert_eq!(err.details["script_path"], "missing-parser.sh");
        assert_eq!(
            err.details["resolved_script"].as_str(),
            Some(
                extension_dir
                    .join("missing-parser.sh")
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[test]
    fn declared_result_parser_errors_with_context_on_non_zero_exit() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let extension_dir = temp_dir.path().join("extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let parser_script = extension_dir.join("parse-results.sh");
        std::fs::write(
            &parser_script,
            r#"#!/usr/bin/env bash
printf 'parser stdout detail\n'
printf 'parser stderr detail\n' >&2
exit 23
"#,
        )
        .expect("parser script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parser_script, std::fs::Permissions::from_mode(0o755))
                .expect("parser script permissions");
        }

        let component = Component::new(
            "fixture".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            "fixture-extension".to_string(),
            None,
        );
        let context = crate::core::extension::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir,
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
        };
        let spec = ParseSpec {
            extension_script: Some("parse-results.sh".to_string()),
            adapters: vec!["fixture-json".to_string()],
            rules: Vec::new(),
            defaults: std::collections::HashMap::new(),
            derive: Vec::new(),
        };
        let run_dir = RunDir::create().expect("run dir");

        let err = run_declared_result_parser(&component, &context, &spec, "{}", &run_dir)
            .expect_err("declared parser non-zero exit should fail");
        run_dir.cleanup();

        assert_eq!(err.code, ErrorCode::ConfigInvalidValue);
        assert!(err.message.contains("exit code 23"));
        assert_eq!(err.details["script_path"], "parse-results.sh");
        assert_eq!(err.details["exit_code"], 23);
        assert!(err.details["command"]
            .as_str()
            .unwrap_or_default()
            .contains("parse-results.sh"));
        assert!(err.details["stdout_tail"]
            .as_str()
            .unwrap_or_default()
            .contains("parser stdout detail"));
        assert!(err.details["stderr_tail"]
            .as_str()
            .unwrap_or_default()
            .contains("parser stderr detail"));
    }

    #[test]
    fn test_run_self_check_test_workflow() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("test.sh"), "printf test-ok\n")
            .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: Vec::new(),
            test: vec!["sh test.sh".to_string()],
            build: Vec::new(),
            bench: Vec::new(),
            trace: Vec::new(),
            deps: Vec::new(),
        });

        let result =
            run_self_check_test_workflow(&component, dir.path(), "fixture".to_string(), true)
                .expect("test self-check should run");

        assert_eq!(result.status, "passed");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.component, "fixture");
        assert!(result.summary.is_some());
    }
}
