use crate as extension;
use crate::runner::tail_lines;
use crate::test::analyze::{analyze, TestAnalysis, TestAnalysisInput};
use crate::test::baseline::{self, TestBaselineComparison, TestCounts};
use crate::test::{
    build_test_runner, build_test_summary, compute_changed_test_scope,
    normalize_test_passthrough_args, parse_coverage_file, parse_failures_file,
    parse_test_results_file_with_spec, parse_test_results_text, parse_test_results_text_with_spec,
    CoverageOutput, TestScopeOutput, TestSummaryOutput,
};
use crate::{ExtensionCapability, ExtensionPhaseTiming};
use homeboy_core::component::Component;
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::error::{Error, ErrorCode};
use homeboy_core::finding::HomeboyFinding;
use homeboy_core::observation::homeboy_findings_from_test_analysis_input;
use homeboy_core::validation_progress::{write_command_artifact, ValidationProgressRecorder};
use homeboy_engine_primitives::baseline::BaselineFlags;
use homeboy_engine_primitives::local_files;
use homeboy_engine_primitives::output_parse::ParseSpec;
pub use homeboy_extension_contract::test_results::TestRunWorkflowResult;
pub use homeboy_extension_contract::test_workflow::RawTestOutput;
use homeboy_refactor_contract::AppliedRefactor;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct TestRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub skip_lint: bool,
    pub coverage: bool,
    pub coverage_min: Option<f64>,
    pub analyze: bool,
    pub baseline_flags: BaselineFlags,
    pub changed_since: Option<String>,
    pub precomputed_changed_files: Option<Vec<String>>,
    pub json_summary: bool,
    pub restore_checkout: bool,
    pub ci_env: Vec<(String, String)>,
    pub passthrough_args: Vec<String>,
}

const RAW_OUTPUT_TAIL_LINES: usize = 80;
const PHPUNIT_NO_DISCOVERY_MARKER: &str = "NO PHPUNIT TEST FILES DISCOVERED";
const REQUIRE_PHPUNIT_TESTS_SETTING: &str = "require_phpunit_tests";

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
    source_path: &Path,
    args: TestRunWorkflowArgs,
    run_dir: &RunDir,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    if !args.restore_checkout {
        return run_main_test_workflow_inner(component, source_path, args, run_dir);
    }

    let component_label = args.component_label.clone();
    let json_summary = args.json_summary;
    run_review_test_lifecycle(source_path, component_label, json_summary, || {
        run_main_test_workflow_inner(component, source_path, args, run_dir)
    })
}

fn run_main_test_workflow_inner(
    component: &Component,
    source_path: &Path,
    args: TestRunWorkflowArgs,
    run_dir: &RunDir,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    let changed_scope = if let Some(ref git_ref) = args.changed_since {
        Some(match args.precomputed_changed_files.as_ref() {
            Some(changed_files) => crate::test::compute_changed_test_scope_for_files(
                component,
                git_ref,
                changed_files,
            )?,
            None => compute_changed_test_scope(component, git_ref)?,
        })
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
            let changed_ref = scope.changed_since.as_deref().unwrap_or("unknown");

            // Fail closed when production/test source changed but the scope
            // selected zero tests: passing green there is not release evidence,
            // it just means the change-to-test mapping missed the impacted
            // files. Documentation/config-only changes leave
            // `source_changes_without_tests` empty and still pass. (#8340)
            if !scope.source_changes_without_tests.is_empty() {
                let impacted = &scope.source_changes_without_tests;
                let preview = impacted
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = impacted.len().saturating_sub(10);
                let impacted_summary = if more > 0 {
                    format!("{preview}, and {more} more")
                } else {
                    preview
                };

                let message = format!(
                    "Changed-scope test gate selected zero tests, but {} source file(s) changed since {changed_ref}: {impacted_summary}. Zero selection is not valid test evidence for a source change.",
                    impacted.len(),
                );
                let findings = Some(vec![HomeboyFinding::builder("test", message.clone())
                    .rule("changed_scope_zero_tests_for_source_change")
                    .category("test-scope")
                    .severity("error")
                    .build()]);
                let hints = Some(vec![
                    format!(
                        "Add or route a test for the changed source, or run the full suite: homeboy test {}",
                        args.component_id
                    ),
                    "If these changes are intentionally test-exempt, exclude them from the release/test scope so the gate can pass with a typed reason.".to_string(),
                ]);

                return Ok(TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: args.component_label,
                    exit_code: 1,
                    test_counts: None,
                    findings,
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

            // No source-relevant change: a genuine no-test scope
            // (documentation/config only) may pass.
            let hints = Some(vec![
                format!(
                    "No impacted tests found for --changed-since {changed_ref} (no production or test source changed)"
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

    let test_context = crate::test::resolve_test_command(component).ok();
    let result_parse = test_context
        .as_ref()
        .and_then(|context| crate::load_extension(&context.extension_id).ok())
        .and_then(|extension| extension.test.and_then(|test| test.result_parse));

    let runner = build_test_runner(
        component,
        args.path_override.clone(),
        &args.settings,
        &args.settings_json,
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
    let mut progress = ValidationProgressRecorder::new(
        run_dir,
        None,
        vec![("test runner".to_string(), args.component_label.clone())],
    )?;
    progress.start(0)?;
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
    let stdout_artifact = write_command_artifact(run_dir, 0, "stdout", &output.stdout)?;
    let stderr_artifact = write_command_artifact(run_dir, 0, "stderr", &output.stderr)?;
    progress.finish(0, output.exit_code, stdout_artifact, stderr_artifact)?;

    if let (Some(context), Some(spec)) = (test_context.as_ref(), result_parse.as_ref()) {
        run_declared_result_parser(component, context, spec, &output.stdout, run_dir)?;
    }

    let mut test_counts = parse_test_results_file_with_spec(&results_file, result_parse.as_ref())?
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
        .as_deref()
        .map(parse_coverage_file)
        .transpose()?
        .flatten();

    let failure_analysis_input = parse_failures_file(&failures_file)?;
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

    hints.push("Full options: homeboy self docs commands/test".to_string());

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
    // wrong. A missing sidecar does not prove that no tests executed.
    let mut hints_vec = hints.unwrap_or_default();
    if status == "failed" && test_counts.is_none() && raw_output.is_some() {
        hints_vec.insert(
            0,
            "The test runner failed before producing structured results. \
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

struct TestCheckoutGuard {
    path: std::path::PathBuf,
    head: String,
}

fn run_review_test_lifecycle(
    source_path: &Path,
    component: String,
    json_summary: bool,
    run: impl FnOnce() -> homeboy_core::Result<TestRunWorkflowResult>,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    let guard = TestCheckoutGuard::capture(source_path)?;
    let result =
        run().unwrap_or_else(|error| failed_test_workflow(component, json_summary, &error));
    guard.restore()?;
    Ok(result)
}

impl TestCheckoutGuard {
    fn capture(path: &Path) -> homeboy_core::Result<Self> {
        let changes = homeboy_core::git::get_uncommitted_changes(&path.to_string_lossy())?;
        if changes.has_changes {
            let files = changes
                .staged
                .iter()
                .chain(changes.unstaged.iter())
                .chain(changes.untracked.iter())
                .take(10)
                .cloned()
                .collect::<Vec<_>>();
            return Err(Error::validation_invalid_argument(
                "working_tree",
                "Review tests require a clean component checkout",
                None,
                Some(vec![format!("Dirty files: {}", files.join(", "))]),
            ));
        }

        let head =
            homeboy_core::git::run_git(path, &["rev-parse", "HEAD"], "capture review test HEAD")?;
        Ok(Self {
            path: path.to_path_buf(),
            head: head.trim().to_string(),
        })
    }

    fn restore(&self) -> homeboy_core::Result<()> {
        homeboy_core::git::run_git(
            &self.path,
            &["reset", "--hard", &self.head],
            "restore review test checkout",
        )?;
        homeboy_core::git::run_git(
            &self.path,
            &["clean", "-fd"],
            "remove review test artifacts",
        )?;

        let changes = homeboy_core::git::get_uncommitted_changes(&self.path.to_string_lossy())?;
        if changes.has_changes {
            return Err(Error::internal_unexpected(
                "review test checkout remained dirty after restoration",
            ));
        }
        Ok(())
    }
}

fn failed_test_workflow(
    component: String,
    json_summary: bool,
    error: &Error,
) -> TestRunWorkflowResult {
    let message = error.to_string();
    TestRunWorkflowResult {
        status: "failed".to_string(),
        component,
        exit_code: 2,
        test_counts: None,
        findings: None,
        failure_analysis_input: None,
        coverage: None,
        baseline_comparison: None,
        analysis: None,
        autofix: None,
        hints: Some(vec![
            "The test runner failed during setup or execution; inspect raw_output.stderr_tail"
                .to_string(),
        ]),
        test_scope: None,
        summary: json_summary.then(|| build_test_summary(None, None, 2)),
        raw_output: Some(RawTestOutput {
            stdout_tail: String::new(),
            stderr_tail: message.clone(),
            truncated: false,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_seen_bytes: 0,
            stdout_retained_bytes: 0,
            stderr_seen_bytes: message.len(),
            stderr_retained_bytes: message.len(),
            stdout_limit_bytes: 0,
            stderr_limit_bytes: 0,
        }),
        extension_phase_timings: Vec::new(),
    }
}

fn run_declared_result_parser(
    component: &Component,
    context: &crate::ExtensionExecutionContext,
    spec: &ParseSpec,
    stdout: &str,
    run_dir: &RunDir,
) -> homeboy_core::Result<()> {
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

    std::fs::create_dir_all(run_dir.path()).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create declared result parser run dir".to_string()),
        )
    })?;

    let results_file = run_dir.step_file(run_dir::files::TEST_RESULTS);
    let source_file = if results_file.is_file() {
        results_file
    } else {
        let stdout_file = run_dir.path().join("test-output.txt");
        if let Some(parent) = stdout_file.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("create parser stdout source directory".to_string()),
                )
            })?;
        }
        local_files::write_file_atomic(&stdout_file, stdout, "write test runner stdout")?;
        stdout_file
    };

    let mut args = vec![source_file.to_string_lossy().to_string()];
    args.extend(spec.adapters.iter().cloned());
    let settings_json = "{}";
    let mut env_vars = crate::execution::build_capability_env(
        &context.extension_id,
        &component.id,
        &context.extension_path,
        std::path::Path::new(&component.local_path),
        settings_json,
        &run_dir.legacy_env_vars(),
    )?;
    let write_results_helper = run_dir.path().join("write-test-results.sh");
    local_files::write_file_atomic(
        &write_results_helper,
        include_str!("../runtime/write-test-results.sh"),
        "write parser runtime helper",
    )?;
    env_vars.push((
        "HOMEBOY_RUNTIME_WRITE_TEST_RESULTS".to_string(),
        write_results_helper.to_string_lossy().to_string(),
    ));
    env_vars.push((
        "HOMEBOY_TEST_RESULTS_FILE".to_string(),
        run_dir
            .step_file(run_dir::files::TEST_RESULTS)
            .to_string_lossy()
            .to_string(),
    ));
    env_vars.push((
        "HOMEBOY_RESULT_PARSE_ADAPTERS".to_string(),
        spec.adapters.join(" "),
    ));

    let output = crate::execution::execute_capability_script(
        &context.extension_path,
        script_path,
        &args,
        &env_vars,
        None,
        None,
        crate::execution::CapabilityScriptOptions {
            passthrough: false,
            stderr_passthrough: false,
            timeout: None,
        },
    )?;
    if !output.success {
        let mut command =
            homeboy_engine_primitives::shell::quote_path(&resolved_script.to_string_lossy());
        if !args.is_empty() {
            command.push(' ');
            command.push_str(&homeboy_engine_primitives::shell::quote_args(&args));
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

    if !run_dir.step_file(run_dir::files::TEST_RESULTS).is_file() {
        let parser_stdout = output.stdout.trim();
        if !parser_stdout.is_empty() {
            let counts = parse_declared_parser_stdout_json(parser_stdout)?;
            let payload = serde_json::json!({
                "total": counts.total,
                "passed": counts.passed,
                "failed": counts.failed,
                "skipped": counts.skipped,
            });
            let results_path = run_dir.step_file(run_dir::files::TEST_RESULTS);
            if let Some(parent) = results_path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some("create parser stdout test results directory".to_string()),
                    )
                })?;
            }
            local_files::write_file_atomic(
                &results_path,
                &serde_json::to_string_pretty(&payload).map_err(|err| {
                    Error::internal_json(
                        err.to_string(),
                        Some("serialize parser stdout test results".to_string()),
                    )
                })?,
                "write parser stdout test results",
            )?;
        }
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

fn parse_declared_parser_stdout_json(stdout: &str) -> homeboy_core::Result<TestCounts> {
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some("parse test result adapter stdout".to_string()),
            Some(stdout.to_string()),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        Error::validation_invalid_argument(
            "test.result_parse.extension_script.stdout",
            "expected a JSON object with unsigned integer total, passed, failed, and skipped fields",
            None,
            None,
        )
    })?;

    let count = |field: &str| -> homeboy_core::Result<u64> {
        object
            .get(field)
            .and_then(|value| value.as_u64())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    format!("test.result_parse.extension_script.stdout.{field}"),
                    "expected an unsigned integer count",
                    None,
                    None,
                )
            })
    };

    Ok(TestCounts::new(
        count("total")?,
        count("passed")?,
        count("failed")?,
        count("skipped")?,
    ))
}

pub fn run_self_check_test_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    run_self_check_test_workflow_with_progress(
        component,
        source_path,
        component_label,
        json_summary,
        None,
        None,
    )
}

pub fn run_self_check_test_workflow_with_progress(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
    run_dir: Option<&RunDir>,
    observation: Option<&homeboy_core::observation::ActiveObservation>,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    let output = extension::self_check::run_self_checks_with_passthrough_and_progress(
        component,
        ExtensionCapability::Test,
        source_path,
        !json_summary,
        run_dir,
        observation,
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
    use crate::test::TestFailure;
    use homeboy_core::component::ComponentScriptsConfig;
    use homeboy_core::test_support::{exec_capable_tempdir, with_isolated_home};

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn clean_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("temp dir");
        run_git(temp.path(), &["init", "-q", "--initial-branch", "main"]);
        run_git(
            temp.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(temp.path(), &["config", "user.name", "Homeboy Test"]);
        std::fs::write(temp.path().join("tracked.txt"), "original\n").expect("tracked file");
        run_git(temp.path(), &["add", "tracked.txt"]);
        run_git(temp.path(), &["commit", "-q", "-m", "fixture"]);
        temp
    }

    fn assert_clean(dir: &Path) {
        assert_eq!(run_git(dir, &["status", "--porcelain=v1"]), "");
        assert_eq!(
            std::fs::read_to_string(dir.join("tracked.txt")).expect("tracked file"),
            "original\n"
        );
        assert!(!dir.join("generated.txt").exists());
    }

    #[test]
    fn setup_failure_returns_structured_result_and_restores_clean_checkout() {
        let repo = clean_repo();

        let result = run_review_test_lifecycle(repo.path(), "fixture".to_string(), true, || {
            std::fs::write(repo.path().join("tracked.txt"), "setup mutation\n")
                .expect("mutate tracked file");
            std::fs::write(repo.path().join("generated.txt"), "setup artifact\n")
                .expect("write setup artifact");
            Err(Error::internal_unexpected("fixture setup failed"))
        })
        .expect("setup failure should become a test result");
        let (output, exit_code) = super::super::report::from_main_workflow(result);
        let json = serde_json::to_value(output).expect("structured output");

        assert_eq!(exit_code, 2);
        assert_eq!(json["passed"], false);
        assert_eq!(json["status"], "failed");
        assert_eq!(json["failure"]["category"], "infrastructure");
        assert!(json["raw_output"]["stderr_tail"]
            .as_str()
            .unwrap_or_default()
            .contains("fixture setup failed"));
        assert_clean(repo.path());
    }

    #[test]
    fn test_failure_returns_structured_result_and_restores_clean_checkout() {
        let repo = clean_repo();

        let result = run_review_test_lifecycle(repo.path(), "fixture".to_string(), true, || {
            std::fs::write(repo.path().join("tracked.txt"), "test mutation\n")
                .expect("mutate tracked file");
            std::fs::write(repo.path().join("generated.txt"), "test artifact\n")
                .expect("write test artifact");
            Ok(TestRunWorkflowResult {
                status: "failed".to_string(),
                component: "fixture".to_string(),
                exit_code: 1,
                test_counts: Some(TestCounts::new(1, 0, 1, 0)),
                findings: None,
                failure_analysis_input: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints: None,
                test_scope: None,
                summary: Some(build_test_summary(
                    Some(&TestCounts::new(1, 0, 1, 0)),
                    None,
                    1,
                )),
                raw_output: None,
                extension_phase_timings: Vec::new(),
            })
        })
        .expect("test failure should remain a test result");
        let (output, exit_code) = super::super::report::from_main_workflow(result);
        let json = serde_json::to_value(output).expect("structured output");

        assert_eq!(exit_code, 1);
        assert_eq!(json["passed"], false);
        assert_eq!(json["test_counts"]["failed"], 1);
        assert_eq!(json["failure"]["category"], "findings");
        assert_clean(repo.path());
    }

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
    fn declared_result_parser_script_normalizes_provider_json() {
        with_isolated_home(|_| {
            // Use an exec-capable tempdir: these tests write a parser script
            // and execute it, so a `noexec` $TMPDIR (e.g. hardened `/tmp`)
            // would fail with exit 126 regardless of the behavior under test.
            let temp_dir = exec_capable_tempdir();
            let extension_dir = temp_dir.path().join("extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            let parser_script = extension_dir.join("parse-results.sh");
            std::fs::write(
                &parser_script,
                r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${2:-}" != "custom-json" ]; then
    exit 7
fi
if [ ! -f "$1" ]; then
    printf 'expected parser input file to exist: %s\n' "$1" >&2
    exit 8
fi
if ! grep -q 'custom-provider/test-results/v1' "$1"; then
    printf 'expected parser input file to contain provider JSON\n' >&2
    exit 9
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
printf '{"total":%s,"passed":%s,"failed":%s,"skipped":%s}\n' "$total" "$passed" "$failed" "$skipped"
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
            let context = crate::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Test,
                extension_id: "fixture-extension".to_string(),
                extension_path: extension_dir,
                script_path: "test.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
            };
            let spec = ParseSpec {
                extension_script: Some("parse-results.sh".to_string()),
                adapters: vec!["custom-json".to_string()],
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
                "schema": "custom-provider/test-results/v1",
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
            let counts = counts.expect("normalized counts should be present");

            run_dir.cleanup();

            assert_eq!(counts.total, 5);
            assert_eq!(counts.passed, 3);
            assert_eq!(counts.failed, 1);
            assert_eq!(counts.skipped, 1);
        });
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
        let context = crate::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir.clone(),
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
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
        // This test executes a capability script, which builds an env derived
        // from HOME / homeboy paths. Run it under the shared `home_lock()` so it
        // is globally serialized against env-mutating tests instead of racing
        // them under default parallelism (#6760, #6804).
        with_isolated_home(|_| {
            // Use an exec-capable tempdir: these tests write a parser script
            // and execute it, so a `noexec` $TMPDIR (e.g. hardened `/tmp`)
            // would fail with exit 126 regardless of the behavior under test.
            let temp_dir = exec_capable_tempdir();
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
            let context = crate::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Test,
                extension_id: "fixture-extension".to_string(),
                extension_path: extension_dir,
                script_path: "test.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
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
        });
    }

    #[test]
    fn declared_result_parser_accepts_flat_count_stdout_json() {
        with_isolated_home(|_| {
            // Use an exec-capable tempdir: these tests write a parser script
            // and execute it, so a `noexec` $TMPDIR (e.g. hardened `/tmp`)
            // would fail with exit 126 regardless of the behavior under test.
            let temp_dir = exec_capable_tempdir();
            let extension_dir = temp_dir.path().join("extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            let parser_script = extension_dir.join("parse-results.sh");
            std::fs::write(
                &parser_script,
                r#"#!/usr/bin/env bash
set -euo pipefail
printf '{"total":5,"passed":3,"failed":1,"skipped":1}\n'
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
            let context = crate::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Test,
                extension_id: "fixture-extension".to_string(),
                extension_path: extension_dir,
                script_path: "test.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
            };
            let spec = ParseSpec {
                extension_script: Some("parse-results.sh".to_string()),
                adapters: vec!["fixture-json".to_string()],
                rules: Vec::new(),
                defaults: std::collections::HashMap::new(),
                derive: Vec::new(),
            };
            let run_dir = RunDir::create().expect("run dir");

            run_declared_result_parser(&component, &context, &spec, "runner output", &run_dir)
                .expect("declared parser stdout should run");

            let counts = parse_test_results_file_with_spec(
                &run_dir.step_file(run_dir::files::TEST_RESULTS),
                Some(&spec),
            )
            .expect("parser stdout JSON should be normalized to test-results.json");
            let counts = counts.expect("normalized counts should be present");

            run_dir.cleanup();

            assert_eq!(counts.total, 5);
            assert_eq!(counts.passed, 3);
            assert_eq!(counts.failed, 1);
            assert_eq!(counts.skipped, 1);
        });
    }

    #[test]
    fn declared_result_parser_rejects_malformed_successful_stdout_json() {
        // Exec-capable tempdir: this test runs the parser script, so a
        // `noexec` $TMPDIR would fail with exit 126 before reaching the
        // malformed-JSON assertion under test.
        let temp_dir = exec_capable_tempdir();
        let extension_dir = temp_dir.path().join("extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let parser_script = extension_dir.join("parse-results.sh");
        std::fs::write(
            &parser_script,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'not json\n'
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
        let context = crate::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir,
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
        };
        let spec = ParseSpec {
            extension_script: Some("parse-results.sh".to_string()),
            adapters: vec!["fixture-json".to_string()],
            rules: Vec::new(),
            defaults: std::collections::HashMap::new(),
            derive: Vec::new(),
        };
        let run_dir = RunDir::create().expect("run dir");

        let error =
            run_declared_result_parser(&component, &context, &spec, "runner output", &run_dir)
                .expect_err("malformed parser stdout should fail");

        run_dir.cleanup();

        assert!(error.message.contains("Invalid JSON"));
        assert_eq!(error.code.as_str(), "validation.invalid_json");
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
            fuzz: Vec::new(),
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
