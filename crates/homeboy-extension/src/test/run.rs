use crate as extension;
use crate::runner::tail_lines;
use crate::test::analyze::{analyze, TestAnalysis, TestAnalysisInput};
use crate::test::baseline::{self, TestBaselineComparison, TestCounts};
use crate::test::durations::{
    build_test_durations, parse_duration_samples, parse_test_durations_file, SlowTestPolicy,
    TestDurations,
};
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
use homeboy_engine_primitives::measurement::{Measurement, Verdict};
use homeboy_engine_primitives::output_parse::ParseSpec;
pub use homeboy_extension_contract::test_results::TestRunWorkflowResult;
pub use homeboy_extension_contract::test_workflow::RawTestOutput;
use homeboy_refactor_contract::AppliedRefactor;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

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
const COMPILER_FAILURE_LIMIT: usize = 20;
const NO_TESTS_APPLICABLE_SCHEMA: &str = "homeboy/no-tests-applicable/v1";
const NO_TESTS_APPLICABLE_FILE_ENV: &str = "HOMEBOY_NO_TESTS_APPLICABLE_FILE";
const NO_TESTS_APPLICABLE_NONCE_ENV: &str = "HOMEBOY_NO_TESTS_APPLICABLE_NONCE";
const NO_TESTS_APPLICABLE_EXTENSION_ENV: &str = "HOMEBOY_NO_TESTS_APPLICABLE_EXTENSION_ID";
const NO_TESTS_APPLICABLE_STEP: &str = "test";
const DEFAULT_TEST_TIMEOUT_SECONDS: u64 = 25 * 60;

pub(crate) fn test_timeout() -> Duration {
    std::env::var("HOMEBOY_TEST_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_TEST_TIMEOUT_SECONDS))
}

#[derive(Deserialize)]
struct NoTestsApplicableEvidence {
    schema: String,
    extension_id: String,
    step: String,
    nonce: String,
    reason: String,
}
/// Classify what the test runner actually measured.
///
/// Executed assertions -- `passed + failed` -- are the unit of evidence.
/// `skipped` is deliberately excluded: an all-skipped result proves only that
/// the runner started. Absent counts are [`Measurement::unreported`], which is
/// a different state from a counted zero and reads differently to an operator.
///
/// No population is supplied. The runner does not independently know how many
/// tests *should* have run (that is what `--filter` and the extension's
/// selection are for), so a zero here is honestly `ZeroUnits`
/// rather than a provably broken instrument.
fn test_measurement(test_counts: Option<&TestCounts>) -> Measurement {
    match test_counts {
        Some(counts) => Measurement::units(counts.passed + counts.failed),
        None => Measurement::unreported(),
    }
}

fn test_run_status(
    runner_success: bool,
    test_counts: Option<&TestCounts>,
    no_tests_applicable: bool,
) -> &'static str {
    if !runner_success {
        return "failed";
    }

    // The one legitimate escape from the measurement requirement, and it is
    // gated on POSITIVE evidence rather than on absence: `no_tests_applicable`
    // is only true when the extension wrote a nonce-matched, schema-matched
    // evidence file naming its reason. "The instrument reported nothing" can
    // never reach here.
    if no_tests_applicable {
        return "skipped";
    }

    // A zero count or an all-skipped result proves only that the runner
    // started. A passing test gate needs evidence that it executed a test.
    //
    // This predicate is now shared (#10685). The behaviour is unchanged in
    // every case -- see `test_run_status_matches_the_shared_predicate` -- but
    // the reasoning is no longer private to this function, and the audit and
    // lint gates answer the same question the same way.
    let intended = if test_counts.is_some_and(|counts| counts.failed == 0) {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    match test_measurement(test_counts).assess().constrain(intended) {
        Ok(Verdict::Pass) => "passed",
        // `Unknown` collapses to `"failed"` here, and only here. This status is
        // a published string in the command output envelope that downstream
        // consumers match on, so introducing a fourth value is a breaking
        // change rather than an additive one. The *label* is therefore lossy
        // while the *decision* is not: an unmeasured run has never rendered
        // green on this path and still does not. `test_phase_report` in
        // `report.rs` carries the distinction that a reader needs -- "test
        // runner reported zero executed tests" versus a timeout versus real
        // failures -- so nothing an operator acts on is lost.
        Ok(Verdict::Unknown) | Ok(Verdict::Fail) => "failed",
        // Unreachable on this path: `test_measurement` never establishes a
        // population, so `Contradicted` cannot be produced. Fail closed.
        Err(_) => "failed",
    }
}

fn no_tests_applicable(
    policy_enabled: bool,
    evidence_file: &Path,
    extension_id: &str,
    nonce: &str,
    test_counts: Option<&TestCounts>,
) -> bool {
    if !policy_enabled || test_counts.is_some_and(|counts| counts.passed + counts.failed > 0) {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(evidence_file) else {
        return false;
    };
    let Ok(evidence) = serde_json::from_str::<NoTestsApplicableEvidence>(&raw) else {
        return false;
    };
    evidence.schema == NO_TESTS_APPLICABLE_SCHEMA
        && evidence.extension_id == extension_id
        && evidence.step == NO_TESTS_APPLICABLE_STEP
        && evidence.nonce == nonce
        && !evidence.reason.trim().is_empty()
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
    let durations_file = run_dir.step_file(run_dir::files::TEST_DURATIONS);

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
            //
            // Restated through the shared predicate (#10685) without changing
            // behaviour: zero tests selected is the observation, and the
            // impacted source files are the independently-known population that
            // says whether that zero is honest. A non-empty population makes
            // this `Contradicted` -- a provably broken instrument, and the one
            // outcome that is a hard error rather than an `unknown`. #8340
            // reached that conclusion on its own, three months before #10685
            // named it; the two agree exactly, which is the main reason this
            // predicate is worth sharing.
            let scope_measurement = Measurement::units(scope.selected_files.len() as u64)
                .against_population(scope.source_changes_without_tests.len() as u64);
            if scope_measurement.assess().is_broken_instrument() {
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
                        "Add or route a test for the changed source, or run the full suite: homeboy review test {}",
                        args.component_id
                    ),
                    "If these changes are intentionally test-exempt, exclude them from the release/test scope so the gate can pass with a typed reason.".to_string(),
                ]);

                return Ok(TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: args.component_label,
                    exit_code: 1,
                    test_counts: None,
                    test_durations: None,
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
                    "Run full suite if needed: homeboy review test {}",
                    args.component_id
                ),
            ]);

            return Ok(TestRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                test_counts: None,
                test_durations: None,
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
    let test_config = test_context
        .as_ref()
        .and_then(|context| crate::load_extension(&context.extension_id).ok())
        .and_then(|extension| extension.test);
    let result_parse = test_config
        .as_ref()
        .and_then(|test| test.result_parse.as_ref());
    let no_tests_policy_enabled = test_config
        .as_ref()
        .and_then(|test| test.no_tests_applicable.as_ref())
        .is_some();
    let no_tests_evidence_file = run_dir.step_file(run_dir::files::NO_TESTS_APPLICABLE);
    let no_tests_nonce = uuid::Uuid::new_v4().to_string();
    let write_results_helper = write_test_results_helper(run_dir)?;

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
    let runner = runner
        .env(
            "HOMEBOY_TEST_RESULTS_FILE",
            results_file.to_string_lossy().as_ref(),
        )
        .env(
            crate::runtime_helper::WRITE_TEST_RESULTS_ENV,
            write_results_helper.to_string_lossy().as_ref(),
        )
        .env_if(
            no_tests_policy_enabled,
            NO_TESTS_APPLICABLE_FILE_ENV,
            no_tests_evidence_file.to_string_lossy().as_ref(),
        )
        .env_if(
            no_tests_policy_enabled,
            NO_TESTS_APPLICABLE_NONCE_ENV,
            &no_tests_nonce,
        )
        .env_if(
            no_tests_policy_enabled,
            NO_TESTS_APPLICABLE_EXTENSION_ENV,
            test_context
                .as_ref()
                .map(|context| context.extension_id.as_str())
                .unwrap_or_default(),
        );
    // In summary mode, capture the child's stdout/stderr into run evidence
    // instead of tee-ing the full compiler/test stream to the terminal. The
    // output is still persisted to artifacts below and a bounded failure tail
    // is surfaced by the summary, so `--summary` stays actionable on large
    // repositories instead of overflowing the caller's display limit (#9845).
    let runner = runner.passthrough(!args.json_summary);
    let passthrough_args = normalize_test_passthrough_args(component, &args.passthrough_args)?;
    let mut progress = ValidationProgressRecorder::new(
        run_dir,
        None,
        vec![("test runner".to_string(), args.component_label.clone())],
    )?;
    progress.start(0)?;
    let timeout = test_timeout();
    homeboy_core::log_status!(
        "test",
        "phase=child command=test runner timeout={}s; streaming bounded child supervision",
        timeout.as_secs()
    );
    // Homeboy's own clock. Unlike anything parsed out of runner output it is
    // always available — including when the child is killed before it prints a
    // single summary line — so the suite-level duration survives a timeout.
    let child_started = std::time::Instant::now();
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
        .timeout(Some(timeout))
        .run()?;
    let child_elapsed = child_started.elapsed().as_secs_f64();
    let stdout_artifact = write_command_artifact(run_dir, 0, "stdout", &output.stdout)?;
    let stderr_artifact = write_command_artifact(run_dir, 0, "stderr", &output.stderr)?;
    progress.finish(0, output.exit_code, stdout_artifact, stderr_artifact)?;

    if let (Some(context), Some(spec)) = (test_context.as_ref(), result_parse.as_ref()) {
        run_declared_result_parser(component, context, spec, &output.stdout, run_dir)?;
    }

    let test_counts =
        parse_test_results_file_with_spec(&results_file, result_parse)?.or_else(|| {
            result_parse
                .as_ref()
                .and_then(|spec| parse_test_results_text_with_spec(&output.stdout, spec))
                .or_else(|| parse_test_results_text(&output.stdout))
        });
    // Duration capture. Advisory throughout: it is derived from evidence that
    // already exists, it is attached to its own field, and nothing below reads
    // it when deciding status, exit code, or baseline comparison. A slow test
    // is a finding, not a failure. (#10655)
    let test_durations = collect_test_durations(
        &durations_file,
        &output.stdout,
        child_elapsed,
        output.timed_out,
        timeout,
    );

    let no_tests_applicable = no_tests_applicable(
        no_tests_policy_enabled,
        &no_tests_evidence_file,
        test_context
            .as_ref()
            .map(|context| context.extension_id.as_str())
            .unwrap_or_default(),
        &no_tests_nonce,
        test_counts.as_ref(),
    );

    // Autofix is owned by `refactor --from test --write`; the test command is read-only.
    let test_autofix: Option<AppliedRefactor> = None;

    let status = test_run_status(output.success, test_counts.as_ref(), no_tests_applicable);

    let coverage = coverage_file
        .as_deref()
        .map(parse_coverage_file)
        .transpose()?
        .flatten();

    // The failure sidecar is optional enrichment: it feeds `--analyze` findings
    // and failure classification, but the primary execution result (success,
    // counts, phase, raw output) is already resolved above. A malformed sidecar
    // must not replace that primary result with a JSON parse error and mask the
    // real underlying failure (e.g. a pre-test runtime/bind failure whose
    // structured evidence the runner already reported). Degrade to no
    // enrichment and attach the parse problem as a secondary diagnostic. (#8489)
    let (mut failure_analysis_input, sidecar_diagnostic) =
        parse_optional_failure_sidecar(&failures_file);
    if failure_analysis_input.is_none() && !output.success {
        failure_analysis_input = parse_compiler_failures(&output.stdout, &output.stderr);
    }
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

    if args.baseline_flags.baseline && !no_tests_applicable {
        if let Some(ref counts) = test_counts {
            let _ = baseline::save_baseline(source_path, &args.component_id, counts)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline && !no_tests_applicable
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

    // Surface an ignored malformed failure sidecar as a secondary diagnostic so
    // the degraded classification is visible without masking the primary result.
    if let Some(diagnostic) = sidecar_diagnostic {
        hints.push(diagnostic);
    }

    if status == "failed" && args.passthrough_args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy review test {} -- --filter=TestName",
            args.component_id
        ));
    }

    if status == "failed" && output.success && test_counts.is_none() {
        hints.push(
            "The test runner succeeded without verifiable test results. Configure its extension result parser or emit a test-results sidecar."
                .to_string(),
        );
    } else if status == "failed"
        && output.success
        && test_counts
            .as_ref()
            .is_some_and(|counts| counts.passed + counts.failed == 0)
    {
        hints.push(
            "The test runner reported no executed tests. Fix the selected test filter or declare an extension no_tests_applicable policy with evidence."
                .to_string(),
        );
    }

    if !args.skip_lint {
        hints.push(format!(
            "Auto-fix lint issues: homeboy refactor {} --from lint --write",
            args.component_id
        ));
    }

    if !coverage_enabled {
        hints.push(format!(
            "Collect coverage: homeboy review test {} --coverage",
            args.component_id
        ));
    }

    if test_counts.is_some()
        && !no_tests_applicable
        && !args.baseline_flags.baseline
        && baseline_comparison.is_none()
    {
        hints.push(format!(
            "Save test baseline: homeboy review test {} --baseline",
            args.component_id
        ));
    }

    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: homeboy review test {} --ratchet",
            args.component_id
        ));
    }

    if status == "failed" && !args.analyze {
        hints.push(format!(
            "Analyze failures: homeboy review test {} --analyze",
            args.component_id
        ));
    }

    if args.passthrough_args.is_empty() {
        hints.push(
            "Pass args to test runner: homeboy review test <component> -- [args]".to_string(),
        );
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
                stdout_tail: homeboy_core::redaction::redact_string(&stdout_tail),
                stderr_tail: homeboy_core::redaction::redact_string(&stderr_tail),
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
    let mut extension_phase_timings = output.extension_phase_timings;
    merge_reported_test_artifact_locators(
        &mut extension_phase_timings,
        &output.stdout,
        &output.stderr,
    );

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
        test_durations,
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
        extension_phase_timings,
    })
}

/// Assemble the duration picture for one test child.
///
/// Order of preference: an extension-written `test.durations` sidecar (richer
/// timings than stdout can carry), then the runner's own output. Homeboy's
/// wall-clock measurement of the child is attached either way, because it is
/// the only duration that survives a kill.
///
/// A terminated child never finishes writing its evidence, so its timings are
/// necessarily partial. They are still returned — the run that blows the
/// budget is precisely the one where knowing what consumed it matters — but
/// they are marked `complete: false` and carry an explicit reason, so a
/// partial picture can never be read as a full one. Nothing here can fail the
/// run: an unreadable sidecar or unparseable output yields no durations, not
/// an error.
fn collect_test_durations(
    durations_file: &Path,
    stdout: &str,
    child_elapsed: f64,
    timed_out: bool,
    budget: Duration,
) -> Option<TestDurations> {
    let incomplete_reason = timed_out.then(|| {
        format!(
            "test child terminated at its {}s budget; timings cover only what completed first",
            budget.as_secs()
        )
    });

    if let Ok(Some(mut declared)) = parse_test_durations_file(durations_file) {
        if declared.phase_seconds.is_none() {
            declared.phase_seconds = Some(child_elapsed);
        }
        if declared.budget_seconds.is_none() {
            declared.budget_seconds = Some(budget.as_secs_f64());
        }
        if let Some(reason) = incomplete_reason {
            declared.complete = false;
            declared.incomplete_reason = Some(reason);
        }
        return Some(declared);
    }

    let durations = build_test_durations(
        parse_duration_samples(stdout),
        Some(child_elapsed),
        SlowTestPolicy::for_budget(Some(budget)),
        incomplete_reason,
    );
    (!durations.is_empty()).then_some(durations)
}

fn parse_compiler_failures(stdout: &str, stderr: &str) -> Option<TestAnalysisInput> {
    let diagnostic = Regex::new(r"^error\[(E\d+)\]: (.+)$").expect("compiler regex is valid");
    let location = Regex::new(r"^\s*--> (.+):(\d+):\d+$").expect("location regex is valid");
    let symbol = Regex::new(r"`([^`]+)`").expect("symbol regex is valid");
    let lines = stdout.lines().chain(stderr.lines()).collect::<Vec<_>>();
    let mut failures = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let Some(captures) = diagnostic.captures(line) else {
            continue;
        };
        let Some((source_file, source_line)) = lines[index + 1..].iter().take(6).find_map(|line| {
            let captures = location.captures(line)?;
            Some((captures[1].to_string(), captures[2].parse::<u32>().ok()?))
        }) else {
            continue;
        };
        let code = captures[1].to_string();
        let message = captures[2].to_string();
        let symbol = symbol
            .captures(&message)
            .map(|captures| captures[1].to_string())
            .unwrap_or_else(|| message.clone());
        failures.push(crate::test::TestFailure {
            test_name: format!("{code}: {symbol}"),
            test_file: String::new(),
            error_type: format!("compiler_error:{code}"),
            message,
            source_file,
            source_line,
        });
        if failures.len() == COMPILER_FAILURE_LIMIT {
            break;
        }
    }

    (!failures.is_empty()).then_some(TestAnalysisInput {
        failures,
        total: 0,
        passed: 0,
    })
}

/// Parse the optional failure sidecar, degrading gracefully on malformed data.
///
/// Returns the parsed enrichment input (or `None` when absent/unparseable) and
/// an optional secondary diagnostic describing an ignored malformed sidecar. A
/// malformed sidecar never propagates as an error: the primary execution result
/// is already resolved and must not be replaced by a sidecar parse failure that
/// masks the real underlying failure. (#8489)
fn parse_optional_failure_sidecar(
    failures_file: &Path,
) -> (Option<TestAnalysisInput>, Option<String>) {
    match parse_failures_file(failures_file) {
        Ok(input) => (input, None),
        Err(error) => {
            let diagnostic = format!(
                "Ignored a malformed test-failures sidecar ({}); the primary run result is preserved. Re-run with --analyze after the extension emits a valid sidecar for failure classification.",
                error.message
            );
            (None, Some(diagnostic))
        }
    }
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
        test_durations: None,
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
            stderr_tail: homeboy_core::redaction::redact_string(&message),
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
    let provider_results_file = run_dir.path().join("files/test-results.json");
    let source_file = if results_file.is_file() {
        results_file
    } else if provider_results_file.is_file() {
        provider_results_file
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
    let write_results_helper = write_test_results_helper(run_dir)?;
    env_vars.push((
        crate::runtime_helper::WRITE_TEST_RESULTS_ENV.to_string(),
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

fn write_test_results_helper(run_dir: &RunDir) -> homeboy_core::Result<std::path::PathBuf> {
    let helper = run_dir.path().join("write-test-results.sh");
    local_files::write_file_atomic(
        &helper,
        include_str!("../runtime/write-test-results.sh"),
        "write test results runtime helper",
    )?;
    Ok(helper)
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
            stdout_tail: homeboy_core::redaction::redact_string(&stdout_tail),
            stderr_tail: homeboy_core::redaction::redact_string(&stderr_tail),
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
        test_durations: None,
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

fn merge_reported_test_artifact_locators(
    timings: &mut Vec<ExtensionPhaseTiming>,
    stdout: &str,
    stderr: &str,
) {
    const MAX_LOCATORS: usize = 32;
    let locator = regex::Regex::new(r"artifact://files/[A-Za-z0-9._/-]+")
        .expect("artifact locator regex is valid");
    let mut reported = std::collections::BTreeSet::new();
    for value in [stdout, stderr] {
        for candidate in locator.find_iter(value).map(|matched| matched.as_str()) {
            let relative = candidate.trim_start_matches("artifact://files/");
            let path = std::path::Path::new(relative);
            if !path.as_os_str().is_empty()
                && !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                reported.insert(candidate.to_string());
            }
        }
    }
    if reported.is_empty() {
        return;
    }
    let existing = timings
        .iter()
        .flat_map(|timing| timing.artifacts.iter())
        .filter_map(|artifact| artifact.get("ref").and_then(serde_json::Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    let artifacts = reported
        .into_iter()
        .filter(|candidate| !existing.contains(candidate.as_str()))
        .take(MAX_LOCATORS)
        .map(|reference| serde_json::json!({ "ref": reference }))
        .collect::<Vec<_>>();
    if !artifacts.is_empty() {
        timings.push(ExtensionPhaseTiming {
            name: "provider-reported-test-artifacts".to_string(),
            duration_ms: 0,
            status: Some("reported".to_string()),
            message: None,
            artifacts,
            metadata: Default::default(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::TestFailure;
    use homeboy_core::component::{ComponentScriptsConfig, ScopedExtensionConfig};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CONDITIONAL_SECRET_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn conditional_secret_env_guard() -> std::sync::MutexGuard<'static, ()> {
        CONDITIONAL_SECRET_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("conditional secret env lock")
    }

    fn assert_artifact_tree_excludes(root: &Path, needle: &str) {
        for entry in std::fs::read_dir(root).expect("read artifact directory") {
            let path = entry.expect("artifact entry").path();
            if path.is_dir() {
                assert_artifact_tree_excludes(&path, needle);
            } else if let Ok(contents) = std::fs::read_to_string(&path) {
                assert!(
                    !contents.contains(needle),
                    "artifact {} leaked declared secret",
                    path.display()
                );
            }
        }
    }

    fn conditional_test_component(home: &Path, source: &Path, mode: &str) -> Component {
        let extension_dir = home.join(".config/homeboy/extensions/conditional-secret-fixture");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join("conditional-secret-fixture.json"),
            r#"{
                "name":"Conditional secret fixture",
                "version":"1.0.0",
                "settings":[
                    {"id":"service","label":"Service","type":"object","default":{"mode":"local"}}
                ],
                "test":{
                    "extension_script":"test.sh",
                    "secret_env_projections":[{
                        "when":{"path":["service","mode"],"equals":"remote"},
                        "names_path":["service","secret_env"]
                    }]
                }
            }"#,
        )
        .expect("extension manifest");
        std::fs::write(
            extension_dir.join("test.sh"),
            "#!/bin/sh\nprintf 'first=%s second=%s settings=%s\\n' \"${FIRST_PROJECTED_SECRET-unset}\" \"${SECOND_PROJECTED_SECRET-unset}\" \"$HOMEBOY_SETTINGS_JSON\"\nexit 1\n",
        )
        .expect("extension script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = extension_dir.join("test.sh");
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(script, permissions).expect("executable script");
        }

        Component {
            id: "conditional-secret-consumer".to_string(),
            local_path: source.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "conditional-secret-fixture".to_string(),
                ScopedExtensionConfig {
                    settings: HashMap::from([(
                        "service".to_string(),
                        serde_json::json!({
                            "mode": mode,
                            "secret_env": {
                                "first": "FIRST_PROJECTED_SECRET",
                                "second": "SECOND_PROJECTED_SECRET"
                            }
                        }),
                    )]),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
    }

    fn fixture_workflow_args(component: &Component) -> TestRunWorkflowArgs {
        TestRunWorkflowArgs {
            component_label: component.id.clone(),
            component_id: component.id.clone(),
            path_override: None,
            settings: Vec::new(),
            settings_json: Vec::new(),
            skip_lint: true,
            coverage: false,
            coverage_min: None,
            analyze: false,
            baseline_flags: Default::default(),
            changed_since: None,
            precomputed_changed_files: None,
            json_summary: true,
            restore_checkout: false,
            ci_env: Vec::new(),
            passthrough_args: Vec::new(),
        }
    }

    #[test]
    fn review_test_projects_matching_secrets_and_skips_non_matching_mode() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = conditional_secret_env_guard();
            let source = tempfile::tempdir().expect("source dir");
            std::env::set_var("FIRST_PROJECTED_SECRET", "first-review-secret");
            std::env::set_var("SECOND_PROJECTED_SECRET", "second-review-secret");

            let matching = conditional_test_component(home.path(), source.path(), "remote");
            let matching_run = RunDir::create().expect("matching run dir");
            let matching_result = run_main_test_workflow(
                &matching,
                source.path(),
                fixture_workflow_args(&matching),
                &matching_run,
            )
            .expect("matching child runs");
            let rendered = serde_json::to_string(&matching_result).expect("review result");
            assert!(rendered.contains("[REDACTED]"));
            for value in ["first-review-secret", "second-review-secret"] {
                assert!(!rendered.contains(value));
                assert_artifact_tree_excludes(matching_run.path(), value);
            }
            let supervision = std::fs::read_to_string(
                matching_run
                    .path()
                    .join(homeboy_core::engine::run_dir::files::CHILD_SUPERVISION),
            )
            .expect("child supervision evidence");
            assert!(supervision.contains("[REDACTED]"));

            std::env::remove_var("FIRST_PROJECTED_SECRET");
            std::env::remove_var("SECOND_PROJECTED_SECRET");
            let local = conditional_test_component(home.path(), source.path(), "local");
            let local_run = RunDir::create().expect("local run dir");
            let local_result = run_main_test_workflow(
                &local,
                source.path(),
                fixture_workflow_args(&local),
                &local_run,
            )
            .expect("non-matching child needs no secrets");
            assert_eq!(local_result.exit_code, 1, "non-matching child executed");
            let local_stdout = std::fs::read_to_string(
                local_run
                    .path()
                    .join("validation-progress/command-1-stdout.log"),
            )
            .expect("non-matching stdout artifact");
            assert!(local_stdout.contains("first=unset second=unset"));
        });
    }

    #[test]
    fn review_test_missing_projected_secret_fails_before_spawn() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = conditional_secret_env_guard();
            let source = tempfile::tempdir().expect("source dir");
            let marker = source.path().join("child-ran");
            let component = conditional_test_component(home.path(), source.path(), "remote");
            std::fs::write(
                home.path()
                    .join(".config/homeboy/extensions/conditional-secret-fixture/test.sh"),
                format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
            )
            .expect("marker script");
            std::env::remove_var("FIRST_PROJECTED_SECRET");
            std::env::remove_var("SECOND_PROJECTED_SECRET");

            let error = run_main_test_workflow(
                &component,
                source.path(),
                fixture_workflow_args(&component),
                &RunDir::create().expect("run dir"),
            )
            .expect_err("missing projected identity must fail before spawn");
            assert!(error.message.contains("FIRST_PROJECTED_SECRET"));
            assert!(!marker.exists());
        });
    }

    #[test]
    fn review_test_injects_declared_secret_and_redacts_child_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/secret-test-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join("secret-test-fixture.json"),
                r#"{
                    "name":"Secret test fixture",
                    "version":"1.0.0",
                    "test":{
                        "extension_script":"test.sh",
                        "secret_env":{"DECLARED_TEST_SECRET":"DECLARED_TEST_SECRET"}
                    }
                }"#,
            )
            .expect("extension manifest");
            std::fs::write(
                extension_dir.join("test.sh"),
                "#!/bin/sh\nprintf 'received=%s\\n' \"$DECLARED_TEST_SECRET\"\nexit 1\n",
            )
            .expect("extension script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let script = extension_dir.join("test.sh");
                let mut permissions = std::fs::metadata(&script)
                    .expect("script metadata")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(script, permissions).expect("executable script");
            }

            let component = Component {
                id: "secret-consumer".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "secret-test-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let run_dir = RunDir::create().expect("run dir");
            std::env::set_var("DECLARED_TEST_SECRET", "review-fixture-secret");
            let result = run_main_test_workflow(
                &component,
                source.path(),
                TestRunWorkflowArgs {
                    component_label: component.id.clone(),
                    component_id: component.id.clone(),
                    path_override: None,
                    settings: Vec::new(),
                    settings_json: Vec::new(),
                    skip_lint: true,
                    coverage: false,
                    coverage_min: None,
                    analyze: false,
                    baseline_flags: Default::default(),
                    changed_since: None,
                    precomputed_changed_files: None,
                    json_summary: true,
                    restore_checkout: false,
                    ci_env: Vec::new(),
                    passthrough_args: Vec::new(),
                },
                &run_dir,
            )
            .expect("review workflow reaches secret-bearing child");
            std::env::remove_var("DECLARED_TEST_SECRET");

            assert_eq!(result.exit_code, 1, "child ran after secret injection");
            let rendered = serde_json::to_string(&result).expect("review result JSON");
            assert!(rendered.contains("[REDACTED]"));
            assert!(!rendered.contains("review-fixture-secret"));
            let artifact = std::fs::read_to_string(
                run_dir
                    .path()
                    .join("validation-progress/command-1-stdout.log"),
            )
            .expect("review stdout artifact");
            assert!(artifact.contains("[REDACTED]"));
            assert!(!artifact.contains("review-fixture-secret"));
            assert_artifact_tree_excludes(run_dir.path(), "review-fixture-secret");
        });
    }

    /// Two lines of a real cargo test run: one binary that finished and
    /// reported its time, and one that started and never did.
    const PARTIAL_RUNNER_OUTPUT: &str = concat!(
        "     Running tests/fast.rs (/t/deps/fast-0123456789abcdef)\n",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.00s\n",
        "     Running tests/slow.rs (/t/deps/slow-fedcba9876543210)\n",
        "running 1 test\n",
        "test the_slow_one has been running for over 60 seconds\n",
    );

    #[test]
    fn a_killed_test_child_still_yields_labelled_partial_timings() {
        // `execute_capability_script` returns partial stdout on timeout (see
        // its own test), so the durations path receives exactly this shape.
        let dir = tempfile::tempdir().expect("tempdir");
        let durations = collect_test_durations(
            &dir.path().join("absent.json"),
            PARTIAL_RUNNER_OUTPUT,
            1500.0,
            true,
            Duration::from_secs(1500),
        )
        .expect("partial output still yields durations");

        assert!(!durations.complete, "a killed run is never a full picture");
        assert!(durations
            .incomplete_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("1500s budget")));
        assert_eq!(
            durations.measured_seconds,
            Some(4.0),
            "only what actually reported is counted"
        );
        assert!(durations
            .slow
            .iter()
            .any(|finding| finding.rule == "unfinished-test-unit"
                && finding.name.contains("the_slow_one")
                && finding.seconds.is_none()));
    }

    #[test]
    fn unparseable_output_still_reports_the_wall_clock_and_no_fabricated_totals() {
        // Homeboy's own measurement of the child is real evidence even when the
        // runner printed nothing timeable, so it is reported. What must never
        // be invented is a *measured* total: no binary reported, so the sum is
        // unknown, not zero.
        let dir = tempfile::tempdir().expect("tempdir");
        let durations = collect_test_durations(
            &dir.path().join("absent.json"),
            "error: could not compile `homeboy`\n",
            12.0,
            false,
            Duration::from_secs(1500),
        )
        .expect("the wall clock is always available");

        assert_eq!(durations.phase_seconds, Some(12.0));
        assert_eq!(
            durations.measured_seconds, None,
            "nothing reported means unknown, never zero"
        );
        assert!(durations.binaries.is_empty());
        assert!(durations.tests.is_empty());
        assert!(durations.slow.is_empty());
        assert!(durations.complete);
    }

    #[test]
    fn a_declared_durations_sidecar_wins_over_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-durations.json");
        std::fs::write(
            &path,
            r#"{"measured_seconds":99.0,"binaries":[{"name":"declared","seconds":99.0,"source":"binary-summary"}]}"#,
        )
        .expect("write sidecar");

        let durations = collect_test_durations(
            &path,
            PARTIAL_RUNNER_OUTPUT,
            120.0,
            false,
            Duration::from_secs(1500),
        )
        .expect("sidecar is consumed");

        assert_eq!(durations.measured_seconds, Some(99.0));
        assert_eq!(durations.binaries.len(), 1);
        // Homeboy's own measurements still fill the gaps the sidecar left.
        assert_eq!(durations.phase_seconds, Some(120.0));
        assert_eq!(durations.budget_seconds, Some(1500.0));
    }

    #[test]
    fn reported_artifact_locators_are_normalized_and_deduplicated() {
        let mut timings = Vec::new();
        merge_reported_test_artifact_locators(
            &mut timings,
            "artifact://files/test-results.json artifact://files/../escape.log",
            "artifact://files/phpunit-output.log artifact://files/test-results.json",
        );

        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].artifacts.len(), 2);
        assert_eq!(
            timings[0].artifacts[0]["ref"],
            "artifact://files/phpunit-output.log"
        );
        assert_eq!(
            timings[0].artifacts[1]["ref"],
            "artifact://files/test-results.json"
        );
    }
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
                test_durations: None,
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
    fn malformed_failure_sidecar_is_ignored_with_a_diagnostic_not_an_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sidecar = temp.path().join("test-failures.json");
        // A malformed failure object — here `source_line` is a string where a
        // number is required — the kind of schema-invalid sidecar a failed
        // recipe can emit. It must not abort the run and mask the real failure. (#8489)
        std::fs::write(
            &sidecar,
            r#"[{"test_id":"bootstrap","message":"input.materialize timeout","error_type":"timeout","source_line":"not-a-number"}]"#,
        )
        .expect("write malformed sidecar");

        let (input, diagnostic) = parse_optional_failure_sidecar(&sidecar);

        assert!(
            input.is_none(),
            "a malformed sidecar must not yield enrichment input"
        );
        let diagnostic = diagnostic.expect("a malformed sidecar must attach a diagnostic");
        assert!(
            diagnostic.contains("malformed test-failures sidecar"),
            "diagnostic: {diagnostic}"
        );
        assert!(
            diagnostic.contains("primary run result is preserved"),
            "diagnostic: {diagnostic}"
        );
    }

    #[test]
    fn valid_failure_sidecar_parses_without_a_diagnostic() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sidecar = temp.path().join("test-failures.json");
        std::fs::write(
            &sidecar,
            r#"[{"test_id":"SomeTest::method","message":"assertion failed","error_type":"assertion"}]"#,
        )
        .expect("write valid sidecar");

        let (input, diagnostic) = parse_optional_failure_sidecar(&sidecar);

        let input = input.expect("a valid sidecar must yield enrichment input");
        assert_eq!(input.failures.len(), 1);
        assert_eq!(input.failures[0].error_type, "assertion");
        assert!(
            diagnostic.is_none(),
            "a valid sidecar must not attach a diagnostic"
        );
    }

    #[test]
    fn compiler_diagnostics_become_release_visible_findings_without_a_sidecar() {
        let output = r#"error[E0425]: cannot find function `rollback_refresh_error` in this scope
   --> crates/homeboy-lab-runner/src/homeboy_refresh/tests/part_a.rs:608:21
    |
608 |         let error = rollback_refresh_error::<()>(
    |                     ^^^^^^^^^^^^^^^^^^^^^^ not found in this scope
"#;
        let input = parse_compiler_failures(output, "").expect("compiler finding");
        let findings = homeboy_findings_from_test_analysis_input(&input).expect("findings");
        let (report, exit_code) = super::super::report::from_main_workflow(TestRunWorkflowResult {
            status: "failed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 101,
            test_counts: None,
            test_durations: None,
            findings: Some(findings),
            failure_analysis_input: Some(input),
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
            extension_phase_timings: Vec::new(),
        });
        let json = serde_json::to_value(report).expect("report json");

        assert_eq!(exit_code, 101);
        assert_eq!(json["failure"]["category"], "findings");
        assert_eq!(json["findings"][0]["rule"], "compiler_error:E0425");
        assert!(json["findings"][0]["message"]
            .as_str()
            .expect("finding message")
            .contains("rollback_refresh_error"));
        assert_eq!(
            json["findings"][0]["file"],
            "crates/homeboy-lab-runner/src/homeboy_refresh/tests/part_a.rs"
        );
        assert_eq!(json["findings"][0]["line"], 608);
    }

    #[test]
    fn absent_failure_sidecar_yields_no_input_and_no_diagnostic() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sidecar = temp.path().join("does-not-exist.json");

        let (input, diagnostic) = parse_optional_failure_sidecar(&sidecar);

        assert!(input.is_none());
        assert!(diagnostic.is_none());
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
        assert_eq!(test_run_status(false, Some(&counts), false), "failed");
    }

    #[test]
    fn status_passes_successful_runner_with_zero_failures() {
        let counts = TestCounts::new(3, 3, 0, 0);
        assert_eq!(test_run_status(true, Some(&counts), false), "passed");
    }

    #[test]
    fn status_fails_successful_runner_with_parsed_failures() {
        let counts = TestCounts::new(3, 2, 1, 0);
        assert_eq!(test_run_status(true, Some(&counts), false), "failed");
    }

    #[test]
    fn status_fails_successful_runner_without_result_evidence() {
        assert_eq!(test_run_status(true, None, false), "failed");
    }

    #[test]
    fn status_fails_all_skipped_tests() {
        assert_eq!(
            test_run_status(true, Some(&TestCounts::new(1, 0, 0, 1)), false),
            "failed"
        );
    }

    /// Recorded no-measurement scenarios, fed through the real status function.
    ///
    /// Assert the effect, not the command string (#10685). The post-merge audit
    /// gate passed for weeks because its test asserted the command it invoked
    /// rather than what that command produced, so every case here is a shape
    /// actually observed in a CI run and every assertion is about the verdict
    /// that came out.
    #[test]
    fn no_recorded_unmeasured_shape_renders_green() {
        let unmeasured: &[(&str, Option<TestCounts>)] = &[
            (
                "child killed before writing its results sidecar (#10639/#10644): counts absent \
                 entirely",
                None,
            ),
            (
                "runner exited 0 having executed nothing: `0 passed; 0 failed`",
                Some(TestCounts::new(0, 0, 0, 0)),
            ),
            (
                "every selected test skipped: the runner started and measured no assertion",
                Some(TestCounts::new(12, 0, 0, 12)),
            ),
            (
                "a total was reported but no assertion resolved -- the shape a truncated \
                 summary parse produces",
                Some(TestCounts::new(412, 0, 0, 0)),
            ),
        ];

        for (scenario, counts) in unmeasured {
            assert_eq!(
                test_run_status(true, counts.as_ref(), false),
                "failed",
                "an unmeasured test phase rendered green: {scenario}"
            );
        }
    }

    /// The migration onto the shared predicate is behaviour-preserving.
    ///
    /// Stated as a property over the whole reachable input space rather than as
    /// a handful of examples, because "identical behaviour" is the entire claim
    /// the refactor rests on and examples cannot support it.
    #[test]
    fn test_run_status_matches_the_shared_predicate() {
        for runner_success in [true, false] {
            for no_tests in [true, false] {
                for counts in [
                    None,
                    Some(TestCounts::new(0, 0, 0, 0)),
                    Some(TestCounts::new(1, 0, 0, 1)),
                    Some(TestCounts::new(3, 3, 0, 0)),
                    Some(TestCounts::new(3, 2, 1, 0)),
                    Some(TestCounts::new(3, 0, 3, 0)),
                    Some(TestCounts::new(9, 4, 0, 5)),
                ] {
                    // The rule as it stood before #10685, verbatim.
                    let legacy = if !runner_success {
                        "failed"
                    } else if no_tests {
                        "skipped"
                    } else if counts.as_ref().is_some_and(|counts| {
                        counts.passed + counts.failed > 0 && counts.failed == 0
                    }) {
                        "passed"
                    } else {
                        "failed"
                    };
                    assert_eq!(
                        test_run_status(runner_success, counts.as_ref(), no_tests),
                        legacy,
                        "shared-predicate migration changed behaviour for \
                         runner_success={runner_success} no_tests={no_tests} counts={counts:?}"
                    );
                }
            }
        }
    }

    /// `skipped` is the one exit from the measurement requirement, and it is
    /// reached only through positive, nonce-bound evidence -- never through the
    /// absence of counts.
    #[test]
    fn only_signed_evidence_can_skip_the_measurement_requirement() {
        assert_eq!(test_run_status(true, None, true), "skipped");
        assert_eq!(
            test_run_status(true, None, false),
            "failed",
            "absent counts must never be read as `nothing to test`"
        );
    }

    #[test]
    fn no_test_policy_requires_bound_structured_evidence() {
        let temp = tempfile::tempdir().expect("temp dir");
        let evidence_file = temp.path().join("no-tests-applicable.json");
        std::fs::write(&evidence_file, r#"{"schema":"homeboy/no-tests-applicable/v1","extension_id":"fixture","step":"test","nonce":"nonce","reason":"docs only"}"#).expect("write evidence");
        assert!(no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
        std::fs::write(&evidence_file, r#"{"schema":"homeboy/no-tests-applicable/v1","extension_id":"fixture","step":"test","nonce":"wrong","reason":"docs only"}"#).expect("write wrong nonce");
        assert!(!no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
        std::fs::write(&evidence_file, r#"{"schema":"homeboy/no-tests-applicable/v1","extension_id":"fixture","step":"lint","nonce":"nonce","reason":"docs only"}"#).expect("write wrong step");
        assert!(!no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
        std::fs::write(&evidence_file, "not json").expect("write malformed evidence");
        assert!(!no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
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
