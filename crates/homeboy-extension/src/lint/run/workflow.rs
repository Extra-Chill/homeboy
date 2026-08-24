//! Lint workflow orchestration — drives lint runs (scoped + full), processes
//! baseline lifecycle, assembles hints, and constructs results.

use super::exit_code::{
    effective_lint_exit_code, normalize_empty_finding_exit_code, normalize_producer_exit_code,
};
use super::findings::{
    build_lint_producer_summaries, build_lint_summary, filter_findings_to_scoped_files,
    filter_lint_findings, mark_zero_finding_producers_passed, parse_lint_producer_summaries_file,
};
use super::formatting::{extract_formatting_findings, self_check_output_is_harness_failure};
use super::hints::build_autofix_hint;
use super::scoping::resolve_scoped_lint_runs;
use super::types::{LintRunWorkflowArgs, LintRunWorkflowResult, ScopedLintPlan, ScopedLintRun};
use crate as extension;
use crate::lint::baseline as lint_baseline;
use crate::lint::build_lint_runner;
use crate::ExtensionCapability;
use homeboy_core::component::Component;
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::finding::HomeboyFinding;
use homeboy_core::validation_progress::{write_command_artifact, ValidationProgressRecorder};
use std::path::{Path, PathBuf};
use std::process::Command;

struct LintRunEvidence {
    findings: Vec<HomeboyFinding>,
    declared_producers: Vec<homeboy_core::finding::FindingProducerSummary>,
    findings_file: PathBuf,
    producers_file: PathBuf,
    changed_files: Option<Vec<String>>,
    step: Option<String>,
    success: bool,
    exit_code: i32,
}

struct ScopedLintOutput {
    output: extension::RunnerOutput,
    evidence: Vec<LintRunEvidence>,
    child_run_dirs: Vec<RunDir>,
}

/// Run the main lint workflow.
///
/// Handles changed-file scoping, autofix planning, lint runner execution,
/// baseline lifecycle, hint assembly, and result construction.
pub fn run_main_lint_workflow(
    component: &Component,
    source_path: &Path,
    args: LintRunWorkflowArgs,
    run_dir: &RunDir,
) -> homeboy_core::Result<LintRunWorkflowResult> {
    let scoped_plan = resolve_scoped_lint_runs(component, &args)?;

    // Early exit if changed-file mode produced no runnable scope.
    //
    // This exit renders `passed` having linted nothing, and until #10685 it did
    // so without recording how many files it declined to lint. Two states
    // reached it and rendered identically:
    //
    //   * `changed_files_considered == 0` — the diff was empty. An honest
    //     green; `measurement_ok` classifies it `EmptyPopulation`.
    //   * `changed_files_considered > 0` with zero runs — files changed and no
    //     declared lint route claimed any of them. Usually still honest (a
    //     documentation-only diff), occasionally a route glob that stopped
    //     matching.
    //
    // The verdict is deliberately NOT moved for the second case. Every
    // documentation-only pull request lands there, so failing closed would make
    // the lint gate red on a large and entirely legitimate class of change —
    // and a gate that is red for everyone is ignored, which is the same end
    // state as the false green. The predicate also genuinely cannot adjudicate
    // here: the route matcher is both the instrument and the only source of the
    // population, so a broken matcher is indistinguishable from an empty
    // population from the inside. See `ScopedLintPlan`.
    //
    // What does change is that the state stops being invisible. The count is
    // logged and carried into the hints, so "0 files linted because nothing
    // changed" and "0 files linted because 47 changed files matched no route"
    // read differently to an operator and to the PR comment.
    if let Some(ref plan) = scoped_plan {
        if plan.runs.is_empty() {
            let hints = (plan.changed_files_considered > 0).then(|| {
                vec![format!(
                    "Lint ran no scopes: {} changed file(s) were considered and none matched any \
                     extension-declared lint route. If that is unexpected, the route globs no \
                     longer cover these files (#10685).",
                    plan.changed_files_considered
                )]
            });
            if let Some(ref hints) = hints {
                eprintln!("{}", hints[0]);
            }
            return Ok(LintRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                harness_error: false,
                infrastructure_failure: false,
                autofix: None,
                hints,
                baseline_comparison: None,
                baseline_provenance: None,
                formatting_findings: None,
                findings: None,
                producer_summaries: Vec::new(),
                summary: if args.json_summary {
                    Some(build_lint_summary(&[], &[], 0))
                } else {
                    None
                },
                self_check_capture: None,
                cargo_target: None,
                extension_phase_timings: Vec::new(),
            });
        }
    }

    // Run lint
    let (output, evidence, child_run_dirs) = if let Some(ref plan) = scoped_plan {
        let runs = &plan.runs;
        let scoped = run_scoped_lint_runs(component, &args, run_dir, runs)?;
        (scoped.output, scoped.evidence, scoped.child_run_dirs)
    } else {
        let mut progress = ValidationProgressRecorder::new(
            run_dir,
            None,
            vec![("lint runner".to_string(), args.component_label.clone())],
        )?;
        let runner = build_lint_runner(crate::lint::LintRunnerRequest {
            component,
            path_override: args.path_override.clone(),
            settings: &args.settings,
            summary: args.summary || args.json_summary,
            file: args.file.as_deref(),
            glob: args.glob.as_deref(),
            errors_only: args.sniff_filters.errors_only,
            sniffs: args.sniff_filters.sniffs.as_deref(),
            exclude_sniffs: args.sniff_filters.exclude_sniffs.as_deref(),
            category: args.category.as_deref(),
            step: None,
            changed_files: None,
            run_dir,
        })?;
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
            .run()?;
        let stdout_artifact = write_command_artifact(run_dir, 0, "stdout", &output.stdout)?;
        let stderr_artifact = write_command_artifact(run_dir, 0, "stderr", &output.stderr)?;
        progress.finish(0, output.exit_code, stdout_artifact, stderr_artifact)?;
        let lint_findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
        if crate::lint::declares_lint_findings_sidecar(component) && !lint_findings_file.is_file() {
            return Err(missing_findings_evidence_error(&lint_findings_file));
        }
        let lint_producers_file = run_dir.step_file(run_dir::files::LINT_PRODUCERS);
        let evidence = vec![LintRunEvidence {
            findings: lint_baseline::parse_findings_file(&lint_findings_file)?,
            declared_producers: parse_lint_producer_summaries_file(&lint_producers_file)?,
            findings_file: lint_findings_file,
            producers_file: lint_producers_file,
            changed_files: None,
            step: None,
            success: output.success,
            exit_code: output.exit_code,
        }];
        (output, evidence, Vec::new())
    };

    let mut lint_findings = Vec::new();
    let mut producer_summaries = Vec::new();
    for evidence in evidence {
        let had_findings = !evidence.findings.is_empty();
        let declared_producers_empty = evidence.declared_producers.is_empty();
        let scoped_run = evidence
            .changed_files
            .as_ref()
            .map(|changed_files| ScopedLintRun {
                glob: String::new(),
                step: evidence.step.clone(),
                changed_files: changed_files.clone(),
            });
        let route_findings = filter_findings_to_scoped_files(
            evidence.findings,
            scoped_run.as_ref().map(std::slice::from_ref),
        );
        let route_findings = filter_lint_findings(route_findings, &args);
        let mut route_producers = build_lint_producer_summaries(
            &route_findings,
            &evidence.findings_file,
            &evidence.producers_file,
            evidence.declared_producers,
            evidence.success,
            evidence.exit_code,
            evidence.step.as_deref(),
        );
        if evidence.changed_files.is_some()
            && had_findings
            && route_findings.is_empty()
            && evidence.exit_code == 1
            && declared_producers_empty
        {
            mark_zero_finding_producers_passed(&mut route_producers);
        }
        lint_findings.extend(route_findings);
        producer_summaries.extend(route_producers);
    }
    let formatting_findings =
        extract_formatting_findings(&output.stdout, &output.stderr, source_path);

    let mut hints = Vec::new();

    let runner_exit_code = normalize_empty_finding_exit_code(
        output.exit_code,
        output.success,
        &lint_findings,
        &producer_summaries,
    );
    let lint_exit_code = normalize_producer_exit_code(runner_exit_code, &producer_summaries);

    // Baseline lifecycle
    let baseline_context = baseline_provenance(&args, scoped_plan.as_ref(), &producer_summaries);
    let (baseline_comparison, baseline_exit_override, baseline_provenance) =
        if args.changed_since.is_some()
            && !args.baseline_flags.baseline
            && !args.baseline_flags.ignore_baseline
        {
            match process_changed_since_baseline(
                component,
                source_path,
                &args,
                &baseline_context,
                &lint_findings,
            ) {
                Ok(result) => result,
                Err(error) => {
                    eprintln!(
                    "[lint] Changed-since baseline unavailable; preserving full finding set: {}",
                    error.message
                );
                    (None, None, baseline_context)
                }
            }
        } else {
            process_baseline(source_path, &args, &lint_findings, baseline_context)?
        };

    let harness_error = lint_exit_code != 0
        && self_check_output_is_harness_failure(output.exit_code, &output.stdout, &output.stderr);
    let infrastructure_failure = producer_summaries.iter().any(|producer| {
        producer.tool == "homeboy-extension-runner"
            && producer.status == "error"
            && producer.metadata.contains_key("failure")
    });
    let hard_error = output.exit_code >= 2
        || harness_error
        || producer_summaries
            .iter()
            .any(|producer| producer.status == "error");
    let exit_code = effective_lint_exit_code(
        lint_exit_code,
        baseline_exit_override,
        hard_error,
        lint_findings.is_empty(),
    );
    let status = if exit_code == 0 {
        "passed"
    } else if infrastructure_failure {
        "error"
    } else {
        "failed"
    }
    .to_string();
    finish_scoped_lint_run_dirs(&child_run_dirs, exit_code == 0);
    let lint_clean = lint_findings.is_empty() && exit_code == 0;

    // Hint assembly — point to the auto-fix CTA for autofixable findings.
    //
    // Per the contract under #1459 (issue #1507), autofixable findings never
    // fail the run; they nudge. The CTA is rendered here in core, not by each
    // extension's runner, so every language extension benefits from a single
    // consistent prose. `homeboy review lint --fix` is the ergonomic adapter and is
    // listed first; the canonical `homeboy refactor --from lint --write`
    // invocation follows for users who want the longer form.
    if !lint_clean && !infrastructure_failure {
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

    hints.push("Full options: homeboy self docs commands/lint".to_string());

    if !args.baseline_flags.baseline && baseline_comparison.is_none() {
        hints.push(format!(
            "Save lint baseline: homeboy review lint {} --baseline",
            args.component_label
        ));
    }

    let hints = if hints.is_empty() { None } else { Some(hints) };

    // A non-zero exit with zero findings whose runner output shows infra
    // markers is a harness failure, not a real lint failure.
    Ok(LintRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        harness_error,
        infrastructure_failure,
        autofix: None,
        hints,
        baseline_comparison,
        baseline_provenance: Some(baseline_provenance),
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
        cargo_target: output.cargo_target,
    })
}

fn baseline_provenance(
    args: &LintRunWorkflowArgs,
    scoped_plan: Option<&ScopedLintPlan>,
    producer_summaries: &[homeboy_core::finding::FindingProducerSummary],
) -> lint_baseline::LintBaselineProvenance {
    let (scope, files) = if let Some(plan) = scoped_plan {
        (
            "changed".to_string(),
            plan.runs
                .iter()
                .flat_map(|run| run.changed_files.iter().cloned())
                .collect(),
        )
    } else if let Some(file) = &args.file {
        ("file".to_string(), vec![file.clone()])
    } else if let Some(glob) = &args.glob {
        ("glob".to_string(), vec![glob.clone()])
    } else {
        ("full".to_string(), Vec::new())
    };
    lint_baseline::LintBaselineProvenance::new(
        files,
        producer_summaries
            .iter()
            .map(|producer| producer.tool.clone())
            .collect(),
        scope,
        args.category.clone(),
        args.sniff_filters.errors_only,
        args.sniff_filters.sniffs.clone(),
        args.sniff_filters.exclude_sniffs.clone(),
    )
}

fn run_scoped_lint_runs(
    component: &Component,
    args: &LintRunWorkflowArgs,
    run_dir: &RunDir,
    runs: &[ScopedLintRun],
) -> homeboy_core::Result<ScopedLintOutput> {
    let mut success = true;
    let mut exit_code = 0;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut extension_phase_timings = Vec::new();
    let mut evidence = Vec::new();
    let mut child_run_dirs = Vec::new();
    // Read the declaration once, not once per scoped run: it is a property of
    // the component's lint extension, and every run in this loop is that same
    // extension.
    let requires_findings_evidence = crate::lint::declares_lint_findings_sidecar(component);
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
        let scoped_run_dir = (index > 0).then(RunDir::create).transpose()?;
        let active_run_dir = scoped_run_dir.as_ref().unwrap_or(run_dir);

        let runner = build_lint_runner(crate::lint::LintRunnerRequest {
            component,
            path_override: args.path_override.clone(),
            settings: &args.settings,
            summary: args.summary || args.json_summary,
            file: args.file.as_deref(),
            glob: Some(run.glob.as_str()),
            errors_only: args.sniff_filters.errors_only,
            sniffs: args.sniff_filters.sniffs.as_deref(),
            exclude_sniffs: args.sniff_filters.exclude_sniffs.as_deref(),
            category: args.category.as_deref(),
            step: run.step.as_deref(),
            changed_files: Some(run.changed_files.as_slice()),
            run_dir: active_run_dir,
        })?;
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
            .run()?;
        let stdout_artifact = write_command_artifact(run_dir, index, "stdout", &output.stdout)?;
        let stderr_artifact = write_command_artifact(run_dir, index, "stderr", &output.stderr)?;
        progress.finish(index, output.exit_code, stdout_artifact, stderr_artifact)?;
        let findings_file = active_run_dir.step_file(run_dir::files::LINT_FINDINGS);
        if requires_findings_evidence && !findings_file.is_file() {
            finish_scoped_lint_run_dir(scoped_run_dir.as_ref(), false);
            return Err(missing_findings_evidence_error(&findings_file));
        }
        let producers_file = active_run_dir.step_file(run_dir::files::LINT_PRODUCERS);
        let parsed_findings = lint_baseline::parse_findings_file(&findings_file);
        let declared_producers = parse_lint_producer_summaries_file(&producers_file);
        let (parsed_findings, declared_producers) = match (parsed_findings, declared_producers) {
            (Ok(findings), Ok(producers)) => (findings, producers),
            (Err(error), _) | (_, Err(error)) => {
                finish_scoped_lint_run_dir(scoped_run_dir.as_ref(), false);
                return Err(error);
            }
        };
        evidence.push(LintRunEvidence {
            findings: parsed_findings,
            declared_producers,
            findings_file,
            producers_file,
            changed_files: Some(run.changed_files.clone()),
            step: run.step.clone(),
            success: output.success,
            exit_code: output.exit_code,
        });
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
            if exit_code == 0 || output.exit_code >= 2 {
                exit_code = output.exit_code;
            }
        }
        if let Some(scoped_run_dir) = scoped_run_dir {
            child_run_dirs.push(scoped_run_dir);
        }
    }

    Ok(ScopedLintOutput {
        output: extension::RunnerOutput {
            exit_code,
            success,
            stdout,
            stderr,
            timed_out: false,
            child_resource: None,
            extension_phase_timings,
            cargo_target: None,
        },
        evidence,
        child_run_dirs,
    })
}

fn finish_scoped_lint_run_dirs(run_dirs: &[RunDir], success: bool) {
    for run_dir in run_dirs {
        finish_scoped_lint_run_dir(Some(run_dir), success);
    }
}

/// Reported only when the extension *declared* `lint.findings` and then did not
/// write it — i.e. it broke a contract it opted into. See
/// [`crate::lint::declares_lint_findings_sidecar`] for why the declaration
/// gates this at all.
fn missing_findings_evidence_error(path: &Path) -> homeboy_core::Error {
    homeboy_core::Error::internal_io(
        format!(
            "Lint runner declares the `{}` structured sidecar but did not produce it at {}",
            crate::lint::LINT_FINDINGS_SIDECAR,
            path.display()
        ),
        Some("lint.findings.evidence".to_string()),
    )
    .with_hint(format!(
        "Either write the sidecar on every exit path (seed it empty for a clean pass) or drop \
         `\"{}\"` from the extension manifest's `structured_sidecars`.",
        crate::lint::LINT_FINDINGS_SIDECAR
    ))
}

pub(super) fn finish_scoped_lint_run_dir(run_dir: Option<&RunDir>, success: bool) {
    if let Some(run_dir) = run_dir {
        run_dir.finish(success);
    }
}

pub fn run_self_check_lint_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> homeboy_core::Result<LintRunWorkflowResult> {
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
    observation: Option<&homeboy_core::observation::ActiveObservation>,
) -> homeboy_core::Result<LintRunWorkflowResult> {
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
Re-run `homeboy review lint {}` or skip only this gate with `--skip-checks=lint`.",
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
        infrastructure_failure: harness_error,
        autofix: None,
        hints,
        baseline_comparison: None,
        baseline_provenance: None,
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
        cargo_target: output.cargo_target,
        extension_phase_timings: Vec::new(),
    })
}

/// Process baseline lifecycle — save, load, compare.
fn process_baseline(
    source_path: &Path,
    args: &LintRunWorkflowArgs,
    lint_findings: &[HomeboyFinding],
    mut provenance: lint_baseline::LintBaselineProvenance,
) -> homeboy_core::Result<(
    Option<lint_baseline::BaselineComparison>,
    Option<i32>,
    lint_baseline::LintBaselineProvenance,
)> {
    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if args.baseline_flags.baseline {
        let saved = lint_baseline::save_baseline_for_scope(
            source_path,
            &args.component_id,
            lint_findings,
            Some(&provenance),
        )?;
        eprintln!(
            "[lint] Baseline saved to {} ({} findings)",
            saved.display(),
            lint_findings.len()
        );
    }

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline {
        if let Some(existing) =
            lint_baseline::load_baseline_for_scope(source_path, Some(&provenance))
        {
            provenance.compared = true;
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

    Ok((baseline_comparison, baseline_exit_override, provenance))
}

/// Run the same changed-file scope from the resolved merge base in an isolated
/// checkout. A persisted baseline cannot represent arbitrary PR ancestry.
fn process_changed_since_baseline(
    component: &Component,
    source_path: &Path,
    args: &LintRunWorkflowArgs,
    provenance: &lint_baseline::LintBaselineProvenance,
    lint_findings: &[HomeboyFinding],
) -> homeboy_core::Result<(
    Option<lint_baseline::BaselineComparison>,
    Option<i32>,
    lint_baseline::LintBaselineProvenance,
)> {
    let changed_since = args
        .changed_since
        .as_deref()
        .expect("changed-since baseline only runs with a git ref");
    let source = source_path.to_string_lossy();
    let base_ref = homeboy_core::git::resolve_merge_base(&source, changed_since)?;
    let repo_root = PathBuf::from(homeboy_core::git::get_git_root(&source)?);
    let component_suffix = source_path.strip_prefix(&repo_root).map_err(|_| {
        homeboy_core::Error::git_command_failed(format!(
            "component path {} is outside repository {}",
            source_path.display(),
            repo_root.display()
        ))
    })?;
    let changed_files = provenance.files.clone();
    let checkout_root = tempfile::tempdir().map_err(|error| {
        homeboy_core::Error::internal_io(
            format!("create changed-since baseline checkout: {error}"),
            Some("lint.changed_since_baseline".to_string()),
        )
    })?;
    let checkout = checkout_root.path().join("base");
    let add = Command::new("git")
        .current_dir(&repo_root)
        .args(["worktree", "add", "--detach", "--quiet"])
        .arg(&checkout)
        .arg(&base_ref)
        .output()
        .map_err(|error| homeboy_core::Error::git_command_failed(error.to_string()))?;
    if !add.status.success() {
        return Err(homeboy_core::Error::git_command_failed(format!(
            "git worktree add baseline {}: {}",
            base_ref,
            String::from_utf8_lossy(&add.stderr).trim()
        )));
    }

    let baseline_component_path = checkout.join(component_suffix);
    let mut baseline_component = component.clone();
    baseline_component.local_path = baseline_component_path.to_string_lossy().to_string();
    let mut baseline_args = args.clone();
    baseline_args.path_override = Some(baseline_component.local_path.clone());
    baseline_args.changed_only = true;
    baseline_args.changed_since = None;
    baseline_args.precomputed_changed_files = Some(changed_files);
    baseline_args.baseline_flags.ignore_baseline = true;
    let baseline_run_dir = RunDir::create()?;
    let baseline_result = run_main_lint_workflow(
        &baseline_component,
        &baseline_component_path,
        baseline_args,
        &baseline_run_dir,
    );
    let remove = Command::new("git")
        .current_dir(&repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(&checkout)
        .output();
    if let Ok(remove) = remove {
        if !remove.status.success() {
            eprintln!(
                "[lint] Failed to remove changed-since baseline checkout: {}",
                String::from_utf8_lossy(&remove.stderr).trim()
            );
        }
    }
    let baseline_findings = baseline_result?.findings.unwrap_or_default();

    let mut provenance = provenance.clone();
    provenance.compared = true;
    provenance.base_ref = Some(base_ref);
    let comparison = lint_baseline::compare_against_findings(lint_findings, &baseline_findings);
    let exit_override = Some(if comparison.drift_increased { 1 } else { 0 });
    Ok((Some(comparison), exit_override, provenance))
}
