use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use homeboy::core::ci_profile::{self, CiResolvedJob};
use homeboy::core::component::{Component, ScopedExtensionConfig};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::invocation::InvocationRequirements;
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::bench as extension_bench;
use homeboy::core::extension::bench::report::collect_artifacts;
use homeboy::core::extension::bench::{
    BenchCommandOutput, BenchGate, BenchResults, BenchRunExecution, BenchRunWorkflowArgs,
    BenchRunWorkflowResult,
};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::rig::lease::ActiveRigRunLease;
use homeboy::core::rig::{self, BenchPrepareReport, BenchSpec, RigSpec, RigStateSnapshot};

use super::observation::{self, BenchObservationStart};
use super::{BenchRunArgs, CmdResult};

struct RigBenchContext {
    id: String,
    source: rig::RigSourceContext,
    snapshot: RigStateSnapshot,
    _lease: Option<ActiveRigRunLease>,
}

impl RigBenchContext {
    fn spec(&self) -> &RigSpec {
        &self.source.spec
    }

    fn package_root(&self) -> Option<&std::path::Path> {
        self.source.package_root.as_deref()
    }
}

fn prepare_rig_bench_context(
    rig_id: &str,
    args: &BenchRunArgs,
) -> homeboy::core::Result<RigBenchContext> {
    let mut source = rig::RigSourceContext::load_for_invocation(rig_id)?;
    report_rig_source(&source);
    let mut spec = source.spec.clone();
    let declared_spec = spec.clone();
    apply_bench_path_override(&mut spec, args);
    source.spec = spec.clone();
    let lease = rig::lease::acquire_active_run_lease(&spec, "bench")?;
    let prepare_settings = bench_prepare_settings(args);
    if let Some(prepare) = rig::run_bench_prepare(&spec, &prepare_settings)? {
        if !prepare.success {
            return Err(homeboy::core::Error::rig_pipeline_failed(
                &spec.id,
                "bench_prepare",
                bench_prepare_failure_message(&prepare),
            ));
        }
    }
    let snapshot = rig::snapshot_state(&declared_spec);
    let id = spec.id.clone();
    Ok(RigBenchContext {
        id,
        source,
        snapshot,
        _lease: lease,
    })
}

fn report_rig_source(source: &rig::RigSourceContext) {
    if let Some(evidence) = rig::package_evidence(&source.spec.id) {
        eprintln!(
            "bench rig source: rig={} package_root={} freshness={:?}",
            evidence.rig_id, evidence.package_root, evidence.freshness
        );
    }
}

fn bench_prepare_failure_message(prepare: &BenchPrepareReport) -> String {
    let failed_steps = prepare
        .pipeline
        .steps
        .iter()
        .filter(|step| step.status == "fail")
        .map(|step| match step.error.as_deref() {
            Some(error) if !error.is_empty() => {
                format!("{} `{}` failed: {}", step.kind, step.label, error)
            }
            _ => format!("{} `{}` failed", step.kind, step.label),
        })
        .collect::<Vec<_>>();

    if failed_steps.is_empty() {
        "rig bench preparation failed; refusing to run bench workload".to_string()
    } else {
        format!(
            "rig bench preparation failed; refusing to run bench workload. Failed bench_prepare steps: {}",
            failed_steps.join("; ")
        )
    }
}

fn bench_prepare_settings(args: &BenchRunArgs) -> Vec<(String, String)> {
    args.setting_args
        .setting
        .iter()
        .cloned()
        .chain(
            args.setting_args
                .setting_json
                .iter()
                .map(|(key, value)| (key.clone(), value.to_string())),
        )
        .collect()
}

fn apply_bench_path_override(spec: &mut RigSpec, args: &BenchRunArgs) {
    let Some(path) = args.comp.path.as_ref() else {
        return;
    };
    let component_id = args.comp.id().map(str::to_string).or_else(|| {
        spec.bench
            .as_ref()
            .and_then(|bench| bench_component_ids(bench).into_iter().next())
    });
    let Some(component_id) = component_id else {
        return;
    };
    if let Some(component) = spec.components.get_mut(&component_id) {
        component.path = path.clone();
    }
}

pub(super) fn bench_component_ids(bench: &BenchSpec) -> Vec<String> {
    if !bench.components.is_empty() {
        return bench.components.clone();
    }
    bench.default_component.iter().cloned().collect()
}

fn rig_bench_components(spec: &RigSpec) -> Vec<String> {
    spec.bench
        .as_ref()
        .map(bench_component_ids)
        .unwrap_or_default()
}

pub(super) fn validate_profile_available_for_rigs(
    rig_ids: &[String],
    profile: &str,
) -> homeboy::core::Result<()> {
    let mut missing = Vec::new();
    let mut available_by_rig = Vec::new();

    for rig_id in rig_ids {
        let spec = rig::load(rig_id)?;
        if !spec.bench_profiles.contains_key(profile) {
            missing.push(rig_id.clone());
        }
        available_by_rig.push((spec.id.clone(), available_profile_names(&spec)));
    }

    if missing.is_empty() {
        return Ok(());
    }

    let available = available_by_rig
        .into_iter()
        .map(|(rig_id, profiles)| format!("{}: {}", rig_id, format_available_profiles(&profiles)))
        .collect::<Vec<_>>()
        .join("; ");

    Err(homeboy::core::Error::validation_invalid_argument(
        "profile",
        format!(
            "bench profile '{}' is not defined by rig(s): {}; available profiles: {}",
            profile,
            missing.join(", "),
            available
        ),
        Some(profile.to_string()),
        None,
    ))
}

fn available_profile_names(spec: &RigSpec) -> Vec<String> {
    let mut profiles: Vec<String> = spec.bench_profiles.keys().cloned().collect();
    profiles.sort();
    profiles
}

fn format_available_profiles(profiles: &[String]) -> String {
    if profiles.is_empty() {
        "<none>".to_string()
    } else {
        profiles.join(", ")
    }
}

fn selected_scenario_ids(
    args: &BenchRunArgs,
    rig_spec: Option<&RigSpec>,
) -> homeboy::core::Result<Vec<String>> {
    let Some(profile) = &args.profile else {
        return Ok(args.scenario_ids.clone());
    };

    let Some(spec) = rig_spec else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "profile",
            "--profile requires --rig because profiles are declared in rig specs",
            Some(profile.clone()),
            None,
        ));
    };

    let Some(scenario_ids) = spec.bench_profiles.get(profile) else {
        let available = available_profile_names(spec);
        return Err(homeboy::core::Error::validation_invalid_argument(
            "profile",
            format!(
                "unknown bench profile '{}' for rig '{}'; available profiles: {}",
                profile,
                spec.id,
                format_available_profiles(&available)
            ),
            Some(profile.clone()),
            Some(available),
        ));
    };

    Ok(scenario_ids.clone())
}

pub(super) fn rig_component_path(spec: &RigSpec, component_id: &str) -> Option<String> {
    rig::resolve_component_path(spec, component_id).ok()
}

pub(super) fn rig_component_for_bench(spec: &RigSpec, component_id: &str) -> Option<Component> {
    let rig_component = spec.components.get(component_id)?;
    let mut extensions = rig_component.extensions.clone()?;
    expand_rig_extension_settings(spec, &mut extensions);
    let mut component = rig::resolve_component(spec, component_id).ok()?;
    component.remote_url = rig_component.remote_url.clone().or(component.remote_url);
    component.extensions = Some(extensions);
    component.resolve_remote_path();
    Some(component)
}

fn expand_rig_extension_settings(
    spec: &RigSpec,
    extensions: &mut HashMap<String, ScopedExtensionConfig>,
) {
    for extension in extensions.values_mut() {
        for value in extension.settings.values_mut() {
            expand_rig_setting_value(spec, value);
        }
    }
}

fn expand_rig_setting_value(spec: &RigSpec, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(raw) => {
            *raw = rig::expand::expand_vars(spec, raw);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                expand_rig_setting_value(spec, value);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                expand_rig_setting_value(spec, value);
            }
        }
        _ => {}
    }
}

fn component_shared_state(
    args: &BenchRunArgs,
    component_id: &str,
    matrix_len: usize,
) -> Option<PathBuf> {
    args.shared_state.as_ref().map(|path| {
        if matrix_len > 1 {
            path.join(component_id)
        } else {
            path.clone()
        }
    })
}

fn effective_warmup_iterations(args: &BenchRunArgs, rig_spec: Option<&RigSpec>) -> Option<u64> {
    args.warmup.or_else(|| {
        rig_spec
            .and_then(|spec| spec.bench.as_ref())
            .and_then(|bench| bench.warmup_iterations)
    })
}

fn declared_bench_gates(rig_spec: Option<&RigSpec>) -> DeclaredBenchGates {
    rig_spec
        .and_then(|spec| spec.bench.as_ref())
        .map(|bench| {
            let scenario_gates = bench
                .metric_gates
                .iter()
                .filter_map(|(scenario_id, metric_gates)| {
                    let gates: Vec<BenchGate> = metric_gates
                        .iter()
                        .flat_map(|(metric, condition)| condition.to_gates(metric))
                        .collect();
                    (!gates.is_empty()).then(|| (scenario_id.clone(), gates))
                })
                .collect();
            let result_gates = bench
                .result_gates
                .iter()
                .flat_map(|(metric, condition)| condition.to_gates(metric))
                .collect();
            DeclaredBenchGates {
                scenario_gates,
                result_gates,
            }
        })
        .unwrap_or_default()
}

#[derive(Default)]
struct DeclaredBenchGates {
    scenario_gates: BTreeMap<String, Vec<BenchGate>>,
    result_gates: Vec<BenchGate>,
}

fn apply_declared_bench_gates(workflow: &mut BenchRunWorkflowResult, gates: DeclaredBenchGates) {
    if gates.scenario_gates.is_empty() && gates.result_gates.is_empty() {
        return;
    }

    let Some(results) = workflow.results.as_mut() else {
        return;
    };

    for scenario in &mut results.scenarios {
        if let Some(gates) = gates.scenario_gates.get(&scenario.id) {
            scenario.gates.extend(gates.clone());
        }
        scenario.gates.extend(gates.result_gates.clone());
    }

    let failures = extension_bench::evaluate_gates(results);
    workflow.gate_results = extension_bench::normalized_gate_results(results);
    if failures.is_empty() {
        return;
    }

    workflow.gate_failures.extend(failures.iter().cloned());
    let hints = workflow.hints.get_or_insert_with(Vec::new);
    hints.extend(failures);
    workflow.status = "failed".to_string();
    if workflow.exit_code == 0 {
        workflow.exit_code = 1;
    }
}

fn suffix_component_results(mut results: BenchResults, component_id: &str) -> BenchResults {
    for scenario in &mut results.scenarios {
        scenario.id = format!("{}:c{}", scenario.id, component_id);
    }
    results
}

fn merge_matrix_results(
    component_ids: &[String],
    outputs: &[BenchCommandOutput],
) -> Option<BenchResults> {
    let mut merged_scenarios = Vec::new();
    let mut budget_findings = Vec::new();
    let mut component_id_seen: Option<String> = None;
    let mut iterations_seen: Option<u64> = None;
    let mut metric_policies_seen = std::collections::BTreeMap::new();

    for (component_id, output) in component_ids.iter().zip(outputs.iter()) {
        let Some(results) = output.results.clone() else {
            continue;
        };
        let suffixed = suffix_component_results(results, component_id);
        if component_id_seen.is_none() {
            component_id_seen = Some(suffixed.component_id.clone());
        }
        if iterations_seen.is_none() {
            iterations_seen = Some(suffixed.iterations);
        }
        for (key, policy) in suffixed.metric_policies {
            metric_policies_seen.entry(key).or_insert(policy);
        }
        budget_findings.extend(suffixed.budget_findings);
        merged_scenarios.extend(suffixed.scenarios);
    }

    if merged_scenarios.is_empty() && component_id_seen.is_none() {
        None
    } else {
        Some(BenchResults {
            component_id: component_ids.join(","),
            iterations: iterations_seen.unwrap_or(0),
            provenance: Default::default(),
            run_metadata: outputs
                .iter()
                .find_map(|output| output.results.as_ref()?.run_metadata.clone()),
            metadata: std::collections::BTreeMap::new(),
            metric_groups: std::collections::BTreeMap::new(),
            timeline: Vec::new(),
            span_definitions: std::collections::BTreeMap::new(),
            diagnostics: outputs
                .iter()
                .flat_map(|output| output.results.as_ref().map(|r| r.diagnostics.clone()))
                .flatten()
                .collect(),
            child_command_failures: outputs
                .iter()
                .flat_map(|output| {
                    output
                        .results
                        .as_ref()
                        .map(|r| r.child_command_failures.clone())
                })
                .flatten()
                .collect(),
            phase_events: outputs
                .iter()
                .flat_map(|output| output.results.as_ref().map(|r| r.phase_events.clone()))
                .flatten()
                .collect(),
            phase_summaries: outputs
                .iter()
                .flat_map(|output| output.results.as_ref().map(|r| r.phase_summaries.clone()))
                .flatten()
                .collect(),
            failure_classification: outputs
                .iter()
                .find_map(|output| output.results.as_ref()?.failure_classification.clone()),
            responsiveness: outputs
                .iter()
                .find_map(|output| output.results.as_ref()?.responsiveness.clone()),
            budget_findings,
            scenarios: merged_scenarios,
            metric_policies: metric_policies_seen,
            metric_policy_presets: std::collections::BTreeMap::new(),
        })
    }
}

pub(super) fn run_single_rig(
    args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_id: String,
) -> CmdResult<BenchCommandOutput> {
    let context = prepare_rig_bench_context(&rig_id, args)?;
    let matrix_components = if let Some(explicit) = args.comp.id() {
        vec![explicit.to_string()]
    } else {
        rig_bench_components(context.spec())
    };

    if matrix_components.len() <= 1 {
        let component_override = matrix_components.first().cloned();
        let shared_state = component_override
            .as_deref()
            .and_then(|id| component_shared_state(args, id, matrix_components.len()));
        return run_component_with_rig_context(
            args,
            passthrough_args,
            Some(&context),
            component_override,
            shared_state,
        );
    }

    let mut outputs = Vec::with_capacity(matrix_components.len());
    let mut first_nonzero_exit: Option<i32> = None;

    for component_id in &matrix_components {
        let shared_state = component_shared_state(args, component_id, matrix_components.len());
        let (output, exit_code) = run_component_with_rig_context(
            args,
            passthrough_args,
            Some(&context),
            Some(component_id.clone()),
            shared_state,
        )?;
        if exit_code != 0 && first_nonzero_exit.is_none() {
            first_nonzero_exit = Some(exit_code);
        }
        outputs.push(output);
    }

    let exit_code = first_nonzero_exit.unwrap_or(0);
    let mut hints = Vec::new();
    for output in &outputs {
        if let Some(output_hints) = &output.hints {
            for hint in output_hints {
                if !hints.contains(hint) {
                    hints.push(hint.clone());
                }
            }
        }
    }

    let merged_results = merge_matrix_results(&matrix_components, &outputs);
    let artifacts = merged_results
        .as_ref()
        .map(collect_artifacts)
        .unwrap_or_default();
    let budget_findings = merged_results
        .as_ref()
        .map(|results| results.budget_findings.clone())
        .unwrap_or_default();
    let gate_results = merged_results
        .as_ref()
        .map(extension_bench::normalized_gate_results)
        .unwrap_or_default();

    Ok((
        BenchCommandOutput {
            passed: outputs.iter().all(|output| output.passed),
            status: if exit_code == 0 { "passed" } else { "failed" }.to_string(),
            component: matrix_components.join(","),
            exit_code,
            iterations: args.iterations,
            artifacts,
            results: merged_results,
            budget_findings,
            gate_results,
            gate_failures: outputs
                .iter()
                .flat_map(|output| output.gate_failures.clone())
                .collect(),
            baseline_comparison: None,
            hints: if hints.is_empty() { None } else { Some(hints) },
            rig_state: Some(context.snapshot),
            failure: None,
            diagnostics: outputs
                .iter()
                .flat_map(|output| output.diagnostics.clone())
                .collect(),
            ci_context: None,
            persisted_run: outputs
                .iter()
                .find_map(|output| output.persisted_run.clone()),
        },
        exit_code,
    ))
}

pub(super) fn run_single(
    args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_id_override: Option<String>,
) -> CmdResult<BenchCommandOutput> {
    let rig_context = match rig_id_override.as_deref() {
        None => None,
        Some(rig_id) => Some(prepare_rig_bench_context(rig_id, args)?),
    };
    run_component_with_rig_context(args, passthrough_args, rig_context.as_ref(), None, None)
}

fn run_component_with_rig_context(
    args: &BenchRunArgs,
    passthrough_args: &[String],
    rig_context: Option<&RigBenchContext>,
    component_override: Option<String>,
    shared_state_override: Option<PathBuf>,
) -> CmdResult<BenchCommandOutput> {
    let rig_spec = rig_context.map(|context| context.spec());
    let rig_id = rig_context.map(|context| context.id.clone());
    let mut rig_snapshot = rig_context.map(|context| context.snapshot.clone());
    let default_component_id = rig_spec.and_then(|spec| {
        spec.bench
            .as_ref()
            .and_then(|bench| bench_component_ids(bench).into_iter().next())
    });

    let effective_id = match (component_override, args.comp.id(), default_component_id) {
        (Some(id), _, _) => id,
        (None, Some(id), _) => id.to_string(),
        (None, None, Some(default)) => default,
        (None, None, None) => args.comp.resolve_id()?,
    };

    let path_override = args
        .comp
        .path
        .clone()
        .or_else(|| rig_spec.and_then(|spec| rig_component_path(spec, &effective_id)));

    let component_override = rig_spec
        .as_ref()
        .and_then(|spec| rig_component_for_bench(spec, &effective_id));

    let mut resolve_options = ResolveOptions::with_capability_and_json(
        &effective_id,
        path_override.clone(),
        ExtensionCapability::Bench,
        args.setting_args.setting.clone(),
        args.setting_args.setting_json.clone(),
    );
    resolve_options.extension_overrides =
        super::effective_extension_overrides(&args.extension_override.extensions, rig_spec);

    let ctx = execution_context::resolve_with_component(&resolve_options, component_override)?;
    super::warn_unknown_setting_keys(
        &ctx,
        &args.setting_args,
        rig_spec
            .and_then(|spec| spec.bench.as_ref())
            .map(|bench| bench.accepted_settings.as_slice())
            .unwrap_or(&[]),
    );
    homeboy::core::hygiene::require_dependency_hygiene_for_source_with_settings(
        &ctx.source_path,
        ctx.extension_path.as_deref(),
        &ctx.settings,
        homeboy::core::hygiene::DependencyHygieneOptions { allow_stale: false },
    )?;
    let ci_profile_job =
        resolve_ci_profile_job(args.ci_profile.as_deref(), ctx.extension_id.as_deref())?;

    if let Some(snapshot) = rig_snapshot.as_mut() {
        let effective_path = ctx.source_path.to_string_lossy().into_owned();
        snapshot.set_effective_component_path(&ctx.component_id, &effective_path, |path| {
            rig::head_sha_and_branch(path)
        });
    }

    if let Some(spec) = rig_spec {
        run_rig_workload_preflight(spec, ctx.extension_id.as_deref())?;
    }

    let run_dir = RunDir::create()?;
    let resource_run = homeboy::core::engine::resource::ResourceSummaryRun::start(Some(format!(
        "bench {}",
        effective_id
    )));

    let (extra_workloads, env_provider_extensions, invocation_requirements) =
        rig_workload_runtime_inputs(rig_context, rig_spec, ctx.extension_id.as_deref());

    let selected_scenarios = selected_scenario_ids(args, rig_spec)?;
    let observation = observation::start(BenchObservationStart {
        component_id: &ctx.component_id,
        component_label: &effective_id,
        source_path: &ctx.source_path,
        args,
        selected_scenarios: &selected_scenarios,
        rig_id: rig_id.as_deref(),
        rig_snapshot: rig_snapshot.as_ref(),
        run_dir: &run_dir,
    });

    let workflow = extension_bench::run_main_bench_workflow(
        &ctx.component,
        &ctx.source_path,
        BenchRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override,
            settings: ctx.resolved_settings().string_overrides(),
            settings_json: ctx.resolved_settings().json_overrides(),
            iterations: args.iterations,
            warmup_iterations: effective_warmup_iterations(args, rig_spec),
            run_id: args.run_id.clone(),
            execution: BenchRunExecution {
                runs: args.runs,
                concurrency: args.concurrency,
            },
            baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
            regression_threshold_percent: args.regression_threshold,
            json_summary: args.json_summary,
            ci_env: ci_profile::ci_job_env(ci_profile_job.as_ref()),
            passthrough_args: bench_passthrough_args(ci_profile_job.as_ref(), passthrough_args),
            scenario_ids: selected_scenarios,
            rig_id: rig_id.clone(),
            shared_state: shared_state_override.or_else(|| args.shared_state.clone()),
            extra_workloads,
            env_provider_extensions,
            rig_package: rig_id
                .as_deref()
                .and_then(homeboy::core::rig::package_evidence),
            invocation_requirements,
        },
        &run_dir,
    );
    if let Err(error) = resource_run.write_to_run_dir(&run_dir) {
        observation::finish_error(observation, &error, &run_dir);
        return Err(error);
    }
    let mut persisted_run = None;
    let workflow = match workflow {
        Ok(mut workflow) => {
            apply_declared_bench_gates(&mut workflow, declared_bench_gates(rig_spec));
            if let Some(summary) = observation::finish_success(observation, &mut workflow, &run_dir)
            {
                let hints = workflow.hints.get_or_insert_with(Vec::new);
                hints.extend(observation::history_hints(&summary));
                persisted_run = Some(observation::persisted_run_pointer(&summary));
            }
            workflow
        }
        Err(error) => {
            observation::finish_error(observation, &error, &run_dir);
            return Err(error);
        }
    };

    let ci_context =
        ci_profile::ci_context_for_job(ci_profile_job.as_ref(), args.ci_profile.as_deref());
    let (mut output, exit_code) = if ci_context.is_some() {
        extension_bench::from_main_workflow_with_rig_and_ci_context(
            workflow,
            rig_snapshot,
            ci_context,
        )
    } else {
        extension_bench::from_main_workflow_with_rig(workflow, rig_snapshot)
    };
    output.persisted_run = persisted_run;
    Ok((output, exit_code))
}

fn resolve_ci_profile_job(
    profile_id: Option<&str>,
    extension_id: Option<&str>,
) -> homeboy::core::Result<Option<CiResolvedJob>> {
    let Some(profile_id) = profile_id else {
        return Ok(None);
    };
    let Some(extension_id) = extension_id else {
        return Err(homeboy::core::Error::validation_missing_argument(vec![
            "--extension <ID> or component bench extension".to_string(),
        ]));
    };
    let jobs = ci_profile::resolve_profile_jobs_for_extension(extension_id, profile_id)?;
    ci_profile::validate_single_profile_command(profile_id, &jobs, "bench")?;
    Ok(jobs.into_iter().next())
}

fn bench_passthrough_args(job: Option<&CiResolvedJob>, cli_args: &[String]) -> Vec<String> {
    let mut args = job.map(|job| job.spec.args.clone()).unwrap_or_default();
    args.extend(cli_args.iter().cloned());
    args
}

fn rig_workload_runtime_inputs(
    rig_context: Option<&RigBenchContext>,
    rig_spec: Option<&RigSpec>,
    extension_id: Option<&str>,
) -> (Vec<PathBuf>, Vec<String>, InvocationRequirements) {
    let Some(spec) = rig_spec else {
        return (Vec::new(), Vec::new(), InvocationRequirements::default());
    };
    let Some(extension_id) = extension_id else {
        return (Vec::new(), Vec::new(), InvocationRequirements::default());
    };

    let package_root = rig_context.and_then(|context| context.package_root());
    (
        rig::workloads_for_extension(
            spec,
            rig::RigWorkloadKind::Bench,
            package_root,
            extension_id,
        ),
        rig::env_provider_extensions_for_extension_workloads(
            spec,
            rig::RigWorkloadKind::Bench,
            extension_id,
        ),
        rig::invocation_requirements_for_extension_workloads(
            spec,
            rig::RigWorkloadKind::Bench,
            extension_id,
        ),
    )
}

fn run_rig_workload_preflight(
    spec: &RigSpec,
    extension_id: Option<&str>,
) -> homeboy::core::Result<()> {
    let groups = extension_id.and_then(|id| {
        rig::check_groups_for_extension_workloads(spec, rig::RigWorkloadKind::Bench, id)
    });
    let check = match groups {
        Some(groups) => rig::run_check_groups(spec, &groups)?,
        None => rig::run_check(spec)?,
    };
    if !check.success {
        return Err(homeboy::core::Error::rig_pipeline_failed(
            &spec.id,
            "check",
            "rig check failed; refusing to run bench against an unhealthy rig",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
    };
    use crate::test_support::with_isolated_home;
    use std::fs;

    fn write_bench_extension(home: &tempfile::TempDir, extension_id: &str) {
        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join(extension_id);
        fs::create_dir_all(&extension_dir).expect("mkdir extension");
        fs::write(
            extension_dir.join(format!("{}.json", extension_id)),
            r#"{
                "name": "Node.js",
                "version": "0.0.0",
                "bench": { "extension_script": "bench-runner.sh" }
            }"#,
        )
        .expect("write extension manifest");
    }

    fn bench_args(component: Option<&str>, path: Option<&str>) -> BenchRunArgs {
        BenchRunArgs {
            comp: PositionalComponentArgs {
                component: component.map(str::to_string),
                path: path.map(str::to_string),
            },
            extension_override: ExtensionOverrideArgs::default(),
            iterations: 1,
            warmup: None,
            runs: 1,
            run_id: None,
            shared_state: None,
            concurrency: 1,
            matrix: Vec::new(),
            runner_pool: None,
            matrix_max_tasks: None,
            matrix_max_queue_depth: None,
            expected_artifact: Vec::new(),
            baseline_args: BaselineArgs::default(),
            regression_threshold: 5.0,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            json_summary: false,
            status_file: None,
            report: Vec::new(),
            rig: vec!["rig".to_string()],
            rig_order: crate::commands::bench::BenchRigOrder::Input,
            rig_concurrency: 1,
            scenario_ids: Vec::new(),
            profile: None,
            ci_profile: None,
            ignore_default_baseline: false,
        }
    }

    #[test]
    fn rig_bench_components_prefers_matrix_over_default_component() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "mdi-substrates",
                "bench": {
                    "default_component": "legacy-default",
                    "components": ["mdi-sdi", "mdi-primary"]
                }
            }"#,
        )
        .expect("parse rig spec");

        assert_eq!(
            rig_bench_components(&rig_spec),
            vec!["mdi-sdi".to_string(), "mdi-primary".to_string()]
        );
    }

    #[test]
    fn rig_bench_components_falls_back_to_default_component() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "single-component-rig",
                "bench": { "default_component": "homeboy" }
            }"#,
        )
        .expect("parse rig spec");

        assert_eq!(rig_bench_components(&rig_spec), vec!["homeboy".to_string()]);
    }

    #[test]
    fn component_shared_state_uses_subdirs_for_matrix_only() {
        let mut args = bench_args(None, None);
        args.shared_state = Some(PathBuf::from("/tmp/shared"));

        assert_eq!(
            component_shared_state(&args, "mdi-primary", 3),
            Some(PathBuf::from("/tmp/shared/mdi-primary"))
        );
        assert_eq!(
            component_shared_state(&args, "mdi-primary", 1),
            Some(PathBuf::from("/tmp/shared"))
        );

        args.shared_state = None;
        assert_eq!(component_shared_state(&args, "mdi-primary", 3), None);
    }

    #[test]
    fn bench_path_override_updates_rig_component_before_prepare() {
        let mut rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "gutenberg-rtc",
                "components": {
                    "gutenberg": { "path": "~/Developer/gutenberg" }
                },
                "bench": { "default_component": "gutenberg" }
            }"#,
        )
        .expect("parse rig spec");
        let args = bench_args(None, Some("/home/user/Developer/_lab_workspaces/gutenberg"));

        apply_bench_path_override(&mut rig_spec, &args);

        assert_eq!(
            rig_spec.components["gutenberg"].path,
            "/home/user/Developer/_lab_workspaces/gutenberg"
        );
    }

    #[test]
    fn bench_path_override_prefers_explicit_component() {
        let mut rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "dual",
                "components": {
                    "primary": { "path": "/src/primary" },
                    "candidate": { "path": "/src/candidate" }
                },
                "bench": { "default_component": "primary" }
            }"#,
        )
        .expect("parse rig spec");
        let args = bench_args(Some("candidate"), Some("/tmp/candidate"));

        apply_bench_path_override(&mut rig_spec, &args);

        assert_eq!(rig_spec.components["primary"].path, "/src/primary");
        assert_eq!(rig_spec.components["candidate"].path, "/tmp/candidate");
    }

    #[test]
    fn declared_bench_gates_parse_from_rig_bench_spec() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "studio-bfb",
                "bench": {
                    "components": ["studio"],
                    "metric_gates": {
                        "wordpress-is-dead": {
                            "native_block_quality_pass": { "equals": 1 },
                            "tool_error_count": { "lte": 0 }
                        }
                    },
                    "result_gates": {
                        "failed_fixture_count": { "lte": 0 }
                    }
                }
            }"#,
        )
        .expect("parse rig spec");

        let gates = declared_bench_gates(Some(&rig_spec));
        let scenario_gates = gates
            .scenario_gates
            .get("wordpress-is-dead")
            .expect("scenario gates should be declared");

        assert_eq!(scenario_gates.len(), 2);
        assert!(scenario_gates.iter().any(|gate| {
            gate.metric == "native_block_quality_pass"
                && gate.op == homeboy::core::extension::bench::BenchGateOp::Eq
                && gate.value == 1.0
        }));
        assert!(scenario_gates.iter().any(|gate| {
            gate.metric == "tool_error_count"
                && gate.op == homeboy::core::extension::bench::BenchGateOp::Lte
                && gate.value == 0.0
        }));
        assert_eq!(gates.result_gates.len(), 1);
        assert_eq!(gates.result_gates[0].metric, "failed_fixture_count");
        assert_eq!(
            gates.result_gates[0].op,
            homeboy::core::extension::bench::BenchGateOp::Lte
        );
        assert_eq!(gates.result_gates[0].value, 0.0);
    }

    #[test]
    fn declared_result_gate_fails_single_pass_workflow() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "fixture-matrix",
                "bench": {
                    "result_gates": {
                        "failed_fixture_count": { "lte": 0 }
                    }
                }
            }"#,
        )
        .expect("parse rig spec");
        let results = homeboy::core::extension::bench::parse_bench_results_str(
            r#"{
                "component_id": "static-site-importer",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "static-site-fixture-matrix",
                        "iterations": 1,
                        "metrics": {
                            "failed_fixture_count": 71,
                            "fixture_count": 71
                        }
                    }
                ]
            }"#,
        )
        .expect("parse bench results");
        let mut workflow = BenchRunWorkflowResult {
            status: "passed".to_string(),
            component: "static-site-importer".to_string(),
            exit_code: 0,
            iterations: 1,
            results: Some(results),
            gate_results: Vec::new(),
            gate_failures: Vec::new(),
            baseline_comparison: None,
            hints: None,
            failure: None,
            diagnostics: Vec::new(),
        };

        apply_declared_bench_gates(&mut workflow, declared_bench_gates(Some(&rig_spec)));

        assert_eq!(workflow.status, "failed");
        assert_eq!(workflow.exit_code, 1);
        assert_eq!(workflow.gate_results.len(), 1);
        assert_eq!(
            workflow.gate_results[0].status,
            homeboy::core::gate::HomeboyGateStatus::Failed
        );
        assert!(workflow.gate_failures[0].contains("failed_fixture_count lte 0"));
    }

    #[test]
    fn rig_component_for_bench_synthesizes_extension_config() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let rig_spec: RigSpec = serde_json::from_str(&format!(
            r#"{{
                "id": "studio",
                "components": {{
                    "studio": {{
                        "path": "{}",
                        "extensions": {{
                            "fixture-bench": {{
                                "package_manager": "pnpm",
                                "workspace": "apps/studio"
                            }}
                        }}
                    }}
                }},
                "bench": {{ "default_component": "studio" }}
            }}"#,
            temp.path().display()
        ))
        .expect("parse rig spec");

        let component = rig_component_for_bench(&rig_spec, "studio")
            .expect("rig component with extensions should synthesize component");

        assert_eq!(component.id, "studio");
        assert_eq!(component.local_path, temp.path().to_string_lossy());
        let fixture_bench = component
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("fixture-bench"))
            .expect("fixture-bench config preserved");
        assert_eq!(
            fixture_bench.settings.get("package_manager"),
            Some(&serde_json::json!("pnpm"))
        );
        assert_eq!(
            fixture_bench.settings.get("workspace"),
            Some(&serde_json::json!("apps/studio"))
        );
    }

    #[test]
    fn rig_component_for_bench_absent_extension_config_falls_back() {
        let rig_spec: RigSpec = serde_json::from_str(
            r#"{
                "id": "legacy",
                "components": { "studio": { "path": "/tmp/studio" } },
                "bench": { "default_component": "studio" }
            }"#,
        )
        .expect("parse rig spec");

        assert!(rig_component_for_bench(&rig_spec, "studio").is_none());
        assert!(rig_component_for_bench(&rig_spec, "missing").is_none());
    }

    #[test]
    fn rig_component_extension_config_resolves_bench_context() {
        with_isolated_home(|home| {
            write_bench_extension(home, "fixture-bench");
            let temp = tempfile::TempDir::new().expect("component dir");
            let rig_spec: RigSpec = serde_json::from_str(&format!(
                r#"{{
                    "id": "studio",
                    "components": {{
                        "studio": {{
                            "path": "{}",
                            "extensions": {{
                                "fixture-bench": {{ "package_manager": "pnpm" }}
                            }}
                        }}
                    }},
                    "bench": {{ "default_component": "studio" }}
                }}"#,
                temp.path().display()
            ))
            .expect("parse rig spec");
            let component_override = rig_component_for_bench(&rig_spec, "studio");

            let ctx = execution_context::resolve_with_component(
                &ResolveOptions::with_capability_and_json(
                    "studio",
                    Some(temp.path().to_string_lossy().to_string()),
                    ExtensionCapability::Bench,
                    Vec::new(),
                    Vec::new(),
                ),
                component_override,
            )
            .expect("rig-owned extension config resolves bench context");

            assert_eq!(ctx.component_id, "studio");
            assert_eq!(ctx.extension_id.as_deref(), Some("fixture-bench"));
            assert!(ctx
                .settings
                .iter()
                .any(|(key, value)| key == "package_manager" && value == "pnpm"));
        });
    }

    #[test]
    fn missing_rig_extension_config_keeps_clear_error() {
        let temp = tempfile::TempDir::new().expect("component dir");
        let err = execution_context::resolve_with_component(
            &ResolveOptions::with_capability_and_json(
                "studio",
                Some(temp.path().to_string_lossy().to_string()),
                ExtensionCapability::Bench,
                Vec::new(),
                Vec::new(),
            ),
            Some(Component {
                id: "studio".to_string(),
                local_path: temp.path().to_string_lossy().to_string(),
                ..Component::default()
            }),
        )
        .expect_err("component without extensions should fail clearly");

        let message = err.to_string();
        assert!(
            message.contains("No extension provider configured"),
            "expected missing-extension error, got: {}",
            message
        );
    }

    fn bench_results(component_id: &str, scenario_id: &str, p95: f64) -> BenchResults {
        serde_json::from_value(serde_json::json!({
            "component_id": component_id,
            "iterations": 10,
            "scenarios": [
                {
                    "id": scenario_id,
                    "iterations": 10,
                    "metrics": { "p95_ms": p95 }
                }
            ],
            "metric_policies": {
                "p95_ms": { "direction": "lower_is_better" }
            }
        }))
        .expect("bench results")
    }

    fn bench_output(component: &str, results: Option<BenchResults>) -> BenchCommandOutput {
        BenchCommandOutput {
            passed: true,
            status: "passed".to_string(),
            component: component.to_string(),
            exit_code: 0,
            iterations: 10,
            artifacts: results.as_ref().map(collect_artifacts).unwrap_or_default(),
            budget_findings: results
                .as_ref()
                .map(|results| results.budget_findings.clone())
                .unwrap_or_default(),
            gate_results: Vec::new(),
            results,
            gate_failures: Vec::new(),
            baseline_comparison: None,
            hints: None,
            rig_state: None,
            failure: None,
            diagnostics: Vec::new(),
            ci_context: None,
            persisted_run: None,
        }
    }

    #[test]
    fn merge_matrix_results_suffixes_scenarios_by_component() {
        let component_ids = vec!["mdi-sdi".to_string(), "mdi-primary".to_string()];
        let outputs = vec![
            bench_output("mdi-sdi", Some(bench_results("mdi-sdi", "cold-boot", 42.0))),
            bench_output(
                "mdi-primary",
                Some(bench_results("mdi-primary", "cold-boot", 50.0)),
            ),
        ];

        let merged = merge_matrix_results(&component_ids, &outputs).expect("merged results");
        assert_eq!(merged.component_id, "mdi-sdi,mdi-primary");
        assert_eq!(merged.iterations, 10);
        assert_eq!(merged.scenarios.len(), 2);
        assert_eq!(merged.scenarios[0].id, "cold-boot:cmdi-sdi");
        assert_eq!(merged.scenarios[1].id, "cold-boot:cmdi-primary");
        assert!(merged.metric_policies.contains_key("p95_ms"));
    }

    #[test]
    fn merge_matrix_results_skips_components_without_parseable_results() {
        let component_ids = vec!["a".to_string(), "b".to_string()];
        let outputs = vec![
            bench_output("a", None),
            bench_output("b", Some(bench_results("b", "boot", 10.0))),
        ];

        let merged = merge_matrix_results(&component_ids, &outputs).expect("merged results");
        assert_eq!(merged.scenarios.len(), 1);
        assert_eq!(merged.scenarios[0].id, "boot:cb");
    }
}
