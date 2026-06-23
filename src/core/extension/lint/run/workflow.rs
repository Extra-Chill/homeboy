//! Lint workflow orchestration — drives lint runs (scoped + full), processes
//! baseline lifecycle, assembles hints, and constructs results.

use super::exit_code::{
    effective_lint_exit_code, normalize_empty_finding_exit_code, normalize_finding_exit_code,
};
use super::findings::{
    build_lint_producer_summaries, build_lint_summary, filter_findings_to_scoped_files,
    filter_lint_findings, mark_zero_finding_producers_passed, parse_lint_producer_summaries_file,
};
use super::formatting::{extract_formatting_findings, self_check_output_is_harness_failure};
use super::hints::build_autofix_hint;
use super::scoping::resolve_scoped_lint_runs;
use super::types::{LintRunWorkflowArgs, LintRunWorkflowResult, ScopedLintRun};
use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::extension::lint::baseline as lint_baseline;
use crate::core::extension::lint::build_lint_runner;
use crate::core::extension::{self, ExtensionCapability};
use crate::core::finding::HomeboyFinding;
use crate::core::validation_progress::{write_command_artifact, ValidationProgressRecorder};
use std::path::{Path, PathBuf};

/// Run the main lint workflow.
///
/// Handles changed-file scoping, autofix planning, lint runner execution,
/// baseline lifecycle, hint assembly, and result construction.
pub fn run_main_lint_workflow(
    component: &Component,
    source_path: &Path,
    args: LintRunWorkflowArgs,
    run_dir: &RunDir,
) -> crate::core::Result<LintRunWorkflowResult> {
    let scoped_runs = resolve_scoped_lint_runs(component, &args)?;

    // Early exit if changed-file mode produced no files
    if let Some(ref runs) = scoped_runs {
        if runs.is_empty() {
            return Ok(LintRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                harness_error: false,
                autofix: None,
                hints: None,
                baseline_comparison: None,
                formatting_findings: None,
                findings: None,
                producer_summaries: Vec::new(),
                summary: if args.json_summary {
                    Some(build_lint_summary(&[], &[], 0))
                } else {
                    None
                },
                self_check_capture: None,
                extension_phase_timings: Vec::new(),
            });
        }
    }

    // Run lint
    let output = if let Some(ref runs) = scoped_runs {
        run_scoped_lint_runs(component, &args, run_dir, runs)?
    } else {
        let mut progress = ValidationProgressRecorder::new(
            run_dir,
            None,
            vec![("lint runner".to_string(), args.component_label.clone())],
        )?;
        let runner = build_lint_runner(
            component,
            args.path_override.clone(),
            &args.settings,
            args.summary || args.json_summary,
            args.file.as_deref(),
            args.glob.as_deref(),
            args.sniff_filters.errors_only,
            args.sniff_filters.sniffs.as_deref(),
            args.sniff_filters.exclude_sniffs.as_deref(),
            args.category.as_deref(),
            None,
            run_dir,
        )?;
        let runner = args
            .ci_env
            .iter()
            .fold(runner, |runner, (key, value)| runner.env(key, value));
        progress.start(0)?;
        let output = runner
            .env_if(
                args.changed_since.is_some(),
                "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES",
                "1",
            )
            .passthrough(!args.json_summary)
            .run()?;
        let stdout_artifact = write_command_artifact(run_dir, 0, "stdout", &output.stdout)?;
        let stderr_artifact = write_command_artifact(run_dir, 0, "stderr", &output.stderr)?;
        progress.finish(0, output.exit_code, stdout_artifact, stderr_artifact)?;
        output
    };

    let lint_findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
    let lint_producers_file = run_dir.step_file(run_dir::files::LINT_PRODUCERS);
    let parsed_lint_findings = lint_baseline::parse_findings_file(&lint_findings_file)?;
    let scoped_filter_removed_findings = scoped_runs.is_some() && !parsed_lint_findings.is_empty();
    let raw_lint_findings =
        filter_findings_to_scoped_files(parsed_lint_findings, scoped_runs.as_deref());
    let lint_findings = filter_lint_findings(raw_lint_findings, &args);
    let formatting_findings =
        extract_formatting_findings(&output.stdout, &output.stderr, source_path);
    let declared_producers = parse_lint_producer_summaries_file(&lint_producers_file)?;
    let mut producer_summaries = build_lint_producer_summaries(
        &lint_findings,
        &lint_findings_file,
        &lint_producers_file,
        declared_producers,
        output.success,
        output.exit_code,
        None,
    );
    if scoped_filter_removed_findings && lint_findings.is_empty() && output.exit_code == 1 {
        mark_zero_finding_producers_passed(&mut producer_summaries);
    }

    let mut hints = Vec::new();

    let runner_exit_code = normalize_empty_finding_exit_code(
        output.exit_code,
        output.success,
        &lint_findings,
        &producer_summaries,
    );
    let lint_exit_code = normalize_finding_exit_code(runner_exit_code, &lint_findings);

    // Baseline lifecycle
    let (baseline_comparison, baseline_exit_override) =
        process_baseline(source_path, &args, &lint_findings)?;

    let exit_code = effective_lint_exit_code(lint_exit_code, baseline_exit_override);
    let status = if exit_code == 0 { "passed" } else { "failed" }.to_string();
    let lint_clean = lint_findings.is_empty() && exit_code == 0;

    // Hint assembly — point to the auto-fix CTA for autofixable findings.
    //
    // Per the contract under #1459 (issue #1507), autofixable findings never
    // fail the run; they nudge. The CTA is rendered here in core, not by each
    // extension's runner, so every language extension benefits from a single
    // consistent prose. `homeboy lint --fix` is the ergonomic alias and is
    // listed first; the canonical `homeboy refactor --from lint --write`
    // invocation follows for users who want the longer form.
    if !lint_clean {
        hints.push(build_autofix_hint(&args));
        if args.changed_only {
            hints.push(
                "--changed-only is file-scoped: findings may be outside the changed hunks in modified files."
                    .to_string(),
            );
        }
        hints.push("Some issues may require manual fixes".to_string());
    }

    if args.file.is_none()
        && args.glob.is_none()
        && !args.changed_only
        && args.changed_since.is_none()
    {
        hints.push(
            "For targeted linting: --file <path>, --glob <pattern>, --changed-only, or --changed-since <ref>".to_string(),
        );
    }

    hints.push("Full options: homeboy docs commands/lint".to_string());

    if !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save lint baseline: homeboy lint {} --baseline",
            args.component_label
        ));
    }

    let hints = if hints.is_empty() { None } else { Some(hints) };

    // A non-zero exit with zero findings whose runner output shows infra
    // markers is a harness failure, not a real lint failure.
    let harness_error = exit_code != 0
        && lint_findings.is_empty()
        && self_check_output_is_harness_failure(output.exit_code, &output.stdout, &output.stderr);

    Ok(LintRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        harness_error,
        autofix: None,
        hints,
        baseline_comparison,
        formatting_findings,
        summary: if args.json_summary {
            Some(build_lint_summary(
                &lint_findings,
                &producer_summaries,
                exit_code,
            ))
        } else {
            None
        },
        findings: Some(lint_findings),
        producer_summaries,
        self_check_capture: None,
        extension_phase_timings: output.extension_phase_timings,
    })
}

fn run_scoped_lint_runs(
    component: &Component,
    args: &LintRunWorkflowArgs,
    run_dir: &RunDir,
    runs: &[ScopedLintRun],
) -> crate::core::Result<extension::RunnerOutput> {
    let mut success = true;
    let mut exit_code = 0;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut extension_phase_timings = Vec::new();
    let mut progress = ValidationProgressRecorder::new(
        run_dir,
        None,
        runs.iter()
            .enumerate()
            .map(|(index, run)| {
                (
                    run.step
                        .clone()
                        .unwrap_or_else(|| format!("lint scoped command {}", index + 1)),
                    run.glob.clone(),
                )
            })
            .collect(),
    )?;

    for (index, run) in runs.iter().enumerate() {
        let scoped_run_dir;
        let active_run_dir = if index == 0 {
            run_dir
        } else {
            scoped_run_dir = RunDir::create()?;
            &scoped_run_dir
        };

        let runner = build_lint_runner(
            component,
            args.path_override.clone(),
            &args.settings,
            args.summary || args.json_summary,
            args.file.as_deref(),
            Some(run.glob.as_str()),
            args.sniff_filters.errors_only,
            args.sniff_filters.sniffs.as_deref(),
            args.sniff_filters.exclude_sniffs.as_deref(),
            args.category.as_deref(),
            run.step.as_deref(),
            active_run_dir,
        )?;
        let runner = args
            .ci_env
            .iter()
            .fold(runner, |runner, (key, value)| runner.env(key, value));
        progress.start(index)?;
        let output = runner
            .env_if(
                args.changed_since.is_some(),
                "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES",
                "1",
            )
            .passthrough(!args.json_summary)
            .run()?;
        let stdout_artifact = write_command_artifact(run_dir, index, "stdout", &output.stdout)?;
        let stderr_artifact = write_command_artifact(run_dir, index, "stderr", &output.stderr)?;
        progress.finish(index, output.exit_code, stdout_artifact, stderr_artifact)?;
        extension_phase_timings.extend(output.extension_phase_timings);
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            stdout.push('\n');
        }
        stdout.push_str(&output.stdout);
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&output.stderr);

        if !output.success {
            success = false;
            if exit_code == 0 {
                exit_code = output.exit_code;
            }
        }
    }

    Ok(extension::RunnerOutput {
        exit_code,
        success,
        stdout,
        stderr,
        child_resource: None,
        extension_phase_timings,
    })
}

pub fn run_self_check_lint_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> crate::core::Result<LintRunWorkflowResult> {
    run_self_check_lint_workflow_with_progress(
        component,
        source_path,
        component_label,
        json_summary,
        None,
        None,
    )
}

pub fn run_self_check_lint_workflow_with_progress(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
    run_dir: Option<&RunDir>,
    observation: Option<&crate::core::observation::ActiveObservation>,
) -> crate::core::Result<LintRunWorkflowResult> {
    let output = extension::self_check::run_self_checks_with_passthrough_and_progress(
        component,
        ExtensionCapability::Lint,
        source_path,
        !json_summary,
        run_dir,
        observation,
    )?;
    let status = if output.success { "passed" } else { "failed" }.to_string();
    // A self-check that exits non-zero while the underlying linter reported
    // nothing is a harness/wrapper failure, not a real lint failure (e.g. the
    // missing `runner-steps.sh` environmental issue). Flag it so release
    // preflight can warn instead of hard-blocking.
    let harness_error = !output.success
        && self_check_output_is_harness_failure(output.exit_code, &output.stdout, &output.stderr);
    let hints = (!output.success).then(|| {
        if harness_error {
            vec![format!(
                "Lint self-check harness for {} exited {} with no findings — the wrapper failed, not the linter. \
Re-run `homeboy lint {}` or skip only this gate with `--skip-checks=lint`.",
                component.id, output.exit_code, component.id
            )]
        } else {
            vec![format!(
                "Fix the failing self-check command declared in {}'s homeboy.json scripts.lint",
                component.id
            )]
        }
    });

    let producer_summaries = build_lint_producer_summaries(
        &[],
        &PathBuf::from(run_dir::files::LINT_FINDINGS),
        &PathBuf::from(run_dir::files::LINT_PRODUCERS),
        Vec::new(),
        output.success,
        output.exit_code,
        Some("self-check"),
    );
    let formatting_findings =
        extract_formatting_findings(&output.stdout, &output.stderr, source_path);

    Ok(LintRunWorkflowResult {
        status,
        component: component_label,
        exit_code: output.exit_code,
        harness_error,
        autofix: None,
        hints,
        baseline_comparison: None,
        formatting_findings,
        findings: Some(Vec::new()),
        producer_summaries: producer_summaries.clone(),
        summary: if json_summary {
            Some(build_lint_summary(
                &[],
                &producer_summaries,
                output.exit_code,
            ))
        } else {
            None
        },
        self_check_capture: Some(output.capture),
        extension_phase_timings: Vec::new(),
    })
}

/// Process baseline lifecycle — save, load, compare.
fn process_baseline(
    source_path: &Path,
    args: &LintRunWorkflowArgs,
    lint_findings: &[HomeboyFinding],
) -> crate::core::Result<(Option<lint_baseline::BaselineComparison>, Option<i32>)> {
    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if args.baseline_flags.baseline {
        let saved = lint_baseline::save_baseline(source_path, &args.component_id, lint_findings)?;
        eprintln!(
            "[lint] Baseline saved to {} ({} findings)",
            saved.display(),
            lint_findings.len()
        );
    }

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(existing) = lint_baseline::load_baseline(source_path) {
            let comparison = lint_baseline::compare(lint_findings, &existing);

            if comparison.drift_increased {
                eprintln!(
                    "[lint] DRIFT INCREASED: {} new finding(s) since baseline",
                    comparison.new_items.len()
                );
                baseline_exit_override = Some(1);
            } else if !comparison.resolved_fingerprints.is_empty() {
                eprintln!(
                    "[lint] Drift reduced: {} finding(s) resolved since baseline",
                    comparison.resolved_fingerprints.len()
                );
                baseline_exit_override = Some(0);
            } else {
                eprintln!("[lint] No change from baseline");
                baseline_exit_override = Some(0);
            }

            baseline_comparison = Some(comparison);
        }
    }

    Ok((baseline_comparison, baseline_exit_override))
}
