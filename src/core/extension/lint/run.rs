//! Lint workflow orchestration — runs lint, resolves changed-file scoping,
//! drives autofix, processes baseline lifecycle, and assembles results.
//!
//! Mirrors `core/extension/test/run.rs` — the command layer provides CLI args,
//! this module owns all business logic and returns a structured result.

use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::engine::shell;
use crate::core::extension::lint::baseline as lint_baseline;
use crate::core::extension::lint::build_lint_runner;
use crate::core::extension::self_check::SelfCheckCaptureMetadata;
use crate::core::extension::{self, ExtensionCapability, LintChangedFileRoute};
use crate::core::finding::{FindingProducerSummary, FindingSource, HomeboyFinding};
use crate::core::git;
use crate::core::refactor::AppliedRefactor;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Arguments for the main lint workflow — populated by the command layer from CLI flags.
#[derive(Debug, Clone)]
pub struct LintRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, serde_json::Value)>,
    pub summary: bool,
    pub file: Option<String>,
    pub glob: Option<String>,
    pub changed_only: bool,
    pub changed_since: Option<String>,
    pub errors_only: bool,
    pub sniffs: Option<String>,
    pub exclude_sniffs: Option<String>,
    pub category: Option<String>,
    pub ci_env: Vec<(String, String)>,
    pub baseline_flags: BaselineFlags,
    pub json_summary: bool,
}

/// Result of the main lint workflow — ready for report assembly.
#[derive(Debug, Clone, Serialize)]
pub struct LintRunWorkflowResult {
    pub status: String,
    pub component: String,
    pub exit_code: i32,
    pub autofix: Option<AppliedRefactor>,
    pub hints: Option<Vec<String>>,
    pub baseline_comparison: Option<lint_baseline::BaselineComparison>,
    pub findings: Option<Vec<HomeboyFinding>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    pub summary: Option<LintSummaryOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub self_check_capture: Option<SelfCheckCaptureMetadata>,
}

/// Compact lint summary for automation consumers.
#[derive(Debug, Clone, Serialize)]
pub struct LintSummaryOutput {
    pub total_findings: usize,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub categories: BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_findings: Vec<HomeboyFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub producer_summaries: Vec<FindingProducerSummary>,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedLintRun {
    glob: String,
    step: Option<String>,
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
) -> crate::core::Result<LintRunWorkflowResult> {
    let scoped_runs = resolve_scoped_lint_runs(component, &args)?;

    // Early exit if changed-file mode produced no files
    if let Some(ref runs) = scoped_runs {
        if runs.is_empty() {
            return Ok(LintRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                autofix: None,
                hints: None,
                baseline_comparison: None,
                findings: None,
                producer_summaries: Vec::new(),
                summary: if args.json_summary {
                    Some(build_lint_summary(&[], &[], 0))
                } else {
                    None
                },
                self_check_capture: None,
            });
        }
    }

    // Run lint
    let output = if let Some(runs) = scoped_runs {
        run_scoped_lint_runs(component, &args, run_dir, &runs)?
    } else {
        let runner = build_lint_runner(
            component,
            args.path_override.clone(),
            &args.settings,
            args.summary || args.json_summary,
            args.file.as_deref(),
            args.glob.as_deref(),
            args.errors_only,
            args.sniffs.as_deref(),
            args.exclude_sniffs.as_deref(),
            args.category.as_deref(),
            None,
            run_dir,
        )?;
        let runner = args
            .ci_env
            .iter()
            .fold(runner, |runner, (key, value)| runner.env(key, value));
        runner
            .env_if(
                args.changed_since.is_some(),
                "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES",
                "1",
            )
            .passthrough(!args.json_summary)
            .run()?
    };

    let lint_findings_file = run_dir.step_file(run_dir::files::LINT_FINDINGS);
    let lint_producers_file = run_dir.step_file(run_dir::files::LINT_PRODUCERS);
    let raw_lint_findings = lint_baseline::parse_findings_file(&lint_findings_file)?;
    let lint_findings = filter_lint_findings(raw_lint_findings, &args);
    let declared_producers = parse_lint_producer_summaries_file(&lint_producers_file)?;
    let producer_summaries = build_lint_producer_summaries(
        &lint_findings,
        &lint_findings_file,
        &lint_producers_file,
        declared_producers,
        output.success,
        output.exit_code,
        None,
    );

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

    Ok(LintRunWorkflowResult {
        status,
        component: args.component_label,
        exit_code,
        autofix: None,
        hints,
        baseline_comparison,
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
    })
}

fn filter_lint_findings(
    findings: Vec<HomeboyFinding>,
    args: &LintRunWorkflowArgs,
) -> Vec<HomeboyFinding> {
    let included_sniffs = parse_csv_filter(args.sniffs.as_deref());
    let excluded_sniffs = parse_csv_filter(args.exclude_sniffs.as_deref());
    let category = args
        .category
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    findings
        .into_iter()
        .filter(|finding| {
            category.is_none_or(|expected| finding.category.as_deref() == Some(expected))
                && (included_sniffs.is_empty()
                    || included_sniffs
                        .iter()
                        .any(|expected| finding_matches_sniff(finding, expected)))
                && !excluded_sniffs
                    .iter()
                    .any(|excluded| finding_matches_sniff(finding, excluded))
        })
        .collect()
}

fn parse_csv_filter(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn finding_matches_sniff(finding: &HomeboyFinding, sniff: &str) -> bool {
    finding.category.as_deref() == Some(sniff)
        || finding.rule.as_deref().is_some_and(|rule| rule == sniff)
        || finding.fingerprint.as_deref() == Some(sniff)
        || finding
            .fingerprint
            .as_deref()
            .is_some_and(|id| id.split("::").any(|part| part == sniff) || id.ends_with(sniff))
}

fn build_lint_producer_summaries(
    findings: &[HomeboyFinding],
    findings_source_path: &Path,
    producers_source_path: &Path,
    declared_producers: Vec<FindingProducerSummary>,
    runner_success: bool,
    runner_exit_code: i32,
    step: Option<&str>,
) -> Vec<FindingProducerSummary> {
    let mut counts = BTreeMap::<String, usize>::new();
    for finding in findings {
        *counts.entry(finding.tool.clone()).or_insert(0) += 1;
    }

    if !declared_producers.is_empty() {
        return declared_producers
            .into_iter()
            .map(|mut summary| {
                summary.finding_count = counts
                    .get(&summary.tool)
                    .copied()
                    .unwrap_or(summary.finding_count);
                if summary.source.is_none() {
                    summary.source = Some(
                        FindingSource::new("sidecar")
                            .label("lint-producers")
                            .path(producers_source_path.display().to_string()),
                    );
                }
                if summary.step.is_none() {
                    summary.step = step.map(str::to_string);
                }
                summary
                    .metadata
                    .entry("source_sidecar".to_string())
                    .or_insert_with(|| serde_json::json!("lint-producers"));
                summary
                    .metadata
                    .entry("source_sidecar_path".to_string())
                    .or_insert_with(|| {
                        serde_json::json!(producers_source_path.display().to_string())
                    });
                summary
                    .metadata
                    .entry("exit_code".to_string())
                    .or_insert_with(|| serde_json::json!(runner_exit_code));
                summary
            })
            .collect();
    }

    if counts.is_empty() {
        counts.insert("lint".to_string(), 0);
    }

    counts
        .into_iter()
        .map(|(tool, finding_count)| {
            let status = if runner_exit_code >= 2 || (!runner_success && finding_count == 0) {
                "error"
            } else if finding_count > 0 {
                "failed"
            } else {
                "passed"
            };
            let mut summary = FindingProducerSummary::new(tool, status)
                .finding_count(finding_count)
                .source(
                    FindingSource::new("sidecar")
                        .label("lint-findings")
                        .path(findings_source_path.display().to_string()),
                )
                .metadata("source_sidecar", "lint-findings")
                .metadata(
                    "source_sidecar_path",
                    findings_source_path.display().to_string(),
                )
                .metadata("exit_code", runner_exit_code);
            if let Some(step) = step {
                summary = summary.step(step.to_string());
            }
            summary
        })
        .collect()
}

fn parse_lint_producer_summaries_file(
    path: &Path,
) -> crate::core::Result<Vec<FindingProducerSummary>> {
    fn parse_error(path: &Path, error: std::io::Error) -> crate::core::Error {
        crate::core::Error::internal_io(
            format!(
                "Failed to read lint producer summaries file {}: {}",
                path.display(),
                error
            ),
            Some("lint.producers.parse".to_string()),
        )
    }

    fn json_error(path: &Path, error: serde_json::Error) -> crate::core::Error {
        crate::core::Error::internal_io(
            format!(
                "Malformed lint producer summaries JSON in {}: {}",
                path.display(),
                error
            ),
            Some("lint.producers.parse".to_string()),
        )
    }

    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| parse_error(path, e))?;

    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(&content).map_err(|e| json_error(path, e))
}

fn normalize_empty_finding_exit_code(
    exit_code: i32,
    success: bool,
    lint_findings: &[HomeboyFinding],
    producer_summaries: &[FindingProducerSummary],
) -> i32 {
    if lint_findings.is_empty()
        && !success
        && exit_code == 1
        && !producer_summaries
            .iter()
            .any(|summary| summary.status != "passed")
    {
        0
    } else {
        exit_code
    }
}

fn normalize_finding_exit_code(exit_code: i32, lint_findings: &[HomeboyFinding]) -> i32 {
    if !lint_findings.is_empty() && exit_code == 0 {
        1
    } else {
        exit_code
    }
}

fn effective_lint_exit_code(exit_code: i32, baseline_exit_override: Option<i32>) -> i32 {
    match baseline_exit_override {
        Some(0) if exit_code >= 2 => exit_code,
        Some(override_code) => override_code,
        None => exit_code,
    }
}

fn build_lint_summary(
    findings: &[HomeboyFinding],
    producer_summaries: &[FindingProducerSummary],
    exit_code: i32,
) -> LintSummaryOutput {
    let mut categories = BTreeMap::new();
    for finding in findings {
        let category = finding
            .category
            .clone()
            .unwrap_or_else(|| "uncategorized".to_string());
        *categories.entry(category).or_insert(0) += 1;
    }

    LintSummaryOutput {
        total_findings: findings.len(),
        categories,
        top_findings: findings.iter().take(20).cloned().collect(),
        producer_summaries: producer_summaries.to_vec(),
        exit_code,
    }
}

fn build_autofix_hint(args: &LintRunWorkflowArgs) -> String {
    let lint_command = lint_autofix_command(args);

    if refactor_can_preserve_scope(args) {
        let refactor_command = refactor_autofix_command(args);
        format!("Auto-fix: {lint_command} (or {refactor_command})")
    } else {
        format!("Auto-fix: {lint_command}")
    }
}

fn lint_autofix_command(args: &LintRunWorkflowArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "lint".to_string(),
        args.component_label.clone(),
    ];

    append_common_scope_args(&mut parts, args);
    parts.push("--fix".to_string());

    shell::quote_args(&parts)
}

fn refactor_autofix_command(args: &LintRunWorkflowArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "refactor".to_string(),
        args.component_label.clone(),
    ];

    append_path_and_changed_since_args(&mut parts, args);
    parts.extend([
        "--from".to_string(),
        "lint".to_string(),
        "--write".to_string(),
    ]);

    shell::quote_args(&parts)
}

fn refactor_can_preserve_scope(args: &LintRunWorkflowArgs) -> bool {
    args.file.is_none() && args.glob.is_none() && !args.changed_only
}

fn append_common_scope_args(parts: &mut Vec<String>, args: &LintRunWorkflowArgs) {
    append_path_and_changed_since_args(parts, args);
    if let Some(file) = &args.file {
        parts.push("--file".to_string());
        parts.push(file.clone());
    }
    if let Some(glob) = &args.glob {
        parts.push("--glob".to_string());
        parts.push(glob.clone());
    }
    if args.changed_only {
        parts.push("--changed-only".to_string());
    }
}

fn append_path_and_changed_since_args(parts: &mut Vec<String>, args: &LintRunWorkflowArgs) {
    if let Some(path) = &args.path_override {
        parts.push("--path".to_string());
        parts.push(path.clone());
    }
    if let Some(changed_since) = &args.changed_since {
        parts.push("--changed-since".to_string());
        parts.push(changed_since.clone());
    }
}

fn run_scoped_lint_runs(
    component: &Component,
    args: &LintRunWorkflowArgs,
    run_dir: &RunDir,
    runs: &[ScopedLintRun],
) -> crate::core::Result<extension::RunnerOutput> {
    let mut success = true;
    let mut exit_code = 0;

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
            args.errors_only,
            args.sniffs.as_deref(),
            args.exclude_sniffs.as_deref(),
            args.category.as_deref(),
            run.step.as_deref(),
            active_run_dir,
        )?;
        let runner = args
            .ci_env
            .iter()
            .fold(runner, |runner, (key, value)| runner.env(key, value));
        let output = runner
            .env_if(
                args.changed_since.is_some(),
                "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES",
                "1",
            )
            .passthrough(!args.json_summary)
            .run()?;

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
        stdout: String::new(),
        stderr: String::new(),
        child_resource: None,
    })
}

pub fn run_self_check_lint_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> crate::core::Result<LintRunWorkflowResult> {
    let output = extension::self_check::run_self_checks_with_passthrough(
        component,
        ExtensionCapability::Lint,
        source_path,
        !json_summary,
    )?;
    let status = if output.success { "passed" } else { "failed" }.to_string();
    let hints = (!output.success).then(|| {
        vec![format!(
            "Fix the failing self-check command declared in {}'s homeboy.json scripts.lint",
            component.id
        )]
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

    Ok(LintRunWorkflowResult {
        status,
        component: component_label,
        exit_code: output.exit_code,
        autofix: None,
        hints,
        baseline_comparison: None,
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
    })
}

/// Resolve runner-compatible scopes from --changed-only or --changed-since flags.
///
/// Returns `Some(Vec::new())` when changed-file mode is active but no compatible
/// files were found — the caller should treat this as an early "passed" exit.
/// Returns `None` when no changed-file scoping is active (use args.glob directly).
fn resolve_scoped_lint_runs(
    component: &Component,
    args: &LintRunWorkflowArgs,
) -> crate::core::Result<Option<Vec<ScopedLintRun>>> {
    if args.changed_only {
        let uncommitted = git::get_uncommitted_changes(&component.local_path)?;
        let mut changed_files: Vec<String> = Vec::new();
        changed_files.extend(uncommitted.staged);
        changed_files.extend(uncommitted.unstaged);
        changed_files.extend(uncommitted.untracked);

        if changed_files.is_empty() {
            println!("No files in working tree changes");
            return Ok(Some(Vec::new()));
        }

        eprintln!(
            "Linting {} changed file(s) (--changed-only is file-scoped; findings may be outside changed hunks)",
            changed_files.len()
        );

        Ok(Some(build_changed_lint_runs(component, &changed_files)))
    } else if let Some(ref git_ref) = args.changed_since {
        let changed_files = git::get_files_changed_since(&component.local_path, git_ref)?;

        if changed_files.is_empty() {
            println!("No files changed since {}", git_ref);
            return Ok(Some(Vec::new()));
        }

        Ok(Some(build_changed_lint_runs(component, &changed_files)))
    } else {
        Ok(None)
    }
}

fn build_changed_lint_runs(component: &Component, changed_files: &[String]) -> Vec<ScopedLintRun> {
    let routes = changed_file_routes_for_component(component);
    build_changed_lint_runs_with_routes(component, changed_files, &routes)
}

fn build_changed_lint_runs_with_routes(
    component: &Component,
    changed_files: &[String],
    routes: &[LintChangedFileRoute],
) -> Vec<ScopedLintRun> {
    if routes.is_empty() {
        return vec![ScopedLintRun {
            glob: glob_for_files(&component.local_path, changed_files),
            step: None,
        }];
    }

    let mut runs = Vec::new();
    for route in routes {
        let matched_files: Vec<String> = changed_files
            .iter()
            .filter(|file| route_matches_file(route, file))
            .cloned()
            .collect();

        if !matched_files.is_empty() {
            runs.push(ScopedLintRun {
                glob: glob_for_files(&component.local_path, &matched_files),
                step: Some(route.step.clone()),
            });
        }
    }
    runs
}

fn changed_file_routes_for_component(component: &Component) -> Vec<LintChangedFileRoute> {
    let Some(extensions) = component.extensions.as_ref() else {
        return Vec::new();
    };

    extensions
        .keys()
        .filter_map(|extension_id| extension::load_extension(extension_id).ok())
        .filter_map(|manifest| manifest.lint)
        .flat_map(|lint| lint.changed_file_routes)
        .collect()
}

fn route_matches_file(route: &LintChangedFileRoute, file: &str) -> bool {
    if !route.extensions.is_empty() && has_extension(file, &route.extensions) {
        return true;
    }

    route
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

fn glob_for_files(root: &str, files: &[String]) -> String {
    let abs_files: Vec<String> = files
        .iter()
        .map(|file| format!("{}/{}", root, file))
        .collect();

    if abs_files.len() == 1 {
        abs_files[0].clone()
    } else {
        format!("{{{}}}", abs_files.join(","))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::ComponentScriptsConfig;
    use crate::core::engine::baseline::BaselineFlags;

    fn component(root: &str) -> Component {
        Component::new(
            "fixture".to_string(),
            root.to_string(),
            "".to_string(),
            None,
        )
    }

    fn split_lint_routes() -> Vec<LintChangedFileRoute> {
        vec![
            LintChangedFileRoute {
                extensions: vec!["php".to_string()],
                globs: Vec::new(),
                step: "phpcs,phpstan".to_string(),
            },
            LintChangedFileRoute {
                extensions: vec![
                    "js".to_string(),
                    "jsx".to_string(),
                    "ts".to_string(),
                    "tsx".to_string(),
                ],
                globs: Vec::new(),
                step: "eslint".to_string(),
            },
        ]
    }

    fn lint_args() -> LintRunWorkflowArgs {
        LintRunWorkflowArgs {
            component_label: "demo".to_string(),
            component_id: "demo".to_string(),
            path_override: None,
            settings: Vec::new(),
            summary: false,
            file: None,
            glob: None,
            changed_only: false,
            changed_since: None,
            errors_only: false,
            sniffs: None,
            exclude_sniffs: None,
            category: None,
            ci_env: Vec::new(),
            baseline_flags: BaselineFlags::default(),
            json_summary: false,
        }
    }

    #[test]
    fn autofix_hint_preserves_changed_since_scope() {
        let mut args = lint_args();
        args.path_override = Some("/tmp/pr checkout".to_string());
        args.changed_since = Some("origin/main".to_string());

        let hint = build_autofix_hint(&args);

        assert!(hint.contains(
            "homeboy lint demo --path '/tmp/pr checkout' --changed-since origin/main --fix"
        ));
        assert!(hint.contains(
            "homeboy refactor demo --path '/tmp/pr checkout' --changed-since origin/main --from lint --write"
        ));
    }

    #[test]
    fn autofix_hint_preserves_changed_only_and_file_scope() {
        let mut args = lint_args();
        args.file = Some("src/lib.rs".to_string());
        args.changed_only = true;

        let hint = build_autofix_hint(&args);

        assert!(hint.contains("homeboy lint demo --file src/lib.rs --changed-only --fix"));
        assert!(!hint.contains("homeboy refactor"));
    }

    #[test]
    fn test_run_self_check_lint_workflow() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("lint.sh"), "printf lint-ok\n")
            .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: vec!["sh lint.sh".to_string()],
            test: Vec::new(),
            build: Vec::new(),
            bench: Vec::new(),
            trace: Vec::new(),
            deps: Vec::new(),
        });

        let result =
            run_self_check_lint_workflow(&component, dir.path(), "fixture".to_string(), false)
                .expect("lint self-check should run");

        assert_eq!(result.status, "passed");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.component, "fixture");
    }

    #[test]
    fn test_run_main_lint_workflow() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init should run");
        let run_dir = RunDir::create().expect("run dir");
        let mut args = lint_args();
        args.changed_only = true;

        let result = run_main_lint_workflow(
            &component(&dir.path().to_string_lossy()),
            dir.path(),
            args,
            &run_dir,
        )
        .expect("unchanged git repo should skip lint runner");

        assert_eq!(result.status, "passed");
        assert_eq!(result.exit_code, 0);
        assert!(result.findings.is_none());
    }

    #[test]
    fn lint_summary_counts_categories_and_caps_top_findings() {
        let findings = (0..25)
            .map(|index| {
                let category = if index % 2 == 0 {
                    "style"
                } else {
                    "correctness"
                };
                HomeboyFinding::builder("lint", "message")
                    .category(category)
                    .fingerprint(format!("src/file-{index}.rs::rule"))
                    .build()
            })
            .collect::<Vec<_>>();

        let producers = build_lint_producer_summaries(
            &findings,
            Path::new("lint-findings.json"),
            Path::new("lint-producers.json"),
            Vec::new(),
            false,
            1,
            None,
        );
        let summary = build_lint_summary(&findings, &producers, 1);

        assert_eq!(summary.total_findings, 25);
        assert_eq!(summary.categories.get("style"), Some(&13));
        assert_eq!(summary.categories.get("correctness"), Some(&12));
        assert_eq!(summary.top_findings.len(), 20);
        assert_eq!(summary.producer_summaries[0].finding_count, 25);
        assert_eq!(summary.exit_code, 1);
    }

    #[test]
    fn producer_summary_sidecar_represents_zero_finding_tools() {
        let dir = tempfile::tempdir().expect("temp dir");
        let producers_file = dir.path().join(run_dir::files::LINT_PRODUCERS);
        std::fs::write(
            &producers_file,
            r#"[
                {"tool":"phpcs","status":"passed","finding_count":0,"step":"phpcs"},
                {"tool":"phpstan","status":"passed","finding_count":0,"step":"phpstan"}
            ]"#,
        )
        .expect("producer summaries should be written");

        let declared =
            parse_lint_producer_summaries_file(&producers_file).expect("producers parse");
        let summaries = build_lint_producer_summaries(
            &[],
            &dir.path().join(run_dir::files::LINT_FINDINGS),
            &producers_file,
            declared,
            true,
            0,
            None,
        );

        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].tool, "phpcs");
        assert_eq!(summaries[0].finding_count, 0);
        assert_eq!(summaries[0].status, "passed");
        let producers_path = producers_file.to_string_lossy().to_string();
        assert_eq!(
            summaries[0].source.as_ref().unwrap().path.as_deref(),
            Some(producers_path.as_str())
        );
    }

    #[test]
    fn filter_lint_findings_keeps_requested_category_only() {
        let mut args = lint_args();
        args.category = Some("security".to_string());
        let findings = vec![
            lint_finding(
                "a",
                "security",
                "WordPress.Security.ValidatedSanitizedInput",
            ),
            lint_finding("b", "database", "WordPress.DB.PreparedSQL"),
            lint_finding("c", "eslint", "react-hooks/rules-of-hooks"),
        ];

        let filtered = filter_lint_findings(findings, &args);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].fingerprint.as_deref(), Some("a"));
    }

    #[test]
    fn filter_lint_findings_honors_include_and_exclude_sniffs() {
        let mut args = lint_args();
        args.sniffs = Some(
            "WordPress.Security.ValidatedSanitizedInput,Generic.WhiteSpace.ScopeIndent".to_string(),
        );
        args.exclude_sniffs = Some("Generic.WhiteSpace.ScopeIndent".to_string());
        let findings = vec![
            lint_finding(
                "inc/a.php::WordPress.Security.ValidatedSanitizedInput",
                "security",
                "WordPress.Security.ValidatedSanitizedInput",
            ),
            lint_finding(
                "inc/b.php::Generic.WhiteSpace.ScopeIndent",
                "whitespace",
                "Generic.WhiteSpace.ScopeIndent",
            ),
            lint_finding(
                "inc/c.php::WordPress.DB.PreparedSQL",
                "database",
                "WordPress.DB.PreparedSQL",
            ),
        ];

        let filtered = filter_lint_findings(findings, &args);

        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].fingerprint.as_deref(),
            Some("inc/a.php::WordPress.Security.ValidatedSanitizedInput")
        );
    }

    #[test]
    fn empty_filtered_findings_turn_lint_finding_exit_into_pass() {
        let exit_code = normalize_empty_finding_exit_code(1, false, &[], &[]);

        assert_eq!(exit_code, 0);
    }

    #[test]
    fn failed_zero_finding_producer_keeps_lint_failure() {
        let producer_summaries = vec![
            FindingProducerSummary::new("phpcs", "passed").finding_count(0),
            FindingProducerSummary::new("phpstan", "failed").finding_count(0),
        ];
        let exit_code = normalize_empty_finding_exit_code(1, false, &[], &producer_summaries);

        assert_eq!(exit_code, 1);
    }

    #[test]
    fn empty_filtered_findings_do_not_hide_infrastructure_errors() {
        let exit_code = normalize_empty_finding_exit_code(2, false, &[], &[]);

        assert_eq!(exit_code, 2);
    }

    #[test]
    fn findings_force_failure_when_runner_exits_cleanly() {
        let exit_code = normalize_finding_exit_code(0, &[lint_finding("a", "security", "rule")]);

        assert_eq!(exit_code, 1);
    }

    #[test]
    fn baseline_clean_override_honors_known_findings_but_not_infrastructure_errors() {
        assert_eq!(effective_lint_exit_code(1, Some(0)), 0);
        assert_eq!(effective_lint_exit_code(2, Some(0)), 2);
    }

    #[test]
    fn manifest_changed_php_files_route_to_php_steps_only() {
        let component = component("/repo");
        let runs = build_changed_lint_runs_with_routes(
            &component,
            &["data-machine.php".to_string(), "inc/Foo.php".to_string()],
            &split_lint_routes(),
        );

        assert_eq!(
            runs,
            vec![ScopedLintRun {
                glob: "{/repo/data-machine.php,/repo/inc/Foo.php}".to_string(),
                step: Some("phpcs,phpstan".to_string()),
            }]
        );
    }

    #[test]
    fn manifest_changed_markdown_files_do_not_route_to_eslint() {
        let component = component("/repo");
        let runs = build_changed_lint_runs_with_routes(
            &component,
            &[
                "docs/core-system/agent-bundles.md".to_string(),
                "README.md".to_string(),
            ],
            &split_lint_routes(),
        );

        assert!(runs.is_empty());
    }

    #[test]
    fn manifest_changed_mixed_php_and_js_files_split_by_runner() {
        let component = component("/repo");
        let runs = build_changed_lint_runs_with_routes(
            &component,
            &[
                "inc/Foo.php".to_string(),
                "docs/notes.md".to_string(),
                "assets/app.js".to_string(),
                "assets/view.tsx".to_string(),
            ],
            &split_lint_routes(),
        );

        assert_eq!(
            runs,
            vec![
                ScopedLintRun {
                    glob: "/repo/inc/Foo.php".to_string(),
                    step: Some("phpcs,phpstan".to_string()),
                },
                ScopedLintRun {
                    glob: "{/repo/assets/app.js,/repo/assets/view.tsx}".to_string(),
                    step: Some("eslint".to_string()),
                },
            ]
        );
    }

    #[test]
    fn manifest_changed_files_can_route_by_glob() {
        let component = component("/repo");
        let routes = vec![LintChangedFileRoute {
            extensions: Vec::new(),
            globs: vec!["assets/**/*.css".to_string()],
            step: "stylelint".to_string(),
        }];
        let runs = build_changed_lint_runs_with_routes(
            &component,
            &["assets/css/admin.css".to_string(), "README.md".to_string()],
            &routes,
        );

        assert_eq!(
            runs,
            vec![ScopedLintRun {
                glob: "/repo/assets/css/admin.css".to_string(),
                step: Some("stylelint".to_string()),
            }]
        );
    }

    #[test]
    fn lint_config_deserializes_changed_file_routes() {
        let config: crate::core::extension::LintConfig = serde_json::from_str(
            r#"{
                "extension_script": "scripts/lint.sh",
                "changed_file_routes": [
                    { "extensions": ["php"], "step": "phpcs,phpstan" },
                    { "globs": ["assets/**/*.css"], "step": "stylelint" }
                ]
            }"#,
        )
        .expect("parse lint config");

        assert_eq!(config.changed_file_routes.len(), 2);
        assert_eq!(config.changed_file_routes[0].extensions, vec!["php"]);
        assert_eq!(config.changed_file_routes[0].step, "phpcs,phpstan");
        assert_eq!(config.changed_file_routes[1].globs, vec!["assets/**/*.css"]);
        assert_eq!(config.changed_file_routes[1].step, "stylelint");
    }

    #[test]
    fn non_wordpress_changed_files_keep_existing_single_runner_scope() {
        let component = Component::new(
            "fixture".to_string(),
            "/repo".to_string(),
            "".to_string(),
            None,
        );
        let runs = build_changed_lint_runs(
            &component,
            &["src/main.rs".to_string(), "README.md".to_string()],
        );

        assert_eq!(
            runs,
            vec![ScopedLintRun {
                glob: "{/repo/src/main.rs,/repo/README.md}".to_string(),
                step: None,
            }]
        );
    }

    fn lint_finding(id: &str, category: &str, rule: &str) -> HomeboyFinding {
        HomeboyFinding::builder("lint", "message")
            .category(category)
            .rule(rule)
            .fingerprint(id)
            .build()
    }
}
