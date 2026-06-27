use std::path::{Path, PathBuf};
use std::time::Duration;

use homeboy::core::artifact_ref::EvidenceRef;
use homeboy::core::artifacts::{
    record_artifact_postprocess_outputs, run_artifact_postprocess_steps, ArtifactPostprocessContext,
};
use homeboy::core::engine::execution_context;
use homeboy::core::engine::invocation::InvocationRequirements;
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::{self, ExtensionCapability, ExtensionRunner, FuzzConfig};
use homeboy::core::fuzz::{
    parse_fuzz_results_file, FuzzArtifact, FuzzCampaign, FuzzFindingStatus, FuzzGateProfile,
};
use homeboy::core::lifecycle::LifecyclePhaseStatus;
use homeboy::core::observation::{ObservationStore, RunRecord, RunStatus};
use homeboy::core::rig::{self, FuzzPrepareReport, RigSpec};
use uuid::Uuid;

use super::report::{
    evaluate_expected_metric_gates, evaluate_fuzz_gates_for_profile, fuzz_coverage_completeness,
    fuzz_result_envelope_evidence_ref, fuzz_result_envelope_from_campaign, gate_status,
    persist_fuzz_run_result_envelope,
};
use super::types::{
    FuzzArtifactPostprocessOutput, FuzzCampaignContract, FuzzExecutionOutput, FuzzRunArgs,
    FuzzRunOutput, FuzzRunnerContract, FuzzWorkloadOutput,
};
use super::workloads::{
    build_target_inventory, fuzz_invocation_requirements, fuzz_workloads, load_rig,
    resolve_component_id, resolve_fuzz_context, select_workload, FuzzRigContext,
};

pub(super) fn run_run(mut args: FuzzRunArgs) -> homeboy::core::Result<(FuzzRunOutput, i32)> {
    if args
        .run_id
        .as_deref()
        .is_none_or(|run_id| run_id.trim().is_empty())
    {
        args.run_id = Some(format!("fuzz-{}", Uuid::new_v4()));
    }
    let rig_context = load_rig(args.rig.as_deref(), &args.setting_args)?;
    if let Some(context) = rig_context.as_ref() {
        let prepare_settings = fuzz_prepare_settings(&args);
        if let Some(prepare) = rig::run_fuzz_prepare(&context.spec, &prepare_settings)? {
            if !prepare.success {
                return Err(homeboy::core::Error::rig_pipeline_failed(
                    &context.spec.id,
                    "fuzz_prepare",
                    fuzz_prepare_failure_message(&prepare),
                ));
            }
        }
    }
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
    let artifacts_dir =
        run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_ARTIFACTS_DIR);
    let postprocess = run_fuzz_artifact_postprocess(
        rig_context.as_ref(),
        extension_id.as_deref(),
        selected_workload,
        &results_path,
        &artifacts_dir,
    )?;
    let (results, results_error) = if results_path.exists() {
        match parse_fuzz_results_file(&results_path) {
            Ok(results) => (Some(results), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let artifact_ref_validation = fuzz_artifact_ref_validation(results.as_ref(), &artifacts_dir);
    let artifact_validation_error = fuzz_run_artifact_validation_error(&args, results.as_ref());
    let postprocess_error = fuzz_postprocess_error(&postprocess);
    let expected_metric_gates =
        evaluate_expected_metric_gates(results.as_ref(), &args.expect_metric);
    let expected_metric_error = fuzz_expected_metric_error(&expected_metric_gates);
    let combined_results_error = results_error
        .as_deref()
        .or(postprocess_error.as_deref())
        .or(artifact_validation_error.as_deref())
        .or(expected_metric_error.as_deref())
        .or(artifact_ref_validation.error.as_deref());
    let outcome = fuzz_run_outcome(
        runner_output.exit_code,
        runner_output.success,
        runner_output.timed_out,
        results.as_ref(),
        combined_results_error,
        args.gate_profile.as_core(),
    );
    let exit_code = outcome.exit_code;
    let success = outcome.success;
    let status = outcome.status.to_string();
    let rig_id = rig_context.map(|context| context.spec.id);
    let workload_id = selected_workload
        .map(|workload| workload.id.clone())
        .or_else(|| args.workload_id.clone());
    let workload_path = selected_workload.and_then(|workload| workload.manifest_path.clone());
    let persisted_evidence = persist_fuzz_run_evidence(FuzzRunEvidenceInput {
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
        artifacts_dir: &artifacts_dir,
        results: results.as_ref(),
        expected_metric_gates: &expected_metric_gates,
        results_error: combined_results_error,
        missing_artifact_refs: &artifact_ref_validation.missing_refs,
        postprocess: &postprocess,
    })?;
    let evidence_followups = fuzz_evidence_followups(
        args.run_id.as_deref(),
        combined_results_error,
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
            run_id: args.run_id.clone(),
            seed: args.seed.clone(),
            inventory_file: args
                .inventory
                .clone()
                .map(|path| path.to_string_lossy().to_string()),
            max_duration: args.max_duration.clone(),
            passthrough_args: args.args.clone(),
            requested_settings: fuzz_requested_settings(&args),
            gates: fuzz_run_gates(
                results.as_ref(),
                args.gate_profile.as_core(),
                &expected_metric_gates,
            ),
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
            postprocess,
            results,
            campaign_contract,
            runner_contract: fuzz_runner_contract(fuzz_config.as_ref()),
            evidence_refs: persisted_evidence.evidence_refs,
            evidence_followups,
        },
        exit_code,
    ))
}

pub(super) fn fuzz_run_artifact_validation_error(
    args: &FuzzRunArgs,
    results: Option<&FuzzCampaign>,
) -> Option<String> {
    if !args.require_case_log && !args.require_coverage_summary && !args.require_result_envelope {
        return None;
    }

    let Some(campaign) = results else {
        return Some(
            "strict fuzz artifact validation requested but runner did not emit a fuzz campaign"
                .to_string(),
        );
    };

    let mut missing = Vec::new();
    if args.require_case_log && !campaign_has_artifact(campaign, &["case-log", "case_log"]) {
        missing.push("case log (--require-case-log)");
    }
    if args.require_coverage_summary
        && campaign.coverage_summary.is_none()
        && !campaign_has_artifact(campaign, &["coverage-summary", "coverage_summary"])
    {
        missing.push("coverage summary (--require-coverage-summary)");
    }
    if args.require_result_envelope
        && !campaign_has_artifact(
            campaign,
            &[
                "result-envelope",
                "result_envelope",
                "fuzz-result-envelope",
                "fuzz_result_envelope",
            ],
        )
    {
        missing.push("result envelope (--require-result-envelope)");
    }

    (!missing.is_empty()).then(|| {
        format!(
            "strict fuzz artifact validation failed; missing required artifact(s): {}",
            missing.join(", ")
        )
    })
}

pub(super) fn fuzz_expected_metric_error(
    gates: &[super::types::FuzzGateEvaluation],
) -> Option<String> {
    let failed = gates
        .iter()
        .filter(|gate| gate.status != "passed")
        .map(|gate| {
            format!(
                "{} expected {} observed {}",
                gate.metric, gate.expected, gate.observed
            )
        })
        .collect::<Vec<_>>();

    (!failed.is_empty())
        .then(|| format!("fuzz expected metric gate(s) failed: {}", failed.join("; ")))
}

fn fuzz_run_gates(
    results: Option<&FuzzCampaign>,
    gate_profile: FuzzGateProfile,
    expected_metric_gates: &[super::types::FuzzGateEvaluation],
) -> Vec<super::types::FuzzGateEvaluation> {
    let mut gates = results
        .map(|results| evaluate_fuzz_gates_for_profile(results, gate_profile))
        .unwrap_or_default();
    gates.extend(expected_metric_gates.iter().cloned());
    gates
}

fn fuzz_requested_settings(args: &FuzzRunArgs) -> serde_json::Value {
    serde_json::json!({
        "setting": args.setting_args.setting,
        "setting_json": args.setting_args.setting_json,
        "expect_metric": args.expect_metric,
    })
}

fn campaign_has_artifact(campaign: &FuzzCampaign, aliases: &[&str]) -> bool {
    campaign
        .artifacts
        .iter()
        .any(|artifact| fuzz_artifact_matches(artifact, aliases))
        || campaign_metadata_has_artifact_ref(&campaign.metadata, aliases)
}

fn fuzz_artifact_matches(artifact: &FuzzArtifact, aliases: &[&str]) -> bool {
    aliases
        .iter()
        .any(|alias| artifact.id == *alias || artifact.kind == *alias)
}

fn campaign_metadata_has_artifact_ref(metadata: &serde_json::Value, aliases: &[&str]) -> bool {
    metadata
        .get("artifact_refs")
        .and_then(|refs| refs.as_array())
        .is_some_and(|refs| {
            refs.iter()
                .any(|artifact_ref| artifact_ref_matches_alias(artifact_ref, aliases))
        })
}

fn artifact_ref_matches_alias(artifact_ref: &serde_json::Value, aliases: &[&str]) -> bool {
    let fields = ["id", "kind", "name", "role", "semantic_key"];
    aliases.iter().any(|alias| {
        fields.iter().any(|field| {
            artifact_ref
                .get(field)
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == *alias)
        })
    })
}

fn fuzz_prepare_settings(args: &FuzzRunArgs) -> Vec<(String, String)> {
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

fn fuzz_prepare_failure_message(prepare: &FuzzPrepareReport) -> String {
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
        "rig fuzz preparation failed; refusing to run fuzz workload".to_string()
    } else {
        format!(
            "rig fuzz preparation failed; refusing to run fuzz workload. Failed fuzz_prepare steps: {}",
            failed_steps.join("; ")
        )
    }
}

pub(super) fn run_fuzz_artifact_postprocess(
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
    workload: Option<&FuzzWorkloadOutput>,
    results_path: &Path,
    artifacts_dir: &Path,
) -> homeboy::core::Result<Vec<FuzzArtifactPostprocessOutput>> {
    let steps = fuzz_artifact_postprocess_steps(rig_context, extension_id, workload);
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    let expand =
        |value: &str| expand_postprocess_path(value, rig_context, results_path, artifacts_dir);
    let outputs = run_artifact_postprocess_steps(
        &steps,
        &ArtifactPostprocessContext {
            artifact_root: artifacts_dir,
            input_root: Some(results_path),
            path_expander: Some(&expand),
        },
    )?;
    Ok(outputs
        .into_iter()
        .map(FuzzArtifactPostprocessOutput::from)
        .collect())
}

fn fuzz_artifact_postprocess_steps(
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
    workload: Option<&FuzzWorkloadOutput>,
) -> Vec<homeboy::core::rig::ArtifactPostprocessSpec> {
    let Some((context, extension_id, workload)) =
        rig_context
            .zip(extension_id)
            .and_then(|(context, extension_id)| {
                workload.map(|workload| (context, extension_id, workload))
            })
    else {
        return Vec::new();
    };
    let Some(manifest_path) = workload.manifest_path.as_deref() else {
        return Vec::new();
    };
    let expanded = rig::workload_path_expansions_for_extension(
        &context.spec,
        rig::RigWorkloadKind::Fuzz,
        context.package_root.as_deref(),
        extension_id,
    );
    let Some(entries) = context.spec.fuzz_workloads.get(extension_id) else {
        return Vec::new();
    };
    entries
        .iter()
        .zip(expanded.iter())
        .find(|(_, expansion)| expansion.expanded_path == Path::new(manifest_path))
        .map(|(entry, _)| entry.artifact_postprocess().to_vec())
        .unwrap_or_default()
}

fn expand_postprocess_path(
    value: &str,
    rig_context: Option<&FuzzRigContext>,
    results_path: &Path,
    artifacts_dir: &Path,
) -> PathBuf {
    let expanded = value
        .replace("${run.fuzz_results}", &results_path.to_string_lossy())
        .replace("${run.fuzz_artifacts}", &artifacts_dir.to_string_lossy());
    PathBuf::from(match rig_context {
        Some(context) => rig::expand::expand_vars(&context.spec, &expanded),
        None => expanded,
    })
}

impl From<homeboy::core::artifacts::ArtifactPostprocessOutput> for FuzzArtifactPostprocessOutput {
    fn from(output: homeboy::core::artifacts::ArtifactPostprocessOutput) -> Self {
        Self {
            id: output.id,
            helper: output.helper,
            action: output.action,
            input: output.input,
            output: output.output,
            required: output.required,
            exit_code: output.exit_code,
            success: output.success,
            stdout: output.stdout,
            stderr: output.stderr,
            error: output.error,
            artifacts: output.artifacts,
        }
    }
}

pub(super) fn fuzz_postprocess_error(outputs: &[FuzzArtifactPostprocessOutput]) -> Option<String> {
    let failed = outputs
        .iter()
        .filter(|output| output.required && !output.success)
        .map(|output| match output.error.as_deref() {
            Some(error) => error.to_string(),
            None => format!("artifact postprocess `{}` failed", output.id),
        })
        .collect::<Vec<_>>();
    (!failed.is_empty()).then(|| {
        format!(
            "required fuzz artifact postprocess step(s) failed: {}",
            failed.join("; ")
        )
    })
}

pub(super) struct FuzzRunOutcome {
    pub(super) status: &'static str,
    pub(super) success: bool,
    pub(super) exit_code: i32,
}

pub(super) fn fuzz_run_outcome(
    runner_exit_code: i32,
    runner_success: bool,
    runner_timed_out: bool,
    results: Option<&FuzzCampaign>,
    results_error: Option<&str>,
    gate_profile: FuzzGateProfile,
) -> FuzzRunOutcome {
    if runner_timed_out {
        return FuzzRunOutcome {
            status: "timeout",
            success: false,
            exit_code: 124,
        };
    }

    if let Some(non_proof_status) = results.and_then(fuzz_campaign_non_proof_status) {
        return FuzzRunOutcome {
            status: non_proof_status,
            success: false,
            exit_code: if runner_exit_code == 0 {
                1
            } else {
                runner_exit_code
            },
        };
    }

    let campaign_failed = gate_profile != FuzzGateProfile::Measurement
        && results.is_some_and(fuzz_campaign_reports_failure);
    let campaign_passed = results.is_some_and(fuzz_campaign_reports_success);
    let success =
        (runner_success || campaign_passed) && !campaign_failed && results_error.is_none();
    FuzzRunOutcome {
        status: if success { "passed" } else { "failed" },
        success,
        exit_code: if success {
            0
        } else if runner_exit_code == 0 {
            1
        } else {
            runner_exit_code
        },
    }
}

fn fuzz_campaign_reports_success(campaign: &FuzzCampaign) -> bool {
    fuzz_metadata_reports_success(&campaign.metadata)
}

fn fuzz_metadata_reports_success(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.get("success").and_then(|success| success.as_bool()) == Some(true)
                || object
                    .get("status")
                    .and_then(|status| status.as_str())
                    .is_some_and(|status| matches!(status, "pass" | "passed" | "success"))
        }
        _ => false,
    }
}

fn fuzz_campaign_non_proof_status(campaign: &FuzzCampaign) -> Option<&'static str> {
    if let Some(status) = fuzz_metadata_non_proof_status(&campaign.metadata) {
        return Some(status);
    }

    if campaign.lifecycle.as_ref().is_some_and(|lifecycle| {
        lifecycle
            .phases
            .iter()
            .any(|phase| phase.status == LifecyclePhaseStatus::Skipped)
    }) {
        return Some("skipped");
    }

    None
}

fn fuzz_metadata_non_proof_status(value: &serde_json::Value) -> Option<&'static str> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(status) = object.get("status").and_then(|status| status.as_str()) {
                let normalized = status.trim().to_ascii_lowercase();
                if matches!(
                    normalized.as_str(),
                    "skipped" | "skip" | "unsupported" | "not_executed" | "not-executed"
                ) {
                    return Some(match normalized.as_str() {
                        "unsupported" => "unsupported",
                        "not_executed" | "not-executed" => "not_executed",
                        _ => "skipped",
                    });
                }
            }
            object.values().find_map(fuzz_metadata_non_proof_status)
        }
        serde_json::Value::Array(values) => values.iter().find_map(fuzz_metadata_non_proof_status),
        _ => None,
    }
}

fn fuzz_campaign_reports_failure(campaign: &FuzzCampaign) -> bool {
    fuzz_metadata_reports_failure(&campaign.metadata)
        || campaign.findings.iter().any(|finding| {
            matches!(
                finding.status,
                FuzzFindingStatus::Open | FuzzFindingStatus::Confirmed
            )
        })
        || campaign.lifecycle.as_ref().is_some_and(|lifecycle| {
            lifecycle
                .phases
                .iter()
                .any(|phase| phase.status == LifecyclePhaseStatus::Failed)
        })
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

    status_failed || success_failed || case_failed || metadata_failure_count_reports_failure(value)
}

fn metadata_failure_count_reports_failure(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            (is_failure_count_key(key)
                && json_numeric_value(value).is_some_and(|count| count > 0.0))
                || metadata_failure_count_reports_failure(value)
        }),
        serde_json::Value::Array(values) => {
            values.iter().any(metadata_failure_count_reports_failure)
        }
        _ => false,
    }
}

fn is_failure_count_key(key: &str) -> bool {
    key == "failure_count" || key.ends_with("_failure_count")
}

fn json_numeric_value(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))
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
    pub(super) artifacts_dir: &'a Path,
    pub(super) results: Option<&'a FuzzCampaign>,
    pub(super) expected_metric_gates: &'a [super::types::FuzzGateEvaluation],
    pub(super) results_error: Option<&'a str>,
    pub(super) missing_artifact_refs: &'a [String],
    pub(super) postprocess: &'a [FuzzArtifactPostprocessOutput],
}

pub(super) struct FuzzRunPersistedEvidence {
    #[allow(dead_code)]
    pub(super) run: Option<RunRecord>,
    pub(super) evidence_refs: Vec<EvidenceRef>,
}

pub(super) fn persist_fuzz_run_evidence(
    input: FuzzRunEvidenceInput<'_>,
) -> homeboy::core::Result<FuzzRunPersistedEvidence> {
    let run_id = input
        .run_id
        .filter(|run_id| !run_id.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("fuzz-{}", Uuid::new_v4()));
    let store = ObservationStore::open_initialized()?;
    let now = chrono::Utc::now().to_rfc3339();
    let metadata = serde_json::json!({
        "source": "homeboy fuzz run",
        "workload_id": input.workload_id,
        "workload_path": input.workload_path,
        "seed": input.args.seed.clone(),
        "max_duration": input.args.max_duration.clone(),
        "passthrough_args": input.args.args.clone(),
        "tracker_refs": input.args.tracker_refs,
        "exit_code": input.exit_code,
        "success": input.success,
        "status": input.status,
        "requested_settings": fuzz_requested_settings(input.args),
        "campaign_id": input.results.map(|campaign| campaign.id.as_str()),
        "results_error": input.results_error,
        "missing_artifact_refs": input.missing_artifact_refs,
        "coverage_completeness": input.results.map(fuzz_coverage_completeness),
        "gates": fuzz_run_gates(
            input.results,
            input.args.gate_profile.as_core(),
            input.expected_metric_gates
        ),
        "gate_status": gate_status(&fuzz_run_gates(
            input.results,
            input.args.gate_profile.as_core(),
            input.expected_metric_gates
        )),
    });
    let run = RunRecord {
        id: run_id.clone(),
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
    let mut evidence_refs = Vec::new();
    if input.results_path.is_file() {
        store.record_artifact(&run_id, "fuzz_results", input.results_path)?;
    }
    if let Some(campaign) = input.results {
        let mut envelope =
            fuzz_result_envelope_from_campaign(input.args, input.component_id, campaign, None)?;
        envelope.status = gate_status(&evaluate_fuzz_gates_for_profile(
            campaign,
            input.args.gate_profile.as_core(),
        ));
        if let Some(artifact) = persist_fuzz_run_result_envelope(Some(&run_id), &envelope)? {
            evidence_refs.push(fuzz_result_envelope_evidence_ref(&artifact));
        }
    }
    if input.artifacts_dir.is_dir() {
        store.record_directory_artifact_with_metadata(
            &run_id,
            "fuzz_artifacts",
            input.artifacts_dir,
            serde_json::json!({
                "source": "HOMEBOY_FUZZ_ARTIFACTS_DIR",
                "missing_artifact_refs": input.missing_artifact_refs,
            }),
        )?;
    }
    let generic_postprocess_outputs = input
        .postprocess
        .iter()
        .map(
            |output| homeboy::core::artifacts::ArtifactPostprocessOutput {
                id: output.id.clone(),
                helper: output.helper.clone(),
                action: output.action.clone(),
                input: output.input.clone(),
                output: output.output.clone(),
                required: output.required,
                exit_code: output.exit_code,
                success: output.success,
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                error: output.error.clone(),
                artifacts: output.artifacts.clone(),
            },
        )
        .collect::<Vec<_>>();
    record_artifact_postprocess_outputs(&store, &run_id, &generic_postprocess_outputs)?;
    Ok(FuzzRunPersistedEvidence {
        run: Some(run),
        evidence_refs,
    })
}

#[derive(Default)]
pub(super) struct FuzzArtifactRefValidation {
    pub(super) missing_refs: Vec<String>,
    pub(super) error: Option<String>,
}

pub(super) fn fuzz_artifact_ref_validation(
    results: Option<&FuzzCampaign>,
    artifacts_dir: &Path,
) -> FuzzArtifactRefValidation {
    let Some(campaign) = results else {
        return FuzzArtifactRefValidation::default();
    };
    let mut missing_refs = Vec::new();
    for artifact in &campaign.artifacts {
        if let Some(path) = artifact
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.path.as_deref())
        {
            collect_missing_artifact_ref(path, artifacts_dir, &mut missing_refs);
        }
    }
    collect_missing_artifact_refs_from_metadata(
        &campaign.metadata,
        artifacts_dir,
        &mut missing_refs,
    );
    missing_refs.sort();
    missing_refs.dedup();

    let error = (!missing_refs.is_empty()).then(|| {
        format!(
            "fuzz campaign references artifact path(s) missing from HOMEBOY_FUZZ_ARTIFACTS_DIR: {}",
            missing_refs.join(", ")
        )
    });
    FuzzArtifactRefValidation {
        missing_refs,
        error,
    }
}

fn collect_missing_artifact_refs_from_metadata(
    value: &serde_json::Value,
    artifacts_dir: &Path,
    missing_refs: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(refs) = object.get("artifact_refs") {
                collect_artifact_refs_value(refs, artifacts_dir, missing_refs);
            }
            for value in object.values() {
                collect_missing_artifact_refs_from_metadata(value, artifacts_dir, missing_refs);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_missing_artifact_refs_from_metadata(value, artifacts_dir, missing_refs);
            }
        }
        _ => {}
    }
}

fn collect_artifact_refs_value(
    value: &serde_json::Value,
    artifacts_dir: &Path,
    missing_refs: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(path) => {
            collect_missing_artifact_ref(path, artifacts_dir, missing_refs)
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_artifact_refs_value(value, artifacts_dir, missing_refs);
            }
        }
        serde_json::Value::Object(object) => {
            for key in ["path", "artifact", "ref"] {
                if let Some(path) = object.get(key).and_then(serde_json::Value::as_str) {
                    collect_missing_artifact_ref(path, artifacts_dir, missing_refs);
                    return;
                }
            }
        }
        _ => {}
    }
}

fn collect_missing_artifact_ref(path: &str, artifacts_dir: &Path, missing_refs: &mut Vec<String>) {
    let Some(resolved) = resolve_local_fuzz_artifact_ref(path, artifacts_dir) else {
        return;
    };
    if !resolved.exists() {
        missing_refs.push(path.to_string());
    }
}

fn resolve_local_fuzz_artifact_ref(path: &str, artifacts_dir: &Path) -> Option<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty()
        || trimmed.contains("://")
        || trimmed.starts_with("homeboy://")
        || trimmed.starts_with("runner-artifact://")
    {
        return None;
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        candidate
            .starts_with(artifacts_dir)
            .then(|| candidate.to_path_buf())
    } else {
        (!trimmed.starts_with(".."))
            .then(|| artifacts_dir.join(candidate))
            .filter(|path| path.starts_with(artifacts_dir))
    }
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
    for tracker_ref in &args.tracker_refs {
        parts.extend([
            "--tracker-ref".to_string(),
            format!("{}:{}", tracker_ref.kind, tracker_ref.id),
        ]);
    }
    if let Some(seed) = args.seed.as_ref() {
        parts.extend(["--seed".to_string(), seed.clone()]);
    }
    if let Some(max_duration) = args.max_duration.as_ref() {
        parts.extend(["--max-duration".to_string(), max_duration.clone()]);
    }
    if args.require_case_log {
        parts.push("--require-case-log".to_string());
    }
    if args.require_coverage_summary {
        parts.push("--require-coverage-summary".to_string());
    }
    if args.require_result_envelope {
        parts.push("--require-result-envelope".to_string());
    }
    for (metric, expected) in &args.expect_metric {
        parts.extend([
            "--expect-metric".to_string(),
            format!("{metric}={expected}"),
        ]);
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
            format!("homeboy fuzz inspect {run_id}"),
            format!("homeboy runs show {run_id}"),
            format!("homeboy runs evidence {run_id}"),
            format!("homeboy runs artifacts {run_id}"),
        ],
        None => vec![
            "Use --run-id <stable-id> when the downstream runner records persisted Homeboy evidence.".to_string(),
            "Inspect the raw runner result with `homeboy fuzz inspect <run-id>` (no runner-log spelunking).".to_string(),
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
    fuzz_runner_contract(None)
}

pub(super) fn fuzz_runner_contract(config: Option<&FuzzConfig>) -> FuzzRunnerContract {
    let mut env: Vec<String> = [
        "HOMEBOY_FUZZ_RESULTS_FILE",
        "HOMEBOY_FUZZ_ARTIFACTS_DIR",
        "HOMEBOY_FUZZ_WORKLOAD_ID",
        "HOMEBOY_FUZZ_WORKLOAD_PATH",
        "HOMEBOY_FUZZ_WORKLOAD_ROOT",
        "HOMEBOY_FUZZ_RUN_ID",
        "HOMEBOY_FUZZ_SEED",
        "HOMEBOY_FUZZ_INVENTORY_FILE",
        "HOMEBOY_FUZZ_MAX_DURATION",
        "HOMEBOY_FUZZ_GATE_PROFILE",
        "HOMEBOY_ARTIFACT_POSTPROCESS_ID",
        "HOMEBOY_ARTIFACT_POSTPROCESS_HELPER",
        "HOMEBOY_ARTIFACT_POSTPROCESS_ACTION",
        "HOMEBOY_ARTIFACT_POSTPROCESS_INPUT",
        "HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT",
        "HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT",
        "HOMEBOY_ARTIFACT_POSTPROCESS_PARAMETERS",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect();

    if let Some(config) = config {
        for key in &config.env {
            let key = key.trim();
            if !key.is_empty() && !env.iter().any(|existing| existing == key) {
                env.push(key.to_string());
            }
        }
    }

    FuzzRunnerContract {
        capability: "fuzz".to_string(),
        extension_script_required: true,
        env,
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
        .timeout(fuzz_max_duration(args.max_duration.as_deref())?)
        .script_args(&args.args);

    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let env = fuzz_runner_env(args, rig_context, workload, &results_path, run_dir)?;
    for (key, value) in env {
        runner = runner.env(&key, &value);
    }

    runner.run()
}

pub(super) fn fuzz_max_duration(raw: Option<&str>) -> homeboy::core::Result<Option<Duration>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let (amount, multiplier) = if let Some(amount) = raw.strip_suffix("ms") {
        (amount, 0)
    } else if let Some(amount) = raw.strip_suffix('s') {
        (amount, 1)
    } else if let Some(amount) = raw.strip_suffix('m') {
        (amount, 60)
    } else if let Some(amount) = raw.strip_suffix('h') {
        (amount, 60 * 60)
    } else {
        (raw, 1)
    };

    let amount = amount.parse::<u64>().map_err(|_| {
        homeboy::core::Error::validation_invalid_argument(
            "max_duration",
            format!(
                "Invalid fuzz max duration '{raw}'. Use a positive duration such as 60s or 5m."
            ),
            Some(raw.to_string()),
            None,
        )
    })?;
    if amount == 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "max_duration",
            "Fuzz max duration must be greater than zero.",
            Some(raw.to_string()),
            None,
        ));
    }

    Ok(Some(if multiplier == 0 {
        Duration::from_millis(amount)
    } else {
        Duration::from_secs(amount.saturating_mul(multiplier))
    }))
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
    let artifacts_dir =
        run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_ARTIFACTS_DIR);
    std::fs::create_dir_all(&artifacts_dir).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(artifacts_dir.display().to_string()),
        )
    })?;
    env.push((
        "HOMEBOY_FUZZ_ARTIFACTS_DIR".to_string(),
        artifacts_dir.to_string_lossy().to_string(),
    ));
    if let Some(workload) = workload {
        env.push(("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), workload.id.clone()));
        if let Some(path) = fuzz_runner_workload_path(workload, rig_context, run_dir)? {
            env.push(("HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(), path.clone()));
        }
    }
    if let Some(package_root) = rig_context.and_then(|context| context.package_root.as_ref()) {
        env.push((
            "HOMEBOY_FUZZ_WORKLOAD_ROOT".to_string(),
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
    env.push((
        "HOMEBOY_FUZZ_GATE_PROFILE".to_string(),
        args.gate_profile.as_str().to_string(),
    ));
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
    if let Ok(path) = crate::core::rig::resolve_component_path(&rig_context.spec, component_id) {
        return path;
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
