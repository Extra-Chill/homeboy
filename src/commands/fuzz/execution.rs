use std::path::Path;

use homeboy::core::engine::execution_context;
use homeboy::core::engine::invocation::InvocationRequirements;
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::{self, ExtensionCapability, ExtensionRunner, FuzzConfig};
use homeboy::core::fuzz::{parse_fuzz_results_file, FuzzCampaign};
use homeboy::core::observation::{ObservationStore, RunRecord, RunStatus};
use homeboy::core::rig::{self, RigSpec};

use super::report::{evaluate_fuzz_gates, fuzz_coverage_completeness};
use super::types::{
    FuzzCampaignContract, FuzzExecutionOutput, FuzzRunArgs, FuzzRunOutput, FuzzRunnerContract,
    FuzzWorkloadOutput,
};
use super::workloads::{
    build_target_inventory, fuzz_invocation_requirements, fuzz_workloads, load_rig,
    resolve_component_id, resolve_fuzz_context, select_workload, FuzzRigContext,
};

pub(super) fn run_run(args: FuzzRunArgs) -> homeboy::core::Result<(FuzzRunOutput, i32)> {
    let rig_context = load_rig(args.rig.as_deref(), &args.setting_args)?;
    let effective_id = resolve_component_id(
        &args.comp,
        rig_context.as_ref().map(|context| &context.spec),
    )?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let extension_id = ctx.extension_id.clone();
    let fuzz_config = extension_id
        .as_deref()
        .and_then(|extension_id| extension::load_extension(extension_id).ok())
        .and_then(|manifest| manifest.fuzz);
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        extension_id.as_deref(),
    );
    let selected_workload = select_workload(&workloads, args.workload_id.as_deref())?;
    let target_inventory = build_target_inventory(
        &ctx.component_id,
        &workloads,
        args.run_id.clone(),
        args.inventory.as_deref(),
    )?;
    let invocation_requirements =
        fuzz_invocation_requirements(rig_context.as_ref(), ctx.extension_id.as_deref());
    let run_dir = RunDir::create()?;
    let runner_output = run_fuzz_extension_script(
        &ctx,
        &args,
        rig_context.as_ref(),
        selected_workload,
        invocation_requirements,
        &run_dir,
    )?;
    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let (results, results_error) = if results_path.exists() {
        match parse_fuzz_results_file(&results_path) {
            Ok(results) => (Some(results), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let outcome = fuzz_run_outcome(
        runner_output.exit_code,
        runner_output.success,
        results.as_ref(),
        results_error.as_deref(),
    );
    let exit_code = outcome.exit_code;
    let success = outcome.success;
    let status = outcome.status.to_string();
    let rig_id = rig_context.map(|context| context.spec.id);
    let workload_id = selected_workload
        .map(|workload| workload.id.clone())
        .or_else(|| args.workload_id.clone());
    let workload_path = selected_workload.and_then(|workload| workload.manifest_path.clone());
    persist_fuzz_run_evidence(FuzzRunEvidenceInput {
        run_id: args.run_id.as_deref(),
        component_id: &ctx.component_id,
        rig_id: rig_id.as_deref(),
        workload_id: workload_id.as_deref(),
        workload_path: workload_path.as_deref(),
        status: &status,
        exit_code,
        success,
        args: &args,
        results_path: &results_path,
        results: results.as_ref(),
        results_error: results_error.as_deref(),
    })?;
    let evidence_followups = fuzz_evidence_followups(
        args.run_id.as_deref(),
        results_error.as_deref(),
        &results_path,
    );
    let campaign_contract = fuzz_campaign_contract(fuzz_config.as_ref(), args.seed.as_deref());

    Ok((
        FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: ctx.component_id,
            rig_id,
            status,
            workload_id,
            workload_path,
            run_id: args.run_id,
            seed: args.seed,
            inventory_file: args
                .inventory
                .map(|path| path.to_string_lossy().to_string()),
            max_duration: args.max_duration,
            passthrough_args: args.args,
            target_inventory: Some(target_inventory),
            execution: Some(FuzzExecutionOutput {
                kind: "fuzz".to_string(),
                extension_id: ctx.extension_id.unwrap_or_default(),
                exit_code,
                success,
                run_dir: run_dir.path().to_string_lossy().to_string(),
                results_file: results_path.to_string_lossy().to_string(),
                stdout: runner_output.stdout,
                stderr: runner_output.stderr,
            }),
            results,
            campaign_contract,
            runner_contract: default_runner_contract(),
            evidence_followups,
        },
        exit_code,
    ))
}

pub(super) struct FuzzRunOutcome {
    pub(super) status: &'static str,
    pub(super) success: bool,
    pub(super) exit_code: i32,
}

pub(super) fn fuzz_run_outcome(
    runner_exit_code: i32,
    runner_success: bool,
    results: Option<&FuzzCampaign>,
    results_error: Option<&str>,
) -> FuzzRunOutcome {
    let nested_failed = results.is_some_and(fuzz_campaign_reports_failure);
    let success = runner_success && !nested_failed && results_error.is_none();
    FuzzRunOutcome {
        status: if success { "passed" } else { "failed" },
        success,
        exit_code: if success {
            runner_exit_code
        } else if runner_exit_code == 0 {
            1
        } else {
            runner_exit_code
        },
    }
}

fn fuzz_campaign_reports_failure(campaign: &FuzzCampaign) -> bool {
    let nested_result_key = ["word", "press", "_fuzz_result"].concat();
    fuzz_metadata_reports_failure(&campaign.metadata)
        || campaign
            .metadata
            .get(nested_result_key)
            .is_some_and(fuzz_metadata_reports_failure)
}

fn fuzz_metadata_reports_failure(value: &serde_json::Value) -> bool {
    let status_failed = value
        .get("status")
        .and_then(|status| status.as_str())
        .is_some_and(|status| matches!(status, "failed" | "errored" | "error"));
    let success_failed = value.get("success").and_then(|success| success.as_bool()) == Some(false);
    let case_failed = value
        .get("case_counts")
        .is_some_and(|counts| json_u64(counts, "failed") > 0 || json_u64(counts, "errored") > 0);

    status_failed || success_failed || case_failed
}

fn json_u64(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).and_then(|entry| entry.as_u64()).unwrap_or(0)
}

pub(super) fn fuzz_campaign_contract(
    config: Option<&FuzzConfig>,
    cli_seed: Option<&str>,
) -> FuzzCampaignContract {
    let unsupported = fuzz_contract_unsupported(config);
    FuzzCampaignContract {
        case_artifact: config.and_then(|config| config.case_artifact.clone()),
        corpus_artifacts: config
            .map(|config| config.corpus_artifacts.clone())
            .unwrap_or_default(),
        seed: cli_seed
            .map(str::to_string)
            .or_else(|| config.and_then(|config| config.seed.clone())),
        replay_command: config.and_then(|config| config.replay_command.clone()),
        minimize_command: config.and_then(|config| config.minimize_command.clone()),
        result_schema: config
            .and_then(|config| config.result_schema.clone())
            .unwrap_or_else(|| homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string()),
        artifact_retention: config.and_then(|config| config.artifact_retention.clone()),
        unsupported,
    }
}

fn fuzz_contract_unsupported(config: Option<&FuzzConfig>) -> Vec<&'static str> {
    let Some(config) = config else {
        return vec![
            "case_artifact",
            "corpus_artifacts",
            "replay_command",
            "minimize_command",
            "artifact_retention",
        ];
    };
    let mut unsupported = Vec::new();
    if config.case_artifact.is_none() {
        unsupported.push("case_artifact");
    }
    if config.corpus_artifacts.is_empty() {
        unsupported.push("corpus_artifacts");
    }
    if config.replay_command.is_none() {
        unsupported.push("replay_command");
    }
    if config.minimize_command.is_none() {
        unsupported.push("minimize_command");
    }
    if config.artifact_retention.is_none() {
        unsupported.push("artifact_retention");
    }
    unsupported
}

pub(super) struct FuzzRunEvidenceInput<'a> {
    pub(super) run_id: Option<&'a str>,
    pub(super) component_id: &'a str,
    pub(super) rig_id: Option<&'a str>,
    pub(super) workload_id: Option<&'a str>,
    pub(super) workload_path: Option<&'a str>,
    pub(super) status: &'a str,
    pub(super) exit_code: i32,
    pub(super) success: bool,
    pub(super) args: &'a FuzzRunArgs,
    pub(super) results_path: &'a Path,
    pub(super) results: Option<&'a FuzzCampaign>,
    pub(super) results_error: Option<&'a str>,
}

pub(super) fn persist_fuzz_run_evidence(
    input: FuzzRunEvidenceInput<'_>,
) -> homeboy::core::Result<Option<RunRecord>> {
    let Some(run_id) = input.run_id.filter(|run_id| !run_id.trim().is_empty()) else {
        return Ok(None);
    };
    let store = ObservationStore::open_initialized()?;
    let now = chrono::Utc::now().to_rfc3339();
    let metadata = serde_json::json!({
        "source": "homeboy fuzz run",
        "workload_id": input.workload_id,
        "workload_path": input.workload_path,
        "seed": input.args.seed.clone(),
        "max_duration": input.args.max_duration.clone(),
        "passthrough_args": input.args.args.clone(),
        "exit_code": input.exit_code,
        "success": input.success,
        "status": input.status,
        "campaign_id": input.results.map(|campaign| campaign.id.as_str()),
        "results_error": input.results_error,
        "coverage_completeness": input.results.map(fuzz_coverage_completeness),
        "gates": input.results.map(evaluate_fuzz_gates),
    });
    let run = RunRecord {
        id: run_id.to_string(),
        kind: "fuzz".to_string(),
        component_id: Some(input.component_id.to_string()),
        started_at: now.clone(),
        finished_at: Some(now),
        status: if input.success {
            RunStatus::Pass.as_str().to_string()
        } else {
            RunStatus::Fail.as_str().to_string()
        },
        command: Some(fuzz_run_command(
            input.component_id,
            input.rig_id,
            input.workload_id,
            input.args,
        )),
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string()),
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        git_sha: None,
        rig_id: input.rig_id.map(str::to_string),
        metadata_json: metadata,
    };
    store.upsert_imported_run(&run)?;
    if input.results_path.is_file() {
        store.record_artifact(run_id, "fuzz_results", input.results_path)?;
    }
    Ok(Some(run))
}

fn fuzz_run_command(
    component_id: &str,
    rig_id: Option<&str>,
    workload_id: Option<&str>,
    args: &FuzzRunArgs,
) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "fuzz".to_string(),
        "run".to_string(),
        component_id.to_string(),
    ];
    if let Some(rig_id) = rig_id {
        parts.extend(["--rig".to_string(), rig_id.to_string()]);
    }
    if let Some(workload_id) = workload_id {
        parts.extend(["--workload".to_string(), workload_id.to_string()]);
    }
    if let Some(run_id) = args.run_id.as_ref() {
        parts.extend(["--run-id".to_string(), run_id.clone()]);
    }
    if let Some(seed) = args.seed.as_ref() {
        parts.extend(["--seed".to_string(), seed.clone()]);
    }
    if let Some(max_duration) = args.max_duration.as_ref() {
        parts.extend(["--max-duration".to_string(), max_duration.clone()]);
    }
    if !args.args.is_empty() {
        parts.push("--".to_string());
        parts.extend(args.args.clone());
    }
    parts.join(" ")
}

pub(super) fn fuzz_evidence_followups(
    run_id: Option<&str>,
    results_error: Option<&str>,
    results_path: &Path,
) -> Vec<String> {
    let mut followups = match run_id.filter(|run_id| !run_id.trim().is_empty()) {
        Some(run_id) => vec![
            format!("homeboy runs show {run_id}"),
            format!("homeboy runs evidence {run_id}"),
            format!("homeboy runs artifacts {run_id}"),
        ],
        None => vec![
            "Use --run-id <stable-id> when the downstream runner records persisted Homeboy evidence.".to_string(),
            "Inspect persisted proof with `homeboy runs show <run-id>` and `homeboy runs evidence <run-id>`.".to_string(),
        ],
    };
    if let Some(error) = results_error {
        followups.push(format!(
            "Inspect raw fuzz results artifact at {} because normalization failed: {error}",
            results_path.display()
        ));
    }
    followups
}

pub(super) fn default_runner_contract() -> FuzzRunnerContract {
    FuzzRunnerContract {
        capability: "fuzz".to_string(),
        extension_script_required: true,
        env: vec![
            "HOMEBOY_FUZZ_RESULTS_FILE",
            "HOMEBOY_FUZZ_WORKLOAD_ID",
            "HOMEBOY_FUZZ_WORKLOAD_PATH",
            "WP_CODEBOX_FUZZ_WORKLOAD_ROOT",
            "HOMEBOY_FUZZ_RUN_ID",
            "HOMEBOY_FUZZ_SEED",
            "HOMEBOY_FUZZ_INVENTORY_FILE",
            "HOMEBOY_FUZZ_MAX_DURATION",
        ],
    }
}

fn run_fuzz_extension_script(
    ctx: &execution_context::ExecutionContext,
    args: &FuzzRunArgs,
    rig_context: Option<&FuzzRigContext>,
    workload: Option<&FuzzWorkloadOutput>,
    invocation_requirements: InvocationRequirements,
    run_dir: &RunDir,
) -> homeboy::core::Result<homeboy::core::extension::RunnerOutput> {
    let execution_context =
        extension::resolve_execution_context(&ctx.component, ExtensionCapability::Fuzz)?;
    if execution_context.script_path.trim().is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "fuzz.extension_script",
            format!(
                "Extension '{}' declares fuzz manifest support but no fuzz runner script",
                execution_context.extension_id
            ),
            Some(execution_context.extension_id),
            None,
        )
        .with_hint(
            "Add fuzz.extension_script to execute workloads, or use `homeboy fuzz list` for manifest-only discovery",
        ));
    }
    let mut runner = ExtensionRunner::for_context(execution_context)
        .component(ctx.component.clone())
        .settings(&args.setting_args.setting)
        .settings_json(&args.setting_args.setting_json)
        .path_override(args.comp.path.clone())
        .with_run_dir(run_dir)
        .invocation_requirements(invocation_requirements)
        .script_args(&args.args);

    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let env = fuzz_runner_env(args, rig_context, workload, &results_path, run_dir)?;
    for (key, value) in env {
        runner = runner.env(&key, &value);
    }

    runner.run()
}

pub(super) fn fuzz_runner_env(
    args: &FuzzRunArgs,
    rig_context: Option<&FuzzRigContext>,
    workload: Option<&FuzzWorkloadOutput>,
    results_path: &Path,
    run_dir: &RunDir,
) -> homeboy::core::Result<Vec<(String, String)>> {
    let mut env = vec![(
        "HOMEBOY_FUZZ_RESULTS_FILE".to_string(),
        results_path.to_string_lossy().to_string(),
    )];
    if let Some(workload) = workload {
        env.push(("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), workload.id.clone()));
        if let Some(path) = fuzz_runner_workload_path(workload, rig_context, run_dir)? {
            env.push(("HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(), path.clone()));
        }
    }
    if let Some(package_root) = rig_context.and_then(|context| context.package_root.as_ref()) {
        env.push((
            "WP_CODEBOX_FUZZ_WORKLOAD_ROOT".to_string(),
            package_root.to_string_lossy().to_string(),
        ));
    }
    push_opt_env(&mut env, "HOMEBOY_FUZZ_RUN_ID", args.run_id.as_ref());
    push_opt_env(&mut env, "HOMEBOY_FUZZ_SEED", args.seed.as_ref());
    if let Some(path) = args.inventory.as_ref() {
        env.push((
            "HOMEBOY_FUZZ_INVENTORY_FILE".to_string(),
            path.to_string_lossy().to_string(),
        ));
    }
    push_opt_env(
        &mut env,
        "HOMEBOY_FUZZ_MAX_DURATION",
        args.max_duration.as_ref(),
    );
    Ok(env)
}

fn fuzz_runner_workload_path(
    workload: &FuzzWorkloadOutput,
    rig_context: Option<&FuzzRigContext>,
    run_dir: &RunDir,
) -> homeboy::core::Result<Option<String>> {
    let Some(manifest_path) = workload.manifest_path.as_ref() else {
        return Ok(None);
    };
    let Some(rig_context) = rig_context else {
        return Ok(Some(manifest_path.clone()));
    };

    let source_path = Path::new(manifest_path);
    let raw = std::fs::read_to_string(source_path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(manifest_path.clone()))
    })?;
    let mut value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "fuzz_workload",
            format!(
                "Failed to parse fuzz workload JSON '{}': {error}",
                source_path.display()
            ),
            Some(manifest_path.clone()),
            None,
        )
    })?;

    expand_fuzz_workload_strings(&mut value, rig_context);
    inject_fuzz_runtime_context(&mut value, rig_context);

    let output_file = format!(
        "fuzz-workload-{}.json",
        sanitize_workload_file_segment(&workload.id)
    );
    let output_path = run_dir.step_file(&output_file);
    let json = serde_json::to_string_pretty(&value).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to encode expanded fuzz workload: {error}"
        ))
    })?;
    std::fs::write(&output_path, format!("{json}\n")).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(output_path.display().to_string()),
        )
    })?;

    Ok(Some(output_path.to_string_lossy().to_string()))
}

fn expand_fuzz_workload_strings(value: &mut serde_json::Value, rig_context: &FuzzRigContext) {
    match value {
        serde_json::Value::String(text) => {
            *text = expand_fuzz_rig_string(rig_context, text);
        }
        serde_json::Value::Array(entries) => {
            for entry in entries {
                expand_fuzz_workload_strings(entry, rig_context);
            }
        }
        serde_json::Value::Object(map) => {
            for entry in map.values_mut() {
                expand_fuzz_workload_strings(entry, rig_context);
            }
        }
        _ => {}
    }
}

fn expand_fuzz_rig_string(rig_context: &FuzzRigContext, input: &str) -> String {
    let spec = fuzz_expansion_rig_spec(rig_context);
    let input = match rig_context.package_root.as_ref() {
        Some(package_root) => input.replace("${package.root}", &package_root.to_string_lossy()),
        None => input.to_string(),
    };
    rig::expand::expand_vars(&spec, &input)
}

fn fuzz_expansion_rig_spec(rig_context: &FuzzRigContext) -> RigSpec {
    let mut spec = rig_context.spec.clone();
    let Some(package_root) = rig_context.package_root.as_ref() else {
        return spec;
    };
    let package_root = package_root.to_string_lossy();
    for (component_id, component) in spec.components.iter_mut() {
        component.path = expanded_fuzz_component_path(rig_context, component_id, &component.path);
        if let Some(checkout_root) = component.checkout_root.as_mut() {
            *checkout_root = checkout_root.replace("${package.root}", &package_root);
        }
    }
    spec
}

fn inject_fuzz_runtime_context(value: &mut serde_json::Value, rig_context: &FuzzRigContext) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let metadata = root
        .entry("metadata")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };

    let components = rig_context
        .spec
        .components
        .iter()
        .map(|(id, component)| {
            let mut component_value = serde_json::to_value(component)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
            if let Some(component_object) = component_value.as_object_mut() {
                component_object.insert(
                    "path".to_string(),
                    serde_json::Value::String(expanded_fuzz_component_path(
                        rig_context,
                        id,
                        &component.path,
                    )),
                );
            }
            (id.clone(), component_value)
        })
        .collect::<serde_json::Map<_, _>>();

    metadata.insert(
        "homeboy_runtime_context".to_string(),
        serde_json::json!({
            "schema": "homeboy/fuzz-workload-runtime-context/v1",
            "rig_id": rig_context.spec.id.clone(),
            "package_root": rig_context.package_root.as_ref().map(|path| path.to_string_lossy().to_string()),
            "components": components,
        }),
    );
}

fn expanded_fuzz_component_path(
    rig_context: &FuzzRigContext,
    component_id: &str,
    fallback: &str,
) -> String {
    let env_name = crate::core::rig::expand::rig_component_path_override_env_name(
        &rig_context.spec.id,
        component_id,
    );
    if let Ok(value) = std::env::var(env_name) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return shellexpand::tilde(trimmed).to_string();
        }
    }
    expand_fuzz_rig_string(rig_context, fallback)
}

fn sanitize_workload_file_segment(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "workload".to_string()
    } else {
        trimmed.to_string()
    }
}

fn push_opt_env(env: &mut Vec<(String, String)>, key: &str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        env.push((key.to_string(), value.clone()));
    }
}
