use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use homeboy::core::component;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::{TraceCheckoutProvenance, TraceCommandOutput};
use homeboy::core::git;

use super::aggregate::{
    aggregate_metric, aggregate_span, TraceAggregateMetricSample, TraceAggregateSpanSample,
};
use super::matrix::{aggregate_to_compare_input, write_json_artifact};
use super::output::compare_trace_aggregates_with_focus;
use super::{
    apply_resolved_trace_secret_env, attach_span_metadata, classification_summaries,
    cli_span_definitions_for_args, load_rig_context, plan_trace_run_order, resolve_component_id,
    resolve_trace_secret_env_once, rig_component_for_trace, run_trace_guardrails_for_args,
    trace_span_metadata_for_args, ResolvedTraceSecretEnv, TraceArgs,
};
use crate::commands::utils::args::PositionalComponentArgs;
use crate::commands::CmdResult;

pub(super) fn run_compare_targets(args: TraceArgs) -> CmdResult<TraceCommandOutput> {
    if args.keep_overlay {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--keep-overlay",
            "trace compare runs baseline and candidate in separate target checkouts; overlays must be reverted after each run",
            None,
            None,
        ));
    }

    let baseline = required_target(args.baseline_target.as_deref(), "--baseline-target")?;
    let candidate = required_target(args.candidate.as_deref(), "--candidate")?;
    let (component_id, base_component) = compare_target_component(&args)?;
    let scenario_id = args
        .scenario_arg
        .clone()
        .or_else(|| {
            args.compare_after
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())
        })
        .ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec!["trace scenario".to_string()])
        })?;
    let output_dir = args.output_dir.clone().unwrap_or_else(|| {
        PathBuf::from(".homeboy")
            .join("trace-compare")
            .join(format!(
                "{}-{}",
                scenario_id,
                chrono::Utc::now().format("%Y%m%d%H%M%S")
            ))
    });
    std::fs::create_dir_all(&output_dir).map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to create trace compare output dir {}: {}",
                output_dir.display(),
                err
            ),
            Some("trace.compare.output_dir".to_string()),
        )
    })?;

    let baseline_target = resolve_target("baseline", baseline, &base_component.local_path)?;
    let candidate_target = resolve_target("candidate", candidate, &base_component.local_path)?;
    let mut proof_args = args.clone();
    proof_args.comp.component = Some(component_id.clone());
    proof_args.scenario = Some(scenario_id.clone());
    proof_args.component_arg = None;
    proof_args.scenario_arg = None;
    proof_args.compare_after = None;
    let proof = run_target_proof_matrix(
        &proof_args,
        &component_id,
        &scenario_id,
        &baseline_target,
        &candidate_target,
    )?;
    let baseline_aggregate = proof.baseline;
    let candidate_aggregate = proof.candidate;

    let baseline_path = output_dir.join("baseline.aggregate.json");
    let candidate_path = output_dir.join("candidate.aggregate.json");
    let compare_path = output_dir.join("compare.json");
    let summary_path = output_dir.join("summary.md");
    write_json_artifact(&baseline_path, &baseline_aggregate)?;
    write_json_artifact(&candidate_path, &candidate_aggregate)?;

    let mut compare = compare_trace_aggregates_with_focus(
        &baseline_path,
        aggregate_to_compare_input(&baseline_aggregate),
        &candidate_path,
        aggregate_to_compare_input(&candidate_aggregate),
        &args.focus_spans,
        args.regression_threshold,
        args.regression_min_delta_ms,
        &args.metric_guardrails,
    );
    compare.before_target = Some(baseline_target.input.clone());
    compare.after_target = Some(candidate_target.input.clone());
    compare.before_git_sha = baseline_target.git_sha;
    compare.after_git_sha = candidate_target.git_sha;
    compare.before_status = Some(baseline_aggregate.status.clone());
    compare.after_status = Some(candidate_aggregate.status.clone());
    compare.before_exit_code = Some(baseline_aggregate.exit_code);
    compare.after_exit_code = Some(candidate_aggregate.exit_code);
    compare.output_dir = Some(output_dir.to_string_lossy().to_string());
    compare.summary_path = Some(summary_path.to_string_lossy().to_string());
    compare.proof_run_order = proof.run_order;
    compare.caveats = trace_compare_caveats(&args);
    compare.browser_proof = browser_proof_for_runs(
        &baseline_target.input,
        &candidate_target.input,
        &baseline_aggregate,
        &candidate_aggregate,
    )?;
    write_json_artifact(&compare_path, &compare)?;
    std::fs::write(
        &summary_path,
        super::output::render_compare_markdown(&compare),
    )
    .map_err(|err| {
        homeboy::core::Error::internal_io(
            format!(
                "Failed to write trace compare summary {}: {}",
                summary_path.display(),
                err
            ),
            Some("trace.compare.summary".to_string()),
        )
    })?;

    let failed = !baseline_aggregate.passed
        || !candidate_aggregate.passed
        || compare.focus_status.as_deref() == Some("fail")
        || compare.guardrail_status.as_deref() == Some("fail")
        || compare.metric_guardrail_status.as_deref() == Some("fail");
    Ok((
        TraceCommandOutput::Compare(compare),
        if failed { 1 } else { 0 },
    ))
}

fn compare_target_component(
    args: &TraceArgs,
) -> homeboy::core::Result<(String, component::Component)> {
    let rig_context = load_rig_context(args.rig.as_deref())?;
    let target_comp = PositionalComponentArgs {
        component: args.component_arg.clone().or_else(|| args.scenario.clone()),
        path: args.comp.path.clone(),
    };
    let component_id = resolve_component_id(
        &target_comp,
        rig_context.as_ref().map(|context| &context.rig_spec),
    )?;
    let rig_component = rig_context
        .as_ref()
        .and_then(|context| rig_component_for_trace(&context.rig_spec, &component_id));
    let component = rig_component.map(Ok).unwrap_or_else(|| {
        component::resolve_effective(Some(&component_id), args.comp.path.as_deref(), None)
    })?;
    Ok((component_id, component))
}

fn required_target<'a>(
    value: Option<&'a str>,
    name: &'static str,
) -> homeboy::core::Result<&'a str> {
    value.ok_or_else(|| homeboy::core::Error::validation_missing_argument(vec![name.to_string()]))
}

struct TargetProofMatrix {
    baseline: extension_trace::TraceAggregateOutput,
    candidate: extension_trace::TraceAggregateOutput,
    run_order: Vec<extension_trace::TraceCompareRunOrderOutput>,
}

fn run_target_proof_matrix(
    args: &TraceArgs,
    component_id: &str,
    scenario_id: &str,
    baseline_target: &ResolvedCompareTarget,
    candidate_target: &ResolvedCompareTarget,
) -> homeboy::core::Result<TargetProofMatrix> {
    let repeat = args.repeat.max(1);
    let plan = plan_trace_run_order(repeat, args.schedule, &["baseline", "candidate"]);
    let resolved_trace_secret_env =
        resolve_trace_secret_env_once(&args.secret_env, Some(component_id))?;
    let span_metadata = trace_span_metadata_for_args(args)?;
    let declared_spans = cli_span_definitions_for_args(args)?;
    let mut baseline = TargetAggregateBuilder::new(
        component_id,
        scenario_id,
        "baseline",
        repeat,
        declared_spans.clone(),
    );
    let mut candidate = TargetAggregateBuilder::new(
        component_id,
        scenario_id,
        "candidate",
        repeat,
        declared_spans,
    );
    let mut proof_run_order = Vec::new();

    for entry in plan {
        let (target, builder) = if entry.group() == "baseline" {
            (baseline_target, &mut baseline)
        } else {
            (candidate_target, &mut candidate)
        };
        let run = execute_target_once(
            args,
            component_id,
            scenario_id,
            target,
            resolved_trace_secret_env.as_ref(),
        );
        let proof_entry = builder.record(entry.index(), run);
        proof_run_order.push(extension_trace::TraceCompareRunOrderOutput {
            index: proof_entry.index,
            group: entry.group().to_string(),
            iteration: entry.iteration(),
            status: proof_entry.status,
            exit_code: proof_entry.exit_code,
            artifact_path: proof_entry.artifact_path,
            failure: proof_entry.failure,
        });
    }

    Ok(TargetProofMatrix {
        baseline: baseline.finish(args, span_metadata.clone())?,
        candidate: candidate.finish(args, span_metadata)?,
        run_order: proof_run_order,
    })
}

fn execute_target_once(
    args: &TraceArgs,
    component_id: &str,
    scenario_id: &str,
    target: &ResolvedCompareTarget,
    resolved_trace_secret_env: Option<&ResolvedTraceSecretEnv>,
) -> Result<super::TraceRunExecution, homeboy::core::Error> {
    let mut run_args = args.clone();
    run_args.comp.component = Some(component_id.to_string());
    run_args.comp.path = Some(target.path.to_string_lossy().to_string());
    run_args.scenario = Some(scenario_id.to_string());
    run_args.compare_after = None;
    run_args.baseline_target = None;
    run_args.candidate = None;
    run_args.repeat = 1;
    run_args.aggregate = None;
    run_args.output_dir = None;
    run_args.checkout_provenance = target.checkout_provenance.clone();
    apply_resolved_trace_secret_env(&mut run_args, resolved_trace_secret_env);
    super::execute_trace_run(run_args)
}

struct RecordedProofRun {
    index: usize,
    status: String,
    exit_code: i32,
    artifact_path: Option<String>,
    failure: Option<String>,
}

struct TargetAggregateBuilder {
    command: &'static str,
    component: String,
    scenario_id: String,
    repeat: usize,
    group: String,
    runs: Vec<extension_trace::TraceAggregateRunOutput>,
    span_samples: BTreeMap<String, Vec<TraceAggregateSpanSample>>,
    metric_samples: BTreeMap<String, Vec<TraceAggregateMetricSample>>,
    span_failures: BTreeMap<String, usize>,
    all_span_ids: BTreeSet<String>,
    failure_count: usize,
    rig_state: Option<homeboy::core::rig::RigStateSnapshot>,
    overlays: Vec<extension_trace::TraceOverlay>,
}

impl TargetAggregateBuilder {
    fn new(
        component_id: &str,
        scenario_id: &str,
        group: &str,
        repeat: usize,
        declared_spans: Vec<extension_trace::TraceSpanDefinition>,
    ) -> Self {
        Self {
            command: "trace.aggregate.spans",
            component: component_id.to_string(),
            scenario_id: scenario_id.to_string(),
            repeat,
            group: group.to_string(),
            runs: Vec::new(),
            span_samples: BTreeMap::new(),
            metric_samples: BTreeMap::new(),
            span_failures: BTreeMap::new(),
            all_span_ids: declared_spans.into_iter().map(|span| span.id).collect(),
            failure_count: 0,
            rig_state: None,
            overlays: Vec::new(),
        }
    }

    fn record(
        &mut self,
        index: usize,
        run: Result<super::TraceRunExecution, homeboy::core::Error>,
    ) -> RecordedProofRun {
        match run {
            Ok(execution) => self.record_execution(index, execution),
            Err(error) => self.record_error(index, error),
        }
    }

    fn record_execution(
        &mut self,
        index: usize,
        execution: super::TraceRunExecution,
    ) -> RecordedProofRun {
        if self.rig_state.is_none() {
            self.rig_state = execution.rig_state.clone();
        }
        if self.overlays.is_empty() && !execution.workflow.overlays.is_empty() {
            self.overlays = execution.workflow.overlays.clone();
        }
        let passed = execution.workflow.exit_code == 0 && execution.workflow.status == "pass";
        if !passed {
            self.failure_count += 1;
        }
        let artifact_path = execution
            .run_dir
            .step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS)
            .to_string_lossy()
            .to_string();
        let mut seen_span_ids = BTreeSet::new();
        if let Some(results) = execution.workflow.results.as_ref() {
            for (metric, value) in &results.metrics {
                if let Some(value) = value.as_f64() {
                    self.metric_samples.entry(metric.clone()).or_default().push(
                        TraceAggregateMetricSample {
                            value,
                            run_index: index,
                            artifact_path: artifact_path.clone(),
                        },
                    );
                }
            }
            for span in &results.span_results {
                self.all_span_ids.insert(span.id.clone());
                seen_span_ids.insert(span.id.clone());
                if span.status == extension_trace::parsing::TraceSpanStatus::Ok {
                    if let Some(duration) = span.duration_ms {
                        self.span_samples.entry(span.id.clone()).or_default().push(
                            TraceAggregateSpanSample {
                                duration_ms: duration,
                                run_index: index,
                                artifact_path: artifact_path.clone(),
                            },
                        );
                        continue;
                    }
                }
                *self.span_failures.entry(span.id.clone()).or_default() += 1;
            }
            for span_id in self.all_span_ids.difference(&seen_span_ids) {
                *self.span_failures.entry(span_id.clone()).or_default() += 1;
            }
        } else {
            for span_id in &self.all_span_ids {
                *self.span_failures.entry(span_id.clone()).or_default() += 1;
            }
        }
        let failure = execution
            .workflow
            .failure
            .as_ref()
            .map(|failure| failure.stderr_excerpt.clone())
            .or_else(|| {
                execution
                    .workflow
                    .results
                    .as_ref()
                    .and_then(|results| results.failure.clone())
            });
        self.runs.push(extension_trace::TraceAggregateRunOutput {
            index,
            passed,
            status: execution.workflow.status.clone(),
            exit_code: execution.workflow.exit_code,
            artifact_path: artifact_path.clone(),
            scenario_id: execution
                .workflow
                .results
                .as_ref()
                .map(|results| results.scenario_id.clone()),
            summary: execution
                .workflow
                .results
                .as_ref()
                .and_then(|results| results.summary.clone()),
            failure: failure.clone(),
        });
        RecordedProofRun {
            index,
            status: execution.workflow.status,
            exit_code: execution.workflow.exit_code,
            artifact_path: Some(artifact_path),
            failure,
        }
    }

    fn record_error(&mut self, index: usize, error: homeboy::core::Error) -> RecordedProofRun {
        self.failure_count += 1;
        for span_id in &self.all_span_ids {
            *self.span_failures.entry(span_id.clone()).or_default() += 1;
        }
        let failure = error.message;
        self.runs.push(extension_trace::TraceAggregateRunOutput {
            index,
            passed: false,
            status: "error".to_string(),
            exit_code: 1,
            artifact_path: String::new(),
            scenario_id: Some(self.scenario_id.clone()),
            summary: None,
            failure: Some(failure.clone()),
        });
        RecordedProofRun {
            index,
            status: "error".to_string(),
            exit_code: 1,
            artifact_path: None,
            failure: Some(failure),
        }
    }

    fn finish(
        mut self,
        args: &TraceArgs,
        span_metadata: BTreeMap<String, extension_trace::TraceSpanMetadata>,
    ) -> homeboy::core::Result<extension_trace::TraceAggregateOutput> {
        let mut spans = self
            .all_span_ids
            .into_iter()
            .map(|id| {
                let samples = self.span_samples.remove(&id).unwrap_or_default();
                let failures = self.span_failures.remove(&id).unwrap_or(0);
                aggregate_span(id, samples, failures)
            })
            .collect::<Vec<_>>();
        let metrics = self
            .metric_samples
            .into_iter()
            .map(|(id, samples)| aggregate_metric(id, samples))
            .collect::<Vec<_>>();
        let unmatched_span_metadata_ids = attach_span_metadata(&mut spans, &span_metadata);
        let classification_summaries = classification_summaries(&spans);
        let guardrails = run_trace_guardrails_for_args(args)?;
        let guardrail_failure_count = guardrails
            .iter()
            .filter(|guardrail| !guardrail.passed)
            .count();
        let focus_spans = super::repeat::focus_aggregate_spans(&spans, &args.focus_spans);
        let exit_code = if self.failure_count == 0 && guardrail_failure_count == 0 {
            0
        } else {
            1
        };
        Ok(extension_trace::TraceAggregateOutput {
            command: self.command,
            passed: exit_code == 0,
            status: if exit_code == 0 { "pass" } else { "fail" }.to_string(),
            component: self.component,
            scenario_id: self.scenario_id,
            phase_preset: args.phase_preset.clone(),
            repeat: self.repeat,
            run_count: self.runs.len(),
            failure_count: self.failure_count,
            exit_code,
            schedule: Some(format!("{}:{}", args.schedule.as_str(), self.group)),
            run_order: self
                .runs
                .iter()
                .map(|run| extension_trace::TraceRunOrderEntryOutput {
                    index: run.index,
                    group: self.group.clone(),
                    iteration: run.index,
                })
                .collect(),
            rig_state: self.rig_state,
            overlays: self.overlays,
            runs: self.runs,
            spans,
            metrics,
            guardrails,
            guardrail_failure_count,
            focus_span_ids: args.focus_spans.clone(),
            focus_spans,
            classification_summaries,
            unmatched_span_metadata_ids,
            profile: super::resolved_profile_output_for_args(args),
        })
    }
}

fn trace_compare_caveats(args: &TraceArgs) -> Vec<String> {
    let mut caveats = Vec::new();
    caveats.push(format!(
        "Runs use `{}` scheduling with `{}` repetitions per side; raw run artifacts remain linked from aggregate `runs` and the A/B matrix.",
        args.schedule.as_str(),
        args.repeat.max(1)
    ));
    caveats.push("This report preserves throttling/profile labels emitted by browser evidence artifacts; synthetic or throttled timing labels should be interpreted as relative proof data, not absolute user timing.".to_string());
    caveats.push("Known lab plumbing issues remain tracked separately: https://github.com/Extra-Chill/homeboy/issues/3621 and https://github.com/Extra-Chill/homeboy/issues/3631.".to_string());
    caveats
}

fn browser_proof_for_runs(
    baseline_label: &str,
    candidate_label: &str,
    baseline: &extension_trace::TraceAggregateOutput,
    candidate: &extension_trace::TraceAggregateOutput,
) -> homeboy::core::Result<Option<extension_trace::TraceBrowserProofOutput>> {
    let baseline_dirs = run_artifact_dirs(baseline);
    let candidate_dirs = run_artifact_dirs(candidate);
    if baseline_dirs.is_empty() && candidate_dirs.is_empty() {
        return Ok(None);
    }
    let report = crate::commands::report::browser_evidence_compare_from_dirs(
        &baseline_dirs,
        &candidate_dirs,
        baseline_label,
        candidate_label,
        false,
    )?;
    if !has_promoted_browser_evidence(&report) {
        return Ok(None);
    }
    let markdown = report.markdown.clone();
    let report_json = serde_json::to_value(&report).map_err(|err| {
        homeboy::core::Error::internal_json(
            err.to_string(),
            Some("trace.compare.browser_proof.serialize".to_string()),
        )
    })?;
    Ok(Some(extension_trace::TraceBrowserProofOutput {
        baseline_dirs: baseline_dirs
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        candidate_dirs: candidate_dirs
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        markdown,
        report: report_json,
    }))
}

fn has_promoted_browser_evidence(
    report: &crate::commands::report::BrowserEvidenceCompareReport,
) -> bool {
    report.variants.iter().any(|variant| {
        !variant.browser_metrics.is_empty()
            || !variant.lifecycle_metrics.is_empty()
            || !variant.request_by_host.is_empty()
            || !variant.request_by_type.is_empty()
            || variant.request_totals.baseline.is_some()
            || variant.request_totals.candidate.is_some()
            || variant.console_errors.baseline.is_some()
            || variant.console_errors.candidate.is_some()
            || variant.page_errors.baseline.is_some()
            || variant.page_errors.candidate.is_some()
    })
}

fn run_artifact_dirs(aggregate: &extension_trace::TraceAggregateOutput) -> Vec<PathBuf> {
    aggregate
        .runs
        .iter()
        .filter_map(|run| {
            (!run.artifact_path.is_empty())
                .then(|| {
                    Path::new(&run.artifact_path)
                        .parent()
                        .map(Path::to_path_buf)
                })
                .flatten()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

struct ResolvedCompareTarget {
    input: String,
    path: PathBuf,
    git_sha: Option<String>,
    checkout_provenance: Option<TraceCheckoutProvenance>,
    _worktree: Option<TemporaryGitWorktree>,
}

fn resolve_target(
    role: &'static str,
    input: &str,
    source_path: &str,
) -> homeboy::core::Result<ResolvedCompareTarget> {
    let input_path = PathBuf::from(input);
    if input_path.exists() {
        let path = input_path.canonicalize().map_err(|err| {
            homeboy::core::Error::internal_io(
                format!("Failed to resolve {} path {}: {}", role, input, err),
                Some("trace.compare.path".to_string()),
            )
        })?;
        let git_sha = git::short_head_revision_at(&path);
        return Ok(ResolvedCompareTarget {
            input: input.to_string(),
            path,
            git_sha,
            checkout_provenance: None,
            _worktree: None,
        });
    }

    let source_root = git::get_git_root(source_path)?;
    let source_root = PathBuf::from(source_root);
    let component_prefix = git::get_component_path_prefix(source_path);
    let resolved_sha = resolve_git_ref_to_full_sha(&source_root, input)?;
    let worktree = TemporaryGitWorktree::add(role, &source_root, &resolved_sha)?;
    let path = component_prefix
        .as_deref()
        .map(|prefix| worktree.path.join(prefix))
        .unwrap_or_else(|| worktree.path.clone());
    let git_sha = git::short_head_revision_at(&path);
    let checkout_provenance = Some(TraceCheckoutProvenance {
        source: "homeboy-trace-compare".to_string(),
        path: path.to_string_lossy().to_string(),
        requested_ref: input.to_string(),
        resolved_sha,
    });
    Ok(ResolvedCompareTarget {
        input: input.to_string(),
        path,
        git_sha,
        checkout_provenance,
        _worktree: Some(worktree),
    })
}

fn resolve_git_ref_to_full_sha(source_root: &Path, git_ref: &str) -> homeboy::core::Result<String> {
    let commit_ref = format!("{git_ref}^{{commit}}");
    git::run_git(
        source_root,
        &["rev-parse", "--verify", &commit_ref],
        "git rev-parse trace compare target",
    )
    .map(|sha| sha.trim().to_string())
}

struct TemporaryGitWorktree {
    source_root: PathBuf,
    path: PathBuf,
}

impl TemporaryGitWorktree {
    fn add(role: &str, source_root: &Path, git_ref: &str) -> homeboy::core::Result<Self> {
        let parent = std::env::temp_dir().join("homeboy-trace-compare");
        std::fs::create_dir_all(&parent).map_err(|err| {
            homeboy::core::Error::internal_io(
                format!(
                    "Failed to create trace compare temp dir {}: {}",
                    parent.display(),
                    err
                ),
                Some("trace.compare.temp".to_string()),
            )
        })?;
        let path = parent.join(format!("{}-{}", role, uuid::Uuid::new_v4()));
        let path_arg = path.to_string_lossy().to_string();
        git::run_git(
            source_root,
            &["worktree", "add", "--detach", &path_arg, git_ref],
            "git worktree add trace compare target",
        )?;
        Ok(Self {
            source_root: source_root.to_path_buf(),
            path,
        })
    }
}

impl Drop for TemporaryGitWorktree {
    fn drop(&mut self) {
        let path = self.path.to_string_lossy().to_string();
        let _ = git::run_git(
            &self.source_root,
            &["worktree", "remove", "--force", &path],
            "git worktree remove trace compare target",
        );
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{BaselineArgs, SettingArgs};
    use homeboy::core::component::ScopedExtensionConfig;

    fn compare_args_for_rig(rig_id: &str, component_id: Option<&str>) -> TraceArgs {
        TraceArgs {
            comp: PositionalComponentArgs {
                component: Some("compare".to_string()),
                path: None,
            },
            component_arg: component_id.map(str::to_string),
            scenario: component_id.map(str::to_string),
            scenario_arg: Some("scenario".to_string()),
            compare_after: None,
            baseline_target: Some("origin/main".to_string()),
            candidate: Some("HEAD".to_string()),
            rig: Some(rig_id.to_string()),
            profile: None,
            profiles: false,
            setting_args: SettingArgs::default(),
            secret_env: Vec::new(),
            json_summary: false,
            report: None,
            experiment: None,
            repeat: 1,
            aggregate: None,
            schedule: super::super::TraceSchedule::Grouped,
            focus_spans: Vec::new(),
            metric_guardrails: Vec::new(),
            spans: Vec::new(),
            phases: Vec::new(),
            attachments: Vec::new(),
            phase_preset: None,
            baseline_args: BaselineArgs::default(),
            regression_threshold: extension_trace::baseline::DEFAULT_REGRESSION_THRESHOLD_PERCENT,
            regression_min_delta_ms: extension_trace::baseline::DEFAULT_REGRESSION_MIN_DELTA_MS,
            overlays: Vec::new(),
            variants: Vec::new(),
            matrix: super::super::TraceVariantMatrixMode::None,
            axes: Vec::new(),
            matrix_env: Vec::new(),
            output_dir: None,
            keep_overlay: false,
            canonical: false,
            allow_local_toolchain: true,
            stale: false,
            force: false,
            checkout_provenance: None,
        }
    }

    #[test]
    fn compare_target_component_uses_rig_declared_component() {
        crate::test_support::with_isolated_home(|_| {
            let component_dir = tempfile::TempDir::new().expect("component dir");
            let rig_dir = homeboy::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("trace-lab.json"),
                serde_json::json!({
                    "id": "trace-lab",
                    "components": {
                        "lab-component": {
                            "path": component_dir.path().display().to_string(),
                            "remote_url": "https://github.com/example/lab-component.git",
                            "extensions": {
                                "trace-extension": { "setting": "from-rig" }
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("write rig");

            let (component_id, component) =
                compare_target_component(&compare_args_for_rig("trace-lab", Some("lab-component")))
                    .expect("rig component resolves");

            assert_eq!(component_id, "lab-component");
            assert_eq!(component.id, "lab-component");
            assert_eq!(component.local_path, component_dir.path().to_string_lossy());
            assert_eq!(
                component.remote_url.as_deref(),
                Some("https://github.com/example/lab-component.git")
            );
            assert_eq!(
                component
                    .extensions
                    .as_ref()
                    .and_then(|extensions| extensions.get("trace-extension"))
                    .and_then(|config: &ScopedExtensionConfig| config.settings.get("setting"))
                    .and_then(serde_json::Value::as_str),
                Some("from-rig")
            );
        });
    }

    #[test]
    fn compare_target_component_uses_single_rig_component_by_default() {
        crate::test_support::with_isolated_home(|_| {
            let component_dir = tempfile::TempDir::new().expect("component dir");
            let rig_dir = homeboy::core::paths::rigs().expect("rig dir");
            std::fs::create_dir_all(&rig_dir).expect("create rig dir");
            std::fs::write(
                rig_dir.join("single-trace-lab.json"),
                serde_json::json!({
                    "id": "single-trace-lab",
                    "components": {
                        "only-component": { "path": component_dir.path().display().to_string() }
                    }
                })
                .to_string(),
            )
            .expect("write rig");

            let (component_id, component) =
                compare_target_component(&compare_args_for_rig("single-trace-lab", None))
                    .expect("single rig component resolves");

            assert_eq!(component_id, "only-component");
            assert_eq!(component.local_path, component_dir.path().to_string_lossy());
        });
    }
}
