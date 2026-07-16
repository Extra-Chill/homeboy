use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use homeboy::core::component;
use homeboy::core::extension::trace as extension_trace;
use homeboy::core::extension::trace::{TraceCheckoutProvenance, TraceCommandOutput};
use homeboy::core::git;
use homeboy::core::observation::{NewRunRecord, RunStatus};
use homeboy::core::trace_compare::{self, CompareArtifactSet, CompareObservation};

use super::aggregate::{
    aggregate_metric, aggregate_span, TraceAggregateMetricSample, TraceAggregateSpanSample,
};
use super::matrix::aggregate_to_compare_input;
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
    let _compare_lease = if let Some(rig_id) = args.rig.as_deref() {
        load_rig_context(Some(rig_id))?
            .map(|context| {
                homeboy::core::rig::lease::acquire_active_run_lease(
                    &context.rig_spec,
                    "trace compare",
                )
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
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
    trace_compare::prepare_output_dir(&output_dir)?;

    let observation = start_compare_observation(&args, &component_id, &scenario_id, &output_dir);

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
    let baseline_run_ids = proof.baseline_run_ids;
    let candidate_run_ids = proof.candidate_run_ids;

    let baseline_path = output_dir.join("baseline.aggregate.json");
    let candidate_path = output_dir.join("candidate.aggregate.json");
    let summary_path = output_dir.join("summary.md");

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
        &output_dir,
        &args,
    )?;

    let summary_markdown = super::output::render_compare_markdown(&compare);
    let compare_artifact_set = CompareArtifactSet {
        baseline_aggregate: &baseline_aggregate,
        candidate_aggregate: &candidate_aggregate,
        compare: &compare,
        summary_markdown: &summary_markdown,
    };
    // Core delegation: artifact persistence lives in trace_compare (core service).
    let artifact_paths =
        trace_compare::persist_compare_artifacts(&output_dir, compare_artifact_set)?; // homeboy-audit: allow-thin-command-adapter

    let failed = !baseline_aggregate.passed
        || !candidate_aggregate.passed
        || compare.focus_status.as_deref() == Some("fail")
        || compare.guardrail_status.as_deref() == Some("fail")
        || compare.metric_guardrail_status.as_deref() == Some("fail");
    let compare_status = if failed { "fail" } else { "pass" };

    let pair_artifact = build_compare_pair_artifact(
        &component_id,
        &scenario_id,
        compare_status,
        &output_dir,
        &artifact_paths,
        &compare,
        ComparePairSideInputs {
            target: Some(baseline_target.input.clone()),
            git_sha: compare.before_git_sha.clone(),
            status: compare.before_status.clone(),
            run_ids: baseline_run_ids,
            aggregate: &baseline_aggregate,
        },
        ComparePairSideInputs {
            target: Some(candidate_target.input.clone()),
            git_sha: compare.after_git_sha.clone(),
            status: compare.after_status.clone(),
            run_ids: candidate_run_ids,
            aggregate: &candidate_aggregate,
        },
    );
    trace_compare::persist_compare_pair_artifact(&artifact_paths.pair, &pair_artifact)?; // homeboy-audit: allow-thin-command-adapter
    let pair_json = serde_json::to_value(&pair_artifact).map_err(|err| {
        homeboy::core::Error::internal_json(
            err.to_string(),
            Some("trace.compare.pair.serialize".to_string()),
        )
    })?;

    finish_compare_observation(
        observation,
        if failed {
            RunStatus::Fail
        } else {
            RunStatus::Pass
        },
        &artifact_paths,
        serde_json::json!({
            "scenario_id": scenario_id,
            "trace_mode": "compare_targets",
            "compare": {
                "baseline_target": baseline_target.input,
                "candidate_target": candidate_target.input,
                "baseline_git_sha": compare.before_git_sha.as_deref(),
                "candidate_git_sha": compare.after_git_sha.as_deref(),
                "baseline_status": compare.before_status.as_deref(),
                "candidate_status": compare.after_status.as_deref(),
                "output_dir": compare.output_dir.as_deref(),
                "summary_path": compare.summary_path.as_deref(),
                "span_count": compare.span_count,
                "focus_status": compare.focus_status.as_deref(),
                "guardrail_status": compare.guardrail_status.as_deref(),
                "metric_guardrail_status": compare.metric_guardrail_status.as_deref(),
            },
            "compare_pair": pair_json,
        }),
    );
    Ok((
        TraceCommandOutput::Compare(compare),
        if failed { 1 } else { 0 },
    ))
}

/// Inputs for one side of the compare pair artifact, assembled from the proof
/// matrix output and the resolved target.
struct ComparePairSideInputs<'a> {
    target: Option<String>,
    git_sha: Option<String>,
    status: Option<String>,
    run_ids: Vec<String>,
    aggregate: &'a extension_trace::TraceAggregateOutput,
}

/// Assemble the first-class, provider-agnostic compare pair artifact linking the
/// compare command, child baseline/candidate run ids, the report path, and the
/// persisted artifact bundle directories.
#[allow(clippy::too_many_arguments)]
fn build_compare_pair_artifact(
    component_id: &str,
    scenario_id: &str,
    status: &str,
    output_dir: &Path,
    artifact_paths: &trace_compare::CompareArtifactPaths,
    compare: &extension_trace::TraceCompareOutput,
    baseline: ComparePairSideInputs<'_>,
    candidate: ComparePairSideInputs<'_>,
) -> trace_compare::ComparePairArtifact {
    let post_compare_artifacts = compare
        .browser_proof
        .as_ref()
        .map(|proof| {
            proof
                .baseline_dirs
                .iter()
                .chain(proof.candidate_dirs.iter())
                .map(|dir| trace_compare::ComparePairLinkedArtifact {
                    kind: "browser-proof-dir".to_string(),
                    path: dir.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    trace_compare::ComparePairArtifact {
        kind: "trace-compare-pair",
        command: "trace.compare".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        component: component_id.to_string(),
        scenario_id: scenario_id.to_string(),
        status: status.to_string(),
        baseline: compare_pair_side(baseline),
        candidate: compare_pair_side(candidate),
        output_dir: output_dir.to_string_lossy().to_string(),
        compare_json: artifact_paths.compare.to_string_lossy().to_string(),
        summary_path: artifact_paths.summary.to_string_lossy().to_string(),
        post_compare_artifacts,
    }
}

fn compare_pair_side(inputs: ComparePairSideInputs<'_>) -> trace_compare::ComparePairSide {
    let artifact_dirs = run_artifact_dirs(inputs.aggregate)
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    trace_compare::ComparePairSide::new(
        inputs.target,
        inputs.git_sha,
        inputs.status,
        inputs.run_ids,
        artifact_dirs,
    )
}

fn start_compare_observation(
    args: &TraceArgs,
    component_id: &str,
    scenario_id: &str,
    output_dir: &Path,
) -> Option<CompareObservation> {
    let cwd = std::env::current_dir().ok();
    CompareObservation::start(
        NewRunRecord::builder("trace")
            .component_id(component_id.to_string())
            .command(std::env::args().collect::<Vec<_>>().join(" "))
            .optional_cwd_path(cwd.as_deref())
            .current_homeboy_version()
            .optional_rig_id(args.rig.clone())
            .metadata(serde_json::json!({
                "scenario_id": scenario_id,
                "trace_mode": "compare_targets",
                "baseline_target": args.baseline_target.as_deref(),
                "candidate_target": args.candidate.as_deref(),
                "output_dir": output_dir.display().to_string(),
            }))
            .build(),
    )
}

fn finish_compare_observation(
    observation: Option<CompareObservation>,
    status: RunStatus,
    paths: &homeboy::core::trace_compare::CompareArtifactPaths,
    metadata: serde_json::Value,
) {
    let Some(observation) = observation else {
        return;
    };
    observation.finish(status, paths, metadata);
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
    baseline_run_ids: Vec<String>,
    candidate_run_ids: Vec<String>,
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

    let baseline_run_ids = baseline.run_ids().to_vec();
    let candidate_run_ids = candidate.run_ids().to_vec();
    Ok(TargetProofMatrix {
        baseline: baseline.finish(args, span_metadata.clone())?,
        candidate: candidate.finish(args, span_metadata)?,
        run_order: proof_run_order,
        baseline_run_ids,
        candidate_run_ids,
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
    super::execute_trace_run(run_args) // homeboy-audit: allow-thin-command-adapter
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
    run_ids: Vec<String>,
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
            run_ids: Vec::new(),
        }
    }

    /// Observation run ids of the child runs recorded for this group, in the
    /// order they executed. Used to link child baseline/candidate run records
    /// into the first-class compare pair artifact.
    fn run_ids(&self) -> &[String] {
        &self.run_ids
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
        if let Some(run_id) = execution.run_id.as_ref() {
            self.run_ids.push(run_id.clone());
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
    output_dir: &Path,
    args: &TraceArgs,
) -> homeboy::core::Result<Option<extension_trace::TraceBrowserProofOutput>> {
    let baseline_dirs = run_artifact_dirs(baseline);
    let candidate_dirs = run_artifact_dirs(candidate);
    if baseline_dirs.is_empty() && candidate_dirs.is_empty() {
        return Ok(None);
    }
    let visual_options = visual_compare_options(args, output_dir)?;
    let report = crate::commands::report::browser_evidence_compare_from_dirs_with_visual(
        &baseline_dirs,
        &candidate_dirs,
        baseline_label,
        candidate_label,
        false,
        visual_options,
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
        baseline_runs: browser_proof_run_refs(baseline),
        candidate_runs: browser_proof_run_refs(candidate),
        markdown,
        report: report_json,
    }))
}

fn browser_proof_run_refs(
    aggregate: &extension_trace::TraceAggregateOutput,
) -> Vec<extension_trace::TraceBrowserProofRunRefOutput> {
    aggregate
        .runs
        .iter()
        .filter(|run| !run.artifact_path.is_empty())
        .map(|run| extension_trace::TraceBrowserProofRunRefOutput {
            index: run.index,
            status: run.status.clone(),
            exit_code: run.exit_code,
            artifact_path: run.artifact_path.clone(),
            artifact_dir: Path::new(&run.artifact_path)
                .parent()
                .map(|path| path.to_string_lossy().to_string()),
        })
        .collect()
}

fn visual_compare_options(
    args: &TraceArgs,
    output_dir: &Path,
) -> homeboy::core::Result<Option<crate::commands::report::VisualCompareOptions>> {
    if !args.visual_compare {
        return Ok(None);
    }
    let Some(provider_command) = args.visual_compare_provider.clone() else {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--visual-compare-provider".to_string(),
        ]));
    };
    Ok(Some(crate::commands::report::VisualCompareOptions {
        artifacts_dir: args
            .visual_artifacts_dir
            .clone()
            .unwrap_or_else(|| output_dir.join("visual-compare")),
        provider_command,
        provider_args: args.visual_provider_args.clone(),
        threshold: args.visual_threshold,
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
        std::fs::create_dir_all(&parent) // homeboy-audit: allow-thin-command-adapter
            .map_err(|err| {
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
        let _ = std::fs::remove_dir_all(&self.path); // homeboy-audit: allow-thin-command-adapter
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_fixture::{write_trace_extension, write_trace_rig};
    use super::*;
    use crate::commands::utils::args::{BaselineArgs, SettingArgs};
    use homeboy::core::component::ScopedExtensionConfig;
    use homeboy::core::observation::ObservationStore;

    fn set_trace_rig_resources(rig_id: &str, resources: serde_json::Value) {
        let rig_path = homeboy::core::paths::rigs()
            .expect("rig dir")
            .join(format!("{rig_id}.json"));
        let mut rig_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&rig_path).expect("read rig"))
                .expect("parse rig");
        rig_json["resources"] = resources;
        std::fs::write(
            rig_path,
            serde_json::to_string(&rig_json).expect("serialize rig"),
        )
        .expect("write rig");
    }

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
            visual_compare: false,
            visual_artifacts_dir: None,
            visual_compare_provider: None,
            visual_provider_args: Vec::new(),
            visual_threshold: None,
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

    #[test]
    fn compare_targets_with_same_resourceful_rig_runs_interleaved_children() {
        crate::test_support::with_isolated_home(|home| {
            write_trace_extension(home);
            let component_dir = tempfile::TempDir::new().expect("component dir");
            let baseline_dir = tempfile::TempDir::new().expect("baseline dir");
            let candidate_dir = tempfile::TempDir::new().expect("candidate dir");
            let output_dir = tempfile::TempDir::new().expect("output dir");
            write_trace_rig(home, "studio-rig", "studio", component_dir.path());
            set_trace_rig_resources(
                "studio-rig",
                serde_json::json!({ "exclusive": ["studio-runtime"] }),
            );

            let mut args = compare_args_for_rig("studio-rig", Some("studio"));
            args.scenario_arg = Some("studio-app-create-site".to_string());
            args.baseline_target = Some(baseline_dir.path().to_string_lossy().to_string());
            args.candidate = Some(candidate_dir.path().to_string_lossy().to_string());
            args.repeat = 2;
            args.schedule = super::super::TraceSchedule::Interleaved;
            args.output_dir = Some(output_dir.path().to_path_buf());

            let (output, exit_code) = run_compare_targets(args).expect("compare target run");

            assert_eq!(exit_code, 0);
            let TraceCommandOutput::Compare(compare) = output else {
                panic!("expected compare output");
            };
            assert_eq!(compare.proof_run_order.len(), 4);
            assert_eq!(compare.proof_run_order[0].group, "baseline");
            assert_eq!(compare.proof_run_order[1].group, "candidate");
            assert_eq!(compare.proof_run_order[2].group, "baseline");
            assert_eq!(compare.proof_run_order[3].group, "candidate");
            assert!(homeboy::core::rig::lease::active_run_leases()
                .expect("active leases")
                .is_empty());
            let store = ObservationStore::open_initialized().expect("store");
            let runs = store
                .list_runs(homeboy::core::observation::RunListFilter {
                    kind: Some("trace".to_string()),
                    ..Default::default()
                })
                .expect("runs");
            let compare_run = runs
                .iter()
                .find(|run| run.metadata_json["trace_mode"] == "compare_targets")
                .expect("compare run persisted");
            assert_eq!(compare_run.status, "pass");
            assert_eq!(compare_run.component_id.as_deref(), Some("studio"));
            assert_eq!(compare_run.rig_id.as_deref(), Some("studio-rig"));
            assert_eq!(
                compare_run.metadata_json["compare"]["baseline_target"],
                baseline_dir.path().to_string_lossy().as_ref()
            );
            assert_eq!(
                compare_run.metadata_json["compare"]["candidate_target"],
                candidate_dir.path().to_string_lossy().as_ref()
            );
            let artifacts = store.list_artifacts(&compare_run.id).expect("artifacts");
            let artifact_kinds: std::collections::BTreeSet<_> = artifacts
                .iter()
                .map(|artifact| artifact.kind.as_str())
                .collect();
            assert!(artifact_kinds.contains("trace-compare-baseline-aggregate"));
            assert!(artifact_kinds.contains("trace-compare-candidate-aggregate"));
            assert!(artifact_kinds.contains("trace-compare-json"));
            assert!(artifact_kinds.contains("trace-compare-summary"));
            assert!(
                artifact_kinds.contains("trace-compare-pair"),
                "compare pair artifact recorded as first-class evidence"
            );

            // The pair artifact is the canonical evidence index: it must link
            // the child baseline/candidate observation run ids and be addressable
            // from the compare run metadata so reporting never spelunks temp paths.
            let pair_artifact = artifacts
                .iter()
                .find(|artifact| artifact.kind == "trace-compare-pair")
                .expect("pair artifact present");
            let pair_path = std::path::Path::new(&pair_artifact.path);
            assert!(pair_path.exists(), "pair artifact persisted to disk");
            let pair: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(pair_path).expect("read pair"))
                    .expect("parse pair");
            assert_eq!(pair["kind"], "trace-compare-pair");
            assert_eq!(pair["command"], "trace.compare");
            assert_eq!(pair["component"], "studio");
            assert_eq!(pair["status"], "pass");
            let baseline_run_ids = pair["baseline"]["run_ids"]
                .as_array()
                .expect("baseline run ids array");
            let candidate_run_ids = pair["candidate"]["run_ids"]
                .as_array()
                .expect("candidate run ids array");
            assert!(
                !baseline_run_ids.is_empty(),
                "at least one baseline child run id linked"
            );
            assert!(
                !candidate_run_ids.is_empty(),
                "at least one candidate child run id linked"
            );
            // Linked child run ids must resolve to real persisted run records.
            for run_id in baseline_run_ids.iter().chain(candidate_run_ids.iter()) {
                let run_id = run_id.as_str().expect("run id string");
                assert!(
                    store.get_run(run_id).expect("lookup child run").is_some(),
                    "child run {run_id} persisted in observation store"
                );
            }
            assert_eq!(
                pair["baseline"]["run_evidence"][0]["evidence_command"],
                format!(
                    "homeboy runs evidence {}",
                    baseline_run_ids[0].as_str().unwrap()
                )
            );

            // The compare run metadata embeds the same pair index so consumers can
            // address compare-level evidence directly from the run record.
            assert_eq!(
                compare_run.metadata_json["compare_pair"]["kind"],
                "trace-compare-pair"
            );
            assert_eq!(
                compare_run.metadata_json["compare_pair"]["baseline"]["run_ids"],
                pair["baseline"]["run_ids"]
            );
        });
    }

    #[test]
    fn browser_proof_run_refs_include_child_artifact_addresses() {
        let aggregate = extension_trace::TraceAggregateOutput {
            command: "trace.aggregate",
            passed: true,
            status: "pass".to_string(),
            component: "woo-stripe".to_string(),
            scenario_id: "ece-visual".to_string(),
            phase_preset: None,
            repeat: 1,
            run_count: 1,
            failure_count: 0,
            exit_code: 0,
            schedule: None,
            run_order: Vec::new(),
            rig_state: None,
            overlays: Vec::new(),
            runs: vec![extension_trace::TraceAggregateRunOutput {
                index: 1,
                passed: true,
                status: "pass".to_string(),
                exit_code: 0,
                artifact_path: "/tmp/homeboy/run-1/trace.json".to_string(),
                scenario_id: Some("ece-visual".to_string()),
                summary: None,
                failure: None,
            }],
            spans: Vec::new(),
            metrics: Vec::new(),
            guardrails: Vec::new(),
            guardrail_failure_count: 0,
            focus_span_ids: Vec::new(),
            focus_spans: Vec::new(),
            classification_summaries: Vec::new(),
            unmatched_span_metadata_ids: Vec::new(),
            profile: None,
        };

        let refs = browser_proof_run_refs(&aggregate);

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].index, 1);
        assert_eq!(refs[0].status, "pass");
        assert_eq!(refs[0].artifact_path, "/tmp/homeboy/run-1/trace.json");
        assert_eq!(refs[0].artifact_dir.as_deref(), Some("/tmp/homeboy/run-1"));
    }
}
