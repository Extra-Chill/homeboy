use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use homeboy::core::component::{Component, ScopedExtensionConfig};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::extension::{self, ExtensionCapability, ExtensionRunner};
use homeboy::core::fuzz::{
    default_fuzz_gates, default_fuzz_required_artifacts, fuzz_core_contract,
    merge_fuzz_target_inventory, parse_fuzz_results_file, parse_fuzz_target_inventory_file,
    FuzzCampaign, FuzzExecutionRequest, FuzzGate, FuzzProvenance, FuzzRequiredArtifact,
    FuzzReplayMetadata, FuzzResultEnvelope, FuzzTargetInventory, FUZZ_CAMPAIGN_SCHEMA,
    FUZZ_CONTRACT_VERSION, FUZZ_EXECUTION_REQUEST_SCHEMA, FUZZ_RESULT_ENVELOPE_SCHEMA,
    FUZZ_TARGET_INVENTORY_SCHEMA,
};
use homeboy::core::observation::{ObservationStore, RunRecord, RunStatus};
use homeboy::core::rig::{self, RigSpec};

use super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
    FUZZ_LAB_LABEL,
};

#[derive(Args)]
pub struct FuzzArgs {
    #[command(subcommand)]
    command: Option<FuzzCommand>,

    #[command(flatten)]
    pub run: FuzzRunArgs,
}

impl FuzzArgs {
    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(CommandJsonFamily::Quality, output_file_mode)
    }

    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        self.is_run_invocation()
            .then(|| LabCommandContract::portable_workload(FUZZ_LAB_LABEL, None, true, &[]))
    }

    pub fn is_run_invocation(&self) -> bool {
        matches!(self.command, None | Some(FuzzCommand::Run(_)))
    }

    pub fn extension_override_ids(&self) -> &[String] {
        self.run.extension_override.extensions.as_slice()
    }
}

#[derive(Subcommand)]
enum FuzzCommand {
    /// Print the product-neutral fuzz schema contract
    Contract,
    /// List declared fuzz workloads without executing them
    List(FuzzListArgs),
    /// Build a fuzz execution request without executing it
    Plan(FuzzPlanArgs),
    /// Resolve the selected fuzz workload contract without executing it
    Run(FuzzRunArgs),
    /// Validate a fuzz result campaign file
    Validate(FuzzValidateArgs),
    /// Persist a result envelope from a fuzz campaign file
    Report(FuzzReportArgs),
    /// Reserved replay surface for persisted fuzz cases
    Replay(FuzzReplayArgs),
}

#[derive(Args, Clone)]
struct FuzzListArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Discover workloads using a rig's component path, extension config, and
    /// rig-declared fuzz workloads.
    #[arg(long, value_name = "RIG_ID")]
    rig: Option<String>,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    setting_args: SettingArgs,
}

#[derive(Args, Clone)]
pub struct FuzzRunArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Run against a rig's component path, extension config, and rig-declared
    /// fuzz workloads.
    #[arg(long, value_name = "RIG_ID")]
    rig: Option<String>,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Extension-declared workload id to select.
    #[arg(long = "workload", value_name = "ID")]
    workload_id: Option<String>,

    /// Stable caller-supplied proof label for downstream fuzz runners.
    #[arg(long = "run-id", value_name = "ID")]
    run_id: Option<String>,

    /// Deterministic seed forwarded by future fuzz runners.
    #[arg(long, value_name = "SEED")]
    seed: Option<String>,

    /// Product-neutral fuzz target inventory JSON discovered before execution.
    #[arg(long = "inventory", value_name = "PATH")]
    inventory: Option<PathBuf>,

    /// Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m.
    #[arg(long, value_name = "DURATION")]
    max_duration: Option<String>,

    /// Additional runner arguments reserved for the fuzz extension script.
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args, Clone)]
struct FuzzPlanArgs {
    #[command(flatten)]
    run: FuzzRunArgs,

    /// Stable request id. Defaults to --run-id, then the selected workload id.
    #[arg(long = "request-id", value_name = "ID")]
    request_id: Option<String>,
}

#[derive(Args, Clone)]
struct FuzzValidateArgs {
    /// Fuzz campaign JSON file emitted by a runner.
    #[arg(value_name = "RESULTS_FILE")]
    results_file: PathBuf,
}

#[derive(Args, Clone)]
struct FuzzReportArgs {
    /// Fuzz campaign JSON file emitted by a runner.
    #[arg(value_name = "RESULTS_FILE")]
    results_file: PathBuf,

    #[command(flatten)]
    run: FuzzRunArgs,

    /// Persist the result envelope JSON to this path.
    #[arg(long = "output-envelope", value_name = "PATH")]
    output_envelope: Option<PathBuf>,

    /// Stable envelope id. Defaults to --run-id, then the campaign id.
    #[arg(long = "envelope-id", value_name = "ID")]
    envelope_id: Option<String>,
}

#[derive(Args, Clone)]
struct FuzzReplayArgs {
    /// Fuzz campaign/result envelope path, or a case id when --artifact is used.
    #[arg(value_name = "ARTIFACT_OR_CASE")]
    artifact_or_case: Option<String>,

    /// Fuzz campaign or result envelope artifact to inspect for replay metadata.
    #[arg(long = "artifact", value_name = "PATH")]
    artifact: Option<PathBuf>,

    /// Case id to replay from the campaign/envelope artifact.
    #[arg(long = "case-id", value_name = "ID")]
    case_id: Option<String>,

    /// Stable Homeboy run id associated with the persisted fuzz evidence.
    #[arg(long = "run-id", value_name = "ID")]
    run_id: Option<String>,

    /// Additional runner arguments reserved for future fuzz replay support.
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Serialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum FuzzOutput {
    Contract(FuzzContractOutput),
    List(FuzzListOutput),
    Plan(FuzzPlanOutput),
    Run(FuzzRunOutput),
    Validate(FuzzValidateOutput),
    Report(FuzzReportOutput),
    Replay(FuzzReplayOutput),
}

#[derive(Serialize)]
pub struct FuzzContractOutput {
    pub command: String,
    pub contract: homeboy::core::fuzz::FuzzCoreContract,
    pub required_artifacts: Vec<FuzzRequiredArtifact>,
    pub gates: Vec<FuzzGate>,
}

#[derive(Serialize)]
pub struct FuzzListOutput {
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub workloads: Vec<FuzzWorkloadOutput>,
    pub count: usize,
    pub run_hint: String,
}

#[derive(Serialize)]
pub struct FuzzRunOutput {
    pub kind: String,
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub status: String,
    pub workload_id: Option<String>,
    pub workload_path: Option<String>,
    pub run_id: Option<String>,
    pub seed: Option<String>,
    pub inventory_file: Option<String>,
    pub max_duration: Option<String>,
    pub passthrough_args: Vec<String>,
    pub target_inventory: Option<FuzzTargetInventory>,
    pub execution: Option<FuzzExecutionOutput>,
    pub results: Option<FuzzCampaign>,
    pub runner_contract: FuzzRunnerContract,
    pub evidence_followups: Vec<String>,
}

#[derive(Serialize)]
pub struct FuzzPlanOutput {
    pub command: String,
    pub component: String,
    pub rig_id: Option<String>,
    pub target_inventory: FuzzTargetInventory,
    pub request: FuzzExecutionRequest,
    pub runner_contract: FuzzRunnerContract,
}

#[derive(Serialize)]
pub struct FuzzValidateOutput {
    pub command: String,
    pub status: String,
    pub results_file: String,
    pub campaign_id: String,
    pub open_findings: usize,
    pub artifacts: usize,
    pub coverage_completeness: FuzzCoverageCompletenessOutput,
    pub gates: Vec<FuzzGateEvaluation>,
}

#[derive(Serialize)]
pub struct FuzzReportOutput {
    pub command: String,
    pub status: String,
    pub results_file: String,
    pub envelope_file: Option<String>,
    pub envelope: FuzzResultEnvelope,
    pub coverage_completeness: FuzzCoverageCompletenessOutput,
    pub gates: Vec<FuzzGateEvaluation>,
}

#[derive(Serialize)]
pub struct FuzzReplayOutput {
    pub command: String,
    pub status: String,
    pub message: String,
    pub artifact_file: Option<String>,
    pub campaign_id: Option<String>,
    pub envelope_id: Option<String>,
    pub case_id: Option<String>,
    pub run_id: Option<String>,
    pub replay: Option<FuzzReplayMetadata>,
    pub env: Vec<FuzzReplayEnv>,
    pub passthrough_args: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Serialize)]
pub struct FuzzReplayEnv {
    pub name: String,
    pub value: String,
}

#[derive(Serialize)]
pub struct FuzzExecutionOutput {
    pub kind: String,
    pub extension_id: String,
    pub exit_code: i32,
    pub success: bool,
    pub run_dir: String,
    pub results_file: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FuzzWorkloadOutput {
    pub id: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub source: String,
    pub manifest_path: Option<String>,
}

#[derive(Serialize)]
pub struct FuzzRunnerContract {
    pub capability: String,
    pub extension_script_required: bool,
    pub env: Vec<&'static str>,
}

#[derive(Serialize, Clone)]
pub struct FuzzGateEvaluation {
    pub gate_id: String,
    pub status: String,
    pub metric: String,
    pub observed: f64,
    pub expected: f64,
}

#[derive(Serialize, Clone)]
pub struct FuzzCoverageCompletenessOutput {
    pub has_summary: bool,
    pub declared_targets: u64,
    pub executable_targets: u64,
    pub proven_targets: u64,
    pub target_coverage_ratio: f64,
    pub declared_operations: u64,
    pub executable_operations: u64,
    pub proven_operations: u64,
    pub operation_coverage_ratio: f64,
    pub skipped_targets: usize,
    pub skipped_operations: usize,
    pub artifact_ids: Vec<String>,
}

pub fn run(args: FuzzArgs, _global: &GlobalArgs) -> CmdResult<FuzzOutput> {
    match args.command {
        Some(FuzzCommand::Contract) => Ok((FuzzOutput::Contract(run_contract()), 0)),
        Some(FuzzCommand::List(list_args)) => Ok((FuzzOutput::List(run_list(list_args)?), 0)),
        Some(FuzzCommand::Plan(plan_args)) => Ok((FuzzOutput::Plan(run_plan(plan_args)?), 0)),
        Some(FuzzCommand::Run(run_args)) => {
            let (output, exit) = run_run(run_args)?;
            Ok((FuzzOutput::Run(output), exit))
        }
        Some(FuzzCommand::Validate(validate_args)) => {
            Ok((FuzzOutput::Validate(run_validate(validate_args)?), 0))
        }
        Some(FuzzCommand::Report(report_args)) => {
            Ok((FuzzOutput::Report(run_report(report_args)?), 0))
        }
        Some(FuzzCommand::Replay(replay_args)) => {
            Ok((FuzzOutput::Replay(run_replay(replay_args)?), 0))
        }
        None => {
            let (output, exit) = run_run(args.run)?;
            Ok((FuzzOutput::Run(output), exit))
        }
    }
}

fn run_contract() -> FuzzContractOutput {
    FuzzContractOutput {
        command: "fuzz.contract".to_string(),
        contract: fuzz_core_contract(),
        required_artifacts: default_fuzz_required_artifacts(),
        gates: default_fuzz_gates(),
    }
}

fn run_replay(args: FuzzReplayArgs) -> homeboy::core::Result<FuzzReplayOutput> {
    let artifact_file = replay_artifact_path(&args);
    let positional_case = args.artifact_or_case.as_ref().and_then(|value| {
        if artifact_file.is_some() && !Path::new(value).exists() {
            Some(value.clone())
        } else {
            None
        }
    });
    let requested_case_id = args.case_id.clone().or(positional_case);

    let resolved = if let Some(path) = artifact_file.as_ref() {
        Some(resolve_replay_artifact(path, requested_case_id.as_deref())?)
    } else {
        None
    };
    let case_id = resolved
        .as_ref()
        .and_then(|resolved| resolved.case_id.clone())
        .or(requested_case_id);
    let replay = resolved
        .as_ref()
        .and_then(|resolved| resolved.replay.clone());
    let env = fuzz_replay_env(
        artifact_file.as_ref(),
        case_id.as_deref(),
        replay.as_ref(),
        args.run_id.as_ref(),
    );

    let status = if artifact_file.is_some() {
        "dry_run"
    } else {
        "needs_artifact"
    };

    Ok(FuzzReplayOutput {
        command: "fuzz.replay".to_string(),
        status: status.to_string(),
        message: "Generic fuzz replay resolves replay metadata and prints the extension-owned execution contract; it does not execute local fuzz code without a component/extension context."
            .to_string(),
        artifact_file: artifact_file.map(|path| path.to_string_lossy().to_string()),
        campaign_id: resolved.as_ref().and_then(|resolved| resolved.campaign_id.clone()),
        envelope_id: resolved.as_ref().and_then(|resolved| resolved.envelope_id.clone()),
        case_id,
        run_id: args.run_id,
        replay,
        env,
        passthrough_args: args.args,
        next_steps: vec![
            "Pass the reported HOMEBOY_FUZZ_REPLAY_* values to the originating extension replay runner."
                .to_string(),
            "Use `homeboy runs artifacts <run-id>` to locate persisted fuzz evidence when a runner records it."
                .to_string(),
        ],
    })
}

#[derive(Clone, Debug)]
struct ResolvedReplayArtifact {
    campaign_id: Option<String>,
    envelope_id: Option<String>,
    case_id: Option<String>,
    replay: Option<FuzzReplayMetadata>,
}

fn replay_artifact_path(args: &FuzzReplayArgs) -> Option<PathBuf> {
    args.artifact.clone().or_else(|| {
        args.artifact_or_case.as_ref().and_then(|value| {
            let path = PathBuf::from(value);
            (path.exists() || value.contains(std::path::MAIN_SEPARATOR) || value.ends_with(".json"))
                .then_some(path)
        })
    })
}

fn resolve_replay_artifact(
    path: &Path,
    requested_case_id: Option<&str>,
) -> homeboy::core::Result<ResolvedReplayArtifact> {
    let contents = std::fs::read_to_string(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    let value: serde_json::Value = serde_json::from_str(&contents).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some(format!("parse fuzz replay artifact {}", path.display())),
            Some(contents.clone()),
        )
    })?;
    let schema = value
        .get("schema")
        .and_then(|schema| schema.as_str())
        .unwrap_or_default();

    if schema == FUZZ_RESULT_ENVELOPE_SCHEMA {
        let envelope: FuzzResultEnvelope = serde_json::from_value(value).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact",
                format!("failed to decode fuzz result envelope: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
        let campaign = envelope.campaign.as_ref();
        let (case_id, replay) = resolve_replay_metadata(campaign, requested_case_id)?;
        return Ok(ResolvedReplayArtifact {
            campaign_id: campaign.map(|campaign| campaign.id.clone()),
            envelope_id: Some(envelope.id),
            case_id,
            replay,
        });
    }

    if schema == FUZZ_CAMPAIGN_SCHEMA {
        let campaign: FuzzCampaign = serde_json::from_value(value).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact",
                format!("failed to decode fuzz campaign: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
        let (case_id, replay) = resolve_replay_metadata(Some(&campaign), requested_case_id)?;
        return Ok(ResolvedReplayArtifact {
            campaign_id: Some(campaign.id),
            envelope_id: None,
            case_id,
            replay,
        });
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "artifact",
        format!(
            "fuzz replay artifact schema must be {FUZZ_CAMPAIGN_SCHEMA} or {FUZZ_RESULT_ENVELOPE_SCHEMA}, got {schema}"
        ),
        Some(path.display().to_string()),
        None,
    ))
}

fn resolve_replay_metadata(
    campaign: Option<&FuzzCampaign>,
    requested_case_id: Option<&str>,
) -> homeboy::core::Result<(Option<String>, Option<FuzzReplayMetadata>)> {
    let Some(campaign) = campaign else {
        return Ok((requested_case_id.map(str::to_string), None));
    };

    if let Some(case_id) = requested_case_id {
        let case = campaign
            .cases
            .iter()
            .find(|case| case.id == case_id)
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "case-id",
                    format!(
                        "fuzz campaign '{}' does not contain case '{case_id}'",
                        campaign.id
                    ),
                    Some(case_id.to_string()),
                    None,
                )
            })?;
        let replay = case
            .replay_id
            .as_ref()
            .and_then(|replay_id| {
                campaign
                    .replay
                    .as_ref()
                    .filter(|replay| replay.id == *replay_id)
            })
            .cloned()
            .or_else(|| campaign.replay.clone());
        return Ok((Some(case.id.clone()), replay));
    }

    if campaign.cases.len() == 1 {
        let case = &campaign.cases[0];
        let replay = case
            .replay_id
            .as_ref()
            .and_then(|replay_id| {
                campaign
                    .replay
                    .as_ref()
                    .filter(|replay| replay.id == *replay_id)
            })
            .cloned()
            .or_else(|| campaign.replay.clone());
        return Ok((Some(case.id.clone()), replay));
    }

    Ok((None, campaign.replay.clone()))
}

fn fuzz_replay_env(
    artifact_file: Option<&PathBuf>,
    case_id: Option<&str>,
    replay: Option<&FuzzReplayMetadata>,
    run_id: Option<&String>,
) -> Vec<FuzzReplayEnv> {
    let mut env = Vec::new();
    if let Some(path) = artifact_file {
        push_replay_env(
            &mut env,
            "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE",
            path.to_string_lossy().to_string(),
        );
    }
    if let Some(case_id) = case_id.filter(|case_id| !case_id.trim().is_empty()) {
        push_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_CASE_ID", case_id.to_string());
    }
    if let Some(run_id) = run_id.filter(|run_id| !run_id.trim().is_empty()) {
        push_replay_env(&mut env, "HOMEBOY_FUZZ_RUN_ID", run_id.clone());
    }
    if let Some(replay) = replay {
        push_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_ID", replay.id.clone());
        push_opt_replay_env(&mut env, "HOMEBOY_FUZZ_REPLAY_SEED", replay.seed.as_ref());
        push_opt_replay_env(
            &mut env,
            "HOMEBOY_FUZZ_REPLAY_ARTIFACT_ID",
            replay.artifact_id.as_ref(),
        );
    }
    env
}

fn push_replay_env(env: &mut Vec<FuzzReplayEnv>, name: &str, value: String) {
    env.push(FuzzReplayEnv {
        name: name.to_string(),
        value,
    });
}

fn push_opt_replay_env(env: &mut Vec<FuzzReplayEnv>, name: &str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        push_replay_env(env, name, value.clone());
    }
}

fn run_list(args: FuzzListArgs) -> homeboy::core::Result<FuzzListOutput> {
    let rig_context = load_rig(args.rig.as_deref())?;
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
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );

    Ok(FuzzListOutput {
        command: "fuzz.list".to_string(),
        component: ctx.component_id,
        rig_id: rig_context.map(|context| context.spec.id),
        count: workloads.len(),
        workloads,
        run_hint: "Select one workload with `homeboy fuzz run <component> --workload <id>`; offload heavy campaigns with the global `--runner <id>` flag when configured.".to_string(),
    })
}

fn run_plan(args: FuzzPlanArgs) -> homeboy::core::Result<FuzzPlanOutput> {
    let rig_context = load_rig(args.run.rig.as_deref())?;
    let effective_id = resolve_component_id(
        &args.run.comp,
        rig_context.as_ref().map(|context| &context.spec),
    )?;
    let ctx = resolve_fuzz_context(
        &effective_id,
        &args.run.comp,
        &args.run.setting_args,
        &args.run.extension_override,
        ExtensionCapability::Fuzz,
        rig_context.as_ref(),
    )?;
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );
    let selected_workload = select_workload(&workloads, args.run.workload_id.as_deref())?;
    let workload_id = selected_workload
        .map(|workload| workload.id.clone())
        .or_else(|| args.run.workload_id.clone());
    let required_artifacts = default_fuzz_required_artifacts();
    let gates = default_fuzz_gates();
    let request_id = args
        .request_id
        .clone()
        .or_else(|| args.run.run_id.clone())
        .or_else(|| workload_id.clone())
        .unwrap_or_else(|| format!("{}-fuzz-request", ctx.component_id));
    let rig_id = rig_context.as_ref().map(|context| context.spec.id.clone());

    let target_inventory = build_target_inventory(
        &ctx.component_id,
        &workloads,
        args.run.run_id.clone(),
        args.run.inventory.as_deref(),
    )?;

    Ok(FuzzPlanOutput {
        command: "fuzz.plan".to_string(),
        component: ctx.component_id.clone(),
        rig_id: rig_id.clone(),
        target_inventory,
        request: FuzzExecutionRequest {
            schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: request_id,
            component: ctx.component_id,
            rig_id,
            workload_id,
            case_ids: Vec::new(),
            seed: args.run.seed,
            max_duration: args.run.max_duration,
            args: args.run.args,
            required_artifacts,
            gates,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        },
        runner_contract: default_runner_contract(),
    })
}

fn run_validate(args: FuzzValidateArgs) -> homeboy::core::Result<FuzzValidateOutput> {
    let campaign = parse_fuzz_results_file(&args.results_file)?;
    let gates = evaluate_fuzz_gates(&campaign);
    let coverage_completeness = fuzz_coverage_completeness(&campaign);

    Ok(FuzzValidateOutput {
        command: "fuzz.validate".to_string(),
        status: gate_status(&gates),
        results_file: args.results_file.to_string_lossy().to_string(),
        campaign_id: campaign.id.clone(),
        open_findings: open_finding_count(&campaign),
        artifacts: campaign.artifacts.len(),
        coverage_completeness,
        gates,
    })
}

fn run_report(args: FuzzReportArgs) -> homeboy::core::Result<FuzzReportOutput> {
    let campaign = parse_fuzz_results_file(&args.results_file)?;
    let gates = evaluate_fuzz_gates(&campaign);
    let coverage_completeness = fuzz_coverage_completeness(&campaign);
    let status = gate_status(&gates);
    let run_id = args.run.run_id.clone();
    let component = args.run.comp.id().unwrap_or("unknown").to_string();
    let request_id = args
        .run
        .run_id
        .clone()
        .or_else(|| args.run.workload_id.clone())
        .unwrap_or_else(|| format!("{}-request", campaign.id));
    let envelope_id = args
        .envelope_id
        .clone()
        .or_else(|| args.run.run_id.clone())
        .unwrap_or_else(|| campaign.id.clone());
    let metadata = fuzz_result_metadata(args.run.inventory.as_deref())?;
    let envelope = FuzzResultEnvelope {
        schema: FUZZ_RESULT_ENVELOPE_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: envelope_id,
        status: status.clone(),
        request: FuzzExecutionRequest {
            schema: FUZZ_EXECUTION_REQUEST_SCHEMA.to_string(),
            version: FUZZ_CONTRACT_VERSION,
            id: request_id,
            component,
            rig_id: args.run.rig,
            workload_id: args.run.workload_id,
            case_ids: campaign.cases.iter().map(|case| case.id.clone()).collect(),
            seed: args.run.seed,
            max_duration: args.run.max_duration,
            args: args.run.args,
            required_artifacts: default_fuzz_required_artifacts(),
            gates: default_fuzz_gates(),
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        },
        campaign: Some(campaign.clone()),
        artifacts: campaign.artifacts.clone(),
        required_artifacts: default_fuzz_required_artifacts(),
        gates: default_fuzz_gates(),
        provenance: Some(fuzz_provenance(run_id)),
        metadata,
        extra: std::collections::BTreeMap::new(),
    };

    if let Some(path) = args.output_envelope.as_ref() {
        let json = serde_json::to_string_pretty(&envelope).map_err(|error| {
            homeboy::core::Error::internal_unexpected(format!(
                "failed to encode fuzz result envelope: {error}"
            ))
        })?;
        std::fs::write(path, json).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
    }

    Ok(FuzzReportOutput {
        command: "fuzz.report".to_string(),
        status,
        results_file: args.results_file.to_string_lossy().to_string(),
        envelope_file: args
            .output_envelope
            .map(|path| path.to_string_lossy().to_string()),
        envelope,
        coverage_completeness,
        gates,
    })
}

fn run_run(args: FuzzRunArgs) -> homeboy::core::Result<(FuzzRunOutput, i32)> {
    let rig_context = load_rig(args.rig.as_deref())?;
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
    let workloads = fuzz_workloads(
        &ctx.component,
        rig_context.as_ref(),
        ctx.extension_id.as_deref(),
    );
    let selected_workload = select_workload(&workloads, args.workload_id.as_deref())?;
    let target_inventory = build_target_inventory(
        &ctx.component_id,
        &workloads,
        args.run_id.clone(),
        args.inventory.as_deref(),
    )?;
    let run_dir = RunDir::create()?;
    let runner_output = run_fuzz_extension_script(&ctx, &args, selected_workload, &run_dir)?;
    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let results = if results_path.exists() {
        Some(parse_fuzz_results_file(&results_path)?)
    } else {
        None
    };
    let exit_code = runner_output.exit_code;
    let success = runner_output.success;
    let status = if success { "passed" } else { "failed" }.to_string();
    let rig_id = rig_context.map(|context| context.spec.id);
    let workload_id = selected_workload
        .map(|workload| workload.id.clone())
        .or_else(|| args.workload_id.clone());
    let workload_path = selected_workload.and_then(|workload| workload.manifest_path.clone());
    persist_fuzz_run_evidence(
        args.run_id.as_deref(),
        &ctx.component_id,
        rig_id.as_deref(),
        workload_id.as_deref(),
        workload_path.as_deref(),
        &status,
        exit_code,
        success,
        &args,
        &results_path,
        results.as_ref(),
    )?;
    let evidence_followups = fuzz_evidence_followups(args.run_id.as_deref());

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
            runner_contract: default_runner_contract(),
            evidence_followups,
        },
        exit_code,
    ))
}

fn persist_fuzz_run_evidence(
    run_id: Option<&str>,
    component_id: &str,
    rig_id: Option<&str>,
    workload_id: Option<&str>,
    workload_path: Option<&str>,
    status: &str,
    exit_code: i32,
    success: bool,
    args: &FuzzRunArgs,
    results_path: &Path,
    results: Option<&FuzzCampaign>,
) -> homeboy::core::Result<Option<RunRecord>> {
    let Some(run_id) = run_id.filter(|run_id| !run_id.trim().is_empty()) else {
        return Ok(None);
    };
    let store = ObservationStore::open_initialized()?;
    let now = chrono::Utc::now().to_rfc3339();
    let metadata = serde_json::json!({
        "source": "homeboy fuzz run",
        "workload_id": workload_id,
        "workload_path": workload_path,
        "seed": args.seed.clone(),
        "max_duration": args.max_duration.clone(),
        "passthrough_args": args.args.clone(),
        "exit_code": exit_code,
        "success": success,
        "status": status,
        "campaign_id": results.map(|campaign| campaign.id.as_str()),
        "coverage_completeness": results.map(fuzz_coverage_completeness),
        "gates": results.map(evaluate_fuzz_gates),
    });
    let run = RunRecord {
        id: run_id.to_string(),
        kind: "fuzz".to_string(),
        component_id: Some(component_id.to_string()),
        started_at: now.clone(),
        finished_at: Some(now),
        status: if success {
            RunStatus::Pass.as_str().to_string()
        } else {
            RunStatus::Fail.as_str().to_string()
        },
        command: Some(fuzz_run_command(component_id, rig_id, workload_id, args)),
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().to_string()),
        homeboy_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        git_sha: None,
        rig_id: rig_id.map(str::to_string),
        metadata_json: metadata,
    };
    store.upsert_imported_run(&run)?;
    if results_path.is_file() {
        store.record_artifact(run_id, "fuzz_results", results_path)?;
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

fn fuzz_evidence_followups(run_id: Option<&str>) -> Vec<String> {
    match run_id.filter(|run_id| !run_id.trim().is_empty()) {
        Some(run_id) => vec![
            format!("homeboy runs show {run_id}"),
            format!("homeboy runs evidence {run_id}"),
            format!("homeboy runs artifacts {run_id}"),
        ],
        None => vec![
            "Use --run-id <stable-id> when the downstream runner records persisted Homeboy evidence.".to_string(),
            "Inspect persisted proof with `homeboy runs show <run-id>` and `homeboy runs evidence <run-id>`.".to_string(),
        ],
    }
}

fn default_runner_contract() -> FuzzRunnerContract {
    FuzzRunnerContract {
        capability: "fuzz".to_string(),
        extension_script_required: true,
        env: vec![
            "HOMEBOY_FUZZ_RESULTS_FILE",
            "HOMEBOY_FUZZ_WORKLOAD_ID",
            "HOMEBOY_FUZZ_WORKLOAD_PATH",
            "HOMEBOY_FUZZ_RUN_ID",
            "HOMEBOY_FUZZ_SEED",
            "HOMEBOY_FUZZ_INVENTORY_FILE",
            "HOMEBOY_FUZZ_MAX_DURATION",
        ],
    }
}

fn build_target_inventory(
    component_id: &str,
    workloads: &[FuzzWorkloadOutput],
    run_id: Option<String>,
    inventory_path: Option<&Path>,
) -> homeboy::core::Result<FuzzTargetInventory> {
    let mut inventory = FuzzTargetInventory {
        schema: FUZZ_TARGET_INVENTORY_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: format!("{}-inventory", component_id),
        surfaces: Vec::new(),
        targets: Vec::new(),
        workloads: Vec::new(),
        seeds: Vec::new(),
        provenance: Some(fuzz_provenance(run_id)),
        metadata: serde_json::json!({
            "declared_workloads": workloads,
        }),
        extra: std::collections::BTreeMap::new(),
    };

    if let Some(path) = inventory_path {
        let discovered = parse_fuzz_target_inventory_file(path)?;
        inventory.metadata["inventory_file"] =
            serde_json::Value::String(path.to_string_lossy().to_string());
        merge_fuzz_target_inventory(&mut inventory, discovered);
    }

    Ok(inventory)
}

fn fuzz_result_metadata(inventory_path: Option<&Path>) -> homeboy::core::Result<serde_json::Value> {
    let Some(path) = inventory_path else {
        return Ok(serde_json::Value::Null);
    };
    let inventory = parse_fuzz_target_inventory_file(path)?;
    Ok(serde_json::json!({
        "inventory_file": path.to_string_lossy(),
        "target_inventory": inventory,
    }))
}

fn fuzz_provenance(run_id: Option<String>) -> FuzzProvenance {
    FuzzProvenance {
        schema: homeboy::core::fuzz::FUZZ_PROVENANCE_SCHEMA.to_string(),
        producer: "homeboy fuzz".to_string(),
        producer_version: None,
        invocation: None,
        run_id,
        source_ref: None,
        created_at: None,
        metadata: serde_json::Value::Null,
        extra: std::collections::BTreeMap::new(),
    }
}

fn evaluate_fuzz_gates(campaign: &FuzzCampaign) -> Vec<FuzzGateEvaluation> {
    default_fuzz_gates()
        .into_iter()
        .map(|gate| {
            let observed = match gate.metric.as_str() {
                "open_findings" => open_finding_count(campaign) as f64,
                "case_log_artifacts" => campaign
                    .artifacts
                    .iter()
                    .filter(|artifact| artifact.kind == "case_log")
                    .count() as f64,
                "target_coverage_ratio" => coverage_ratio(
                    campaign.coverage_summary.as_ref(),
                    |summary| summary.proven_targets,
                    |summary| summary.declared_targets,
                ),
                "operation_coverage_ratio" => coverage_ratio(
                    campaign.coverage_summary.as_ref(),
                    |summary| summary.proven_operations,
                    |summary| summary.declared_operations,
                ),
                _ => 0.0,
            };
            let passed = threshold_passes(observed, gate.operator, gate.value);
            FuzzGateEvaluation {
                gate_id: gate.id,
                status: if passed { "passed" } else { "failed" }.to_string(),
                metric: gate.metric,
                observed,
                expected: gate.value,
            }
        })
        .collect()
}

fn coverage_ratio(
    summary: Option<&homeboy::core::fuzz::FuzzCoverageSummary>,
    covered: impl Fn(&homeboy::core::fuzz::FuzzCoverageSummary) -> u64,
    total: impl Fn(&homeboy::core::fuzz::FuzzCoverageSummary) -> u64,
) -> f64 {
    let Some(summary) = summary else {
        return 0.0;
    };
    let total = total(summary);
    if total == 0 {
        1.0
    } else {
        covered(summary) as f64 / total as f64
    }
}

fn fuzz_coverage_completeness(campaign: &FuzzCampaign) -> FuzzCoverageCompletenessOutput {
    match campaign.coverage_summary.as_ref() {
        Some(summary) => FuzzCoverageCompletenessOutput {
            has_summary: true,
            declared_targets: summary.declared_targets,
            executable_targets: summary.executable_targets,
            proven_targets: summary.proven_targets,
            target_coverage_ratio: coverage_ratio(
                Some(summary),
                |summary| summary.proven_targets,
                |summary| summary.declared_targets,
            ),
            declared_operations: summary.declared_operations,
            executable_operations: summary.executable_operations,
            proven_operations: summary.proven_operations,
            operation_coverage_ratio: coverage_ratio(
                Some(summary),
                |summary| summary.proven_operations,
                |summary| summary.declared_operations,
            ),
            skipped_targets: summary.skipped_targets.len(),
            skipped_operations: summary.skipped_operations.len(),
            artifact_ids: summary.artifact_ids.clone(),
        },
        None => FuzzCoverageCompletenessOutput {
            has_summary: false,
            declared_targets: 0,
            executable_targets: 0,
            proven_targets: 0,
            target_coverage_ratio: 0.0,
            declared_operations: 0,
            executable_operations: 0,
            proven_operations: 0,
            operation_coverage_ratio: 0.0,
            skipped_targets: 0,
            skipped_operations: 0,
            artifact_ids: Vec::new(),
        },
    }
}

fn open_finding_count(campaign: &FuzzCampaign) -> usize {
    campaign
        .findings
        .iter()
        .filter(|finding| finding.status == homeboy::core::fuzz::FuzzFindingStatus::Open)
        .count()
}

fn gate_status(gates: &[FuzzGateEvaluation]) -> String {
    if gates.iter().all(|gate| gate.status == "passed") {
        "passed".to_string()
    } else {
        "failed".to_string()
    }
}

fn threshold_passes(
    observed: f64,
    operator: homeboy::core::fuzz::FuzzThresholdOperator,
    expected: f64,
) -> bool {
    match operator {
        homeboy::core::fuzz::FuzzThresholdOperator::GreaterThan => observed > expected,
        homeboy::core::fuzz::FuzzThresholdOperator::GreaterThanOrEqual => observed >= expected,
        homeboy::core::fuzz::FuzzThresholdOperator::LessThan => observed < expected,
        homeboy::core::fuzz::FuzzThresholdOperator::LessThanOrEqual => observed <= expected,
        homeboy::core::fuzz::FuzzThresholdOperator::Equal => {
            (observed - expected).abs() < f64::EPSILON
        }
    }
}

fn run_fuzz_extension_script(
    ctx: &execution_context::ExecutionContext,
    args: &FuzzRunArgs,
    workload: Option<&FuzzWorkloadOutput>,
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
        .script_args(&args.args);

    let results_path = run_dir.step_file(homeboy::core::engine::run_dir::files::FUZZ_RESULTS);
    let env = fuzz_runner_env(args, workload, &results_path);
    for (key, value) in env {
        runner = runner.env(&key, &value);
    }

    runner.run()
}

fn fuzz_runner_env(
    args: &FuzzRunArgs,
    workload: Option<&FuzzWorkloadOutput>,
    results_path: &Path,
) -> Vec<(String, String)> {
    let mut env = vec![(
        "HOMEBOY_FUZZ_RESULTS_FILE".to_string(),
        results_path.to_string_lossy().to_string(),
    )];
    if let Some(workload) = workload {
        env.push(("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), workload.id.clone()));
        if let Some(path) = workload.manifest_path.as_ref() {
            env.push(("HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(), path.clone()));
        }
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
    env
}

fn push_opt_env(env: &mut Vec<(String, String)>, key: &str, value: Option<&String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        env.push((key.to_string(), value.clone()));
    }
}

type FuzzRigContext = rig::RigSourceContext;

fn load_rig(rig_id: Option<&str>) -> homeboy::core::Result<Option<FuzzRigContext>> {
    let Some(rig_id) = rig_id else {
        return Ok(None);
    };
    Ok(Some(rig::RigSourceContext::load(rig_id)?))
}

fn resolve_component_id(
    comp: &PositionalComponentArgs,
    rig_spec: Option<&RigSpec>,
) -> homeboy::core::Result<String> {
    if let Some(id) = comp.id() {
        return Ok(id.to_string());
    }

    if let Some(spec) = rig_spec {
        if let Some(default) = spec
            .fuzz
            .as_ref()
            .and_then(|fuzz| fuzz.default_component.as_deref())
        {
            return Ok(default.to_string());
        }

        return Err(homeboy::core::Error::validation_invalid_argument(
            "fuzz.default_component",
            format!(
                "rig '{}' does not declare fuzz.default_component; pass a component id or add fuzz.default_component to the rig spec",
                spec.id
            ),
            None,
            None,
        ));
    }

    comp.resolve_id()
}

fn resolve_fuzz_context(
    component_id: &str,
    comp: &PositionalComponentArgs,
    settings: &SettingArgs,
    extension_override: &ExtensionOverrideArgs,
    capability: ExtensionCapability,
    rig_context: Option<&FuzzRigContext>,
) -> homeboy::core::Result<execution_context::ExecutionContext> {
    let rig_spec = rig_context.map(|context| &context.spec);
    let path_override = comp
        .path
        .clone()
        .or_else(|| rig_spec.and_then(|spec| rig_component_path(spec, component_id)));
    let component_override = rig_spec.and_then(|spec| rig_component_for_fuzz(spec, component_id));

    let mut resolve_options = ResolveOptions::with_capability_and_json(
        component_id,
        path_override,
        capability,
        settings.setting.clone(),
        settings.setting_json.clone(),
    );
    resolve_options.extension_overrides = extension_override.extensions.clone();

    execution_context::resolve_with_component(&resolve_options, component_override)
}

fn rig_component_path(spec: &RigSpec, component_id: &str) -> Option<String> {
    spec.components
        .get(component_id)
        .map(|component| rig::expand::expand_vars(spec, &component.path))
}

fn rig_component_for_fuzz(spec: &RigSpec, component_id: &str) -> Option<Component> {
    let rig_component = spec.components.get(component_id)?;
    let mut extensions = rig_component.extensions.clone()?;
    expand_rig_extension_settings(spec, &mut extensions);
    let mut component = Component {
        id: component_id.to_string(),
        local_path: rig::expand::expand_vars(spec, &rig_component.path),
        remote_url: rig_component.remote_url.clone(),
        extensions: Some(extensions),
        ..Component::default()
    };
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
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                expand_rig_setting_value(spec, value);
            }
        }
        _ => {}
    }
}

fn fuzz_workloads(
    component: &homeboy::core::component::Component,
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
) -> Vec<FuzzWorkloadOutput> {
    let mut workloads: Vec<FuzzWorkloadOutput> = component
        .script_commands(ExtensionCapability::Fuzz)
        .iter()
        .enumerate()
        .map(|(index, _command)| FuzzWorkloadOutput {
            id: format!("component-script-{}", index + 1),
            label: None,
            description: None,
            source: "component.scripts.fuzz".to_string(),
            manifest_path: None,
        })
        .collect();

    if let (Some(context), Some(extension_id)) = (rig_context, extension_id) {
        workloads.extend(
            rig::workload_path_expansions_for_extension(
                &context.spec,
                rig::RigWorkloadKind::Fuzz,
                context.package_root.as_deref(),
                extension_id,
            )
            .into_iter()
            .map(|expansion| fuzz_workload_from_path(extension_id, &expansion.expanded_path)),
        );
    }

    if let Some(extensions) = component.extensions.as_ref() {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                workloads.extend(manifest.fuzz_workloads().iter().map(|workload| {
                    FuzzWorkloadOutput {
                        id: workload.id.clone(),
                        label: workload.label.clone(),
                        description: workload.description.clone(),
                        source: format!("extension:{extension_id}"),
                        manifest_path: None,
                    }
                }));
            }
        }
    }

    workloads
}

fn fuzz_workload_from_path(extension_id: &str, path: &Path) -> FuzzWorkloadOutput {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("rig-fuzz-workload")
        .to_string();
    FuzzWorkloadOutput {
        id,
        label: path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        description: None,
        source: format!("rig_workloads:{extension_id}:{}", path.to_string_lossy()),
        manifest_path: Some(path.to_string_lossy().to_string()),
    }
}

fn select_workload<'a>(
    workloads: &'a [FuzzWorkloadOutput],
    workload_id: Option<&str>,
) -> homeboy::core::Result<Option<&'a FuzzWorkloadOutput>> {
    if let Some(workload_id) = workload_id {
        return workloads
            .iter()
            .find(|workload| workload.id == workload_id)
            .map(Some)
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "workload",
                    format!("Unknown fuzz workload '{workload_id}'. Run `homeboy fuzz list` to inspect declared workloads."),
                    None,
                    None,
                )
            });
    }

    if workloads.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workload",
            "No fuzz workloads are declared for this component/rig/extension selection",
            None,
            Some(vec![
                "Run `homeboy fuzz list <component> --rig <id>` to inspect the resolved selection.".to_string(),
                "Declare extension fuzz workloads, component scripts.fuzz commands, or rig fuzz_workloads before claiming fuzz coverage.".to_string(),
                "If the command is available in source but not on the Lab runner, run `homeboy lab status --runner <id>` and refresh or upgrade the runner binary.".to_string(),
            ]),
        ));
    }

    let mut path_workloads = workloads
        .iter()
        .filter(|workload| workload.manifest_path.is_some());
    let first = path_workloads.next();
    if first.is_some() && path_workloads.next().is_none() {
        return Ok(first);
    }

    if workloads.len() > 1 {
        let workload_ids = workloads
            .iter()
            .map(|workload| workload.id.clone())
            .collect::<Vec<_>>();
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workload",
            "Multiple fuzz workloads are declared; select one explicitly with --workload <id>",
            None,
            Some(vec![
                format!("Available workload ids: {}", workload_ids.join(", ")),
                "Run `homeboy fuzz list` for labels, descriptions, sources, and manifest paths."
                    .to_string(),
            ]),
        ));
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use homeboy::test_support::with_isolated_home;

    #[derive(Parser)]
    struct FuzzCli {
        #[command(flatten)]
        args: FuzzArgs,
    }

    #[test]
    fn fuzz_run_parses_generic_contract_flags() {
        let cli = FuzzCli::parse_from([
            "fuzz",
            "run",
            "component-a",
            "--rig",
            "package-fuzz",
            "--workload",
            "parser",
            "--run-id",
            "proof-1",
            "--seed",
            "1234",
            "--inventory",
            "/tmp/fuzz-inventory.json",
            "--max-duration",
            "60s",
            "--",
            "--engine",
            "libfuzzer",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Run(run)) => {
                assert_eq!(run.comp.component.as_deref(), Some("component-a"));
                assert_eq!(run.rig.as_deref(), Some("package-fuzz"));
                assert_eq!(run.workload_id.as_deref(), Some("parser"));
                assert_eq!(run.run_id.as_deref(), Some("proof-1"));
                assert_eq!(run.seed.as_deref(), Some("1234"));
                assert_eq!(
                    run.inventory.as_deref(),
                    Some(Path::new("/tmp/fuzz-inventory.json"))
                );
                assert_eq!(run.max_duration.as_deref(), Some("60s"));
                assert_eq!(run.args, vec!["--engine", "libfuzzer"]);
            }
            _ => panic!("expected fuzz run command"),
        }
    }

    #[test]
    fn fuzz_output_contract_has_stable_variant_discriminators() {
        let contract = serde_json::to_value(FuzzOutput::Contract(run_contract())).unwrap();
        assert_eq!(contract["variant"], "contract");
        assert_eq!(
            contract["contract"]["schemas"]["result_envelope"],
            homeboy::core::fuzz::FUZZ_RESULT_ENVELOPE_SCHEMA
        );

        let list = serde_json::to_value(FuzzOutput::List(FuzzListOutput {
            command: "fuzz.list".to_string(),
            component: "component-a".to_string(),
            rig_id: None,
            workloads: Vec::new(),
            count: 0,
            run_hint: "hint".to_string(),
        }))
        .unwrap();
        assert_eq!(list["variant"], "list");

        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            rig_id: Some("package-fuzz".to_string()),
            status: "passed".to_string(),
            workload_id: Some("parser".to_string()),
            workload_path: None,
            run_id: None,
            seed: None,
            inventory_file: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            target_inventory: None,
            execution: None,
            results: None,
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: Vec::new(),
            },
            evidence_followups: Vec::new(),
        }))
        .unwrap();
        assert_eq!(run["variant"], "run");
        assert_eq!(run["kind"], "fuzz");
        assert_eq!(run["rig_id"], "package-fuzz");
    }

    #[test]
    fn fuzz_gate_evaluation_requires_case_log_evidence() {
        let campaign = FuzzCampaign {
            schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
            id: "campaign-1".to_string(),
            title: None,
            safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: Vec::new(),
            workloads: Vec::new(),
            cases: Vec::new(),
            seeds: Vec::new(),
            coverage: Vec::new(),
            coverage_summary: None,
            findings: Vec::new(),
            artifacts: Vec::new(),
            thresholds: Vec::new(),
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let gates = evaluate_fuzz_gates(&campaign);

        assert_eq!(gate_status(&gates), "failed");
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "has-case-evidence" && gate.status == "failed" && gate.observed == 0.0
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "target-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.0
        }));
    }

    #[test]
    fn fuzz_gate_evaluation_requires_complete_target_and_operation_coverage() {
        let mut campaign = FuzzCampaign {
            schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
            id: "campaign-1".to_string(),
            title: None,
            safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: Vec::new(),
            workloads: Vec::new(),
            cases: Vec::new(),
            seeds: Vec::new(),
            coverage: Vec::new(),
            coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
                schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
                declared_targets: 2,
                executable_targets: 2,
                proven_targets: 1,
                declared_operations: 4,
                executable_operations: 4,
                proven_operations: 4,
                skipped_targets: Vec::new(),
                skipped_operations: Vec::new(),
                artifact_ids: vec!["coverage-report".to_string()],
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }),
            findings: Vec::new(),
            artifacts: vec![homeboy::core::fuzz::FuzzArtifact {
                schema: homeboy::core::fuzz::FUZZ_ARTIFACT_SCHEMA.to_string(),
                id: "case-log".to_string(),
                kind: "case_log".to_string(),
                artifact: None,
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }],
            thresholds: Vec::new(),
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let gates = evaluate_fuzz_gates(&campaign);

        assert!(gates.iter().any(|gate| {
            gate.gate_id == "target-coverage-complete"
                && gate.status == "failed"
                && gate.observed == 0.5
        }));
        assert!(gates.iter().any(|gate| {
            gate.gate_id == "operation-coverage-complete"
                && gate.status == "passed"
                && gate.observed == 1.0
        }));

        campaign.coverage_summary.as_mut().unwrap().proven_targets = 2;
        assert_eq!(gate_status(&evaluate_fuzz_gates(&campaign)), "passed");
    }

    #[test]
    fn fuzz_coverage_completeness_reports_summary_counts() {
        let campaign = FuzzCampaign {
            schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
            id: "campaign-1".to_string(),
            title: None,
            safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: Vec::new(),
            workloads: Vec::new(),
            cases: Vec::new(),
            seeds: Vec::new(),
            coverage: Vec::new(),
            coverage_summary: Some(homeboy::core::fuzz::FuzzCoverageSummary {
                schema: homeboy::core::fuzz::FUZZ_COVERAGE_SUMMARY_SCHEMA.to_string(),
                declared_targets: 2,
                executable_targets: 1,
                proven_targets: 1,
                declared_operations: 0,
                executable_operations: 0,
                proven_operations: 0,
                skipped_targets: vec![homeboy::core::fuzz::FuzzCoverageSkip {
                    id: "target-2".to_string(),
                    reason: "not_executable".to_string(),
                    label: None,
                }],
                skipped_operations: Vec::new(),
                artifact_ids: vec!["coverage-report".to_string()],
                metadata: serde_json::Value::Null,
                extra: std::collections::BTreeMap::new(),
            }),
            findings: Vec::new(),
            artifacts: Vec::new(),
            thresholds: Vec::new(),
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };

        let summary = fuzz_coverage_completeness(&campaign);

        assert!(summary.has_summary);
        assert_eq!(summary.declared_targets, 2);
        assert_eq!(summary.target_coverage_ratio, 0.5);
        assert_eq!(summary.operation_coverage_ratio, 1.0);
        assert_eq!(summary.skipped_targets, 1);
        assert_eq!(summary.artifact_ids, vec!["coverage-report"]);
    }

    #[test]
    fn fuzz_workloads_include_rig_declared_paths() {
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": "/tmp/package",
                    "extensions": {
                        "generic": {
                            "settings": {}
                        }
                    }
                }
            },
            "fuzz": {
                "default_component": "package"
            },
            "fuzz_workloads": {
                "generic": [
                    { "path": "${package.root}/fuzz/checkout-create-order.json" }
                ]
            }
        }))
        .expect("parse rig spec");
        let component = rig_component_for_fuzz(&spec, "package").expect("rig component");
        let context = FuzzRigContext {
            spec,
            package_root: Some(std::path::PathBuf::from("/tmp/homeboy-rigs/package")),
        };

        let workloads = fuzz_workloads(&component, Some(&context), Some("generic"));

        assert!(workloads.iter().any(|workload| {
            workload.id == "checkout-create-order"
                && workload.manifest_path.as_deref()
                    == Some("/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json")
                && workload.source
                    == "rig_workloads:generic:/tmp/homeboy-rigs/package/fuzz/checkout-create-order.json"
        }));
    }

    #[test]
    fn resolve_component_id_uses_fuzz_default_component() {
        let spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "fuzz": {
                "default_component": "package"
            }
        }))
        .expect("parse rig spec");
        let comp = PositionalComponentArgs {
            component: None,
            path: None,
        };

        assert_eq!(
            resolve_component_id(&comp, Some(&spec)).expect("resolve component"),
            "package"
        );
    }

    #[test]
    fn fuzz_runner_env_includes_results_file_selected_workload_path_and_generic_contract() {
        let args = FuzzRunArgs {
            comp: PositionalComponentArgs {
                component: Some("component-a".to_string()),
                path: None,
            },
            rig: None,
            extension_override: ExtensionOverrideArgs { extensions: vec![] },
            setting_args: SettingArgs {
                setting: vec![],
                setting_json: vec![],
            },
            workload_id: Some("parser".to_string()),
            run_id: Some("proof-1".to_string()),
            seed: Some("1234".to_string()),
            inventory: Some(PathBuf::from("/tmp/fuzz-inventory.json")),
            max_duration: Some("60s".to_string()),
            args: vec![],
        };
        let workload = FuzzWorkloadOutput {
            id: "parser".to_string(),
            label: None,
            description: None,
            source: "rig_workloads:generic:/tmp/fuzz/parser.json".to_string(),
            manifest_path: Some("/tmp/fuzz/parser.json".to_string()),
        };

        let results_path = Path::new("/tmp/homeboy-run/fuzz-results.json");

        let env = fuzz_runner_env(&args, Some(&workload), results_path);

        assert!(env.contains(&(
            "HOMEBOY_FUZZ_RESULTS_FILE".to_string(),
            "/tmp/homeboy-run/fuzz-results.json".to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_WORKLOAD_ID".to_string(), "parser".to_string())));
        assert!(env.contains(&(
            "HOMEBOY_FUZZ_WORKLOAD_PATH".to_string(),
            "/tmp/fuzz/parser.json".to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_RUN_ID".to_string(), "proof-1".to_string())));
        assert!(env.contains(&("HOMEBOY_FUZZ_SEED".to_string(), "1234".to_string())));
        assert!(env.contains(&(
            "HOMEBOY_FUZZ_INVENTORY_FILE".to_string(),
            "/tmp/fuzz-inventory.json".to_string()
        )));
        assert!(env.contains(&("HOMEBOY_FUZZ_MAX_DURATION".to_string(), "60s".to_string())));
    }

    #[test]
    fn fuzz_run_persists_requested_run_id_and_results_artifact() {
        with_isolated_home(|home| {
            let args = FuzzRunArgs {
                comp: PositionalComponentArgs {
                    component: Some("component-a".to_string()),
                    path: None,
                },
                rig: Some("package-fuzz".to_string()),
                extension_override: ExtensionOverrideArgs { extensions: vec![] },
                setting_args: SettingArgs {
                    setting: vec![],
                    setting_json: vec![],
                },
                workload_id: Some("parser".to_string()),
                run_id: Some("proof-1".to_string()),
                seed: Some("1234".to_string()),
                inventory: None,
                max_duration: None,
                args: vec![],
            };
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(&results_path, "{}").expect("results file");

            let persisted = persist_fuzz_run_evidence(
                args.run_id.as_deref(),
                "component-a",
                args.rig.as_deref(),
                args.workload_id.as_deref(),
                Some("/tmp/fuzz/parser.json"),
                "passed",
                0,
                true,
                &args,
                &results_path,
                None,
            )
            .expect("persist fuzz run")
            .expect("run record");

            assert_eq!(persisted.id, "proof-1");
            assert_eq!(persisted.kind, "fuzz");
            assert_eq!(persisted.status, "pass");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run("proof-1")
                .expect("get run")
                .expect("persisted run");
            assert_eq!(run.component_id.as_deref(), Some("component-a"));
            assert_eq!(run.rig_id.as_deref(), Some("package-fuzz"));
            assert_eq!(run.metadata_json["workload_id"], "parser");
            assert_eq!(run.metadata_json["seed"], "1234");
            assert!(run
                .command
                .as_deref()
                .unwrap_or_default()
                .contains("homeboy fuzz run component-a"));
            let artifacts = store.list_artifacts("proof-1").expect("artifacts");
            assert_eq!(artifacts.len(), 1);
            assert_eq!(artifacts[0].kind, "fuzz_results");
            assert_eq!(artifacts[0].artifact_type, "file");
            assert!(std::path::Path::new(&artifacts[0].path).is_file());
        });
    }

    #[test]
    fn fuzz_replay_parses_artifact_and_case_id_flags() {
        let cli = FuzzCli::parse_from([
            "fuzz",
            "replay",
            "/tmp/fuzz-results.json",
            "--case-id",
            "case-1",
            "--run-id",
            "proof-1",
            "--",
            "--runner-flag",
        ]);

        match cli.args.command {
            Some(FuzzCommand::Replay(replay)) => {
                assert_eq!(
                    replay.artifact_or_case.as_deref(),
                    Some("/tmp/fuzz-results.json")
                );
                assert_eq!(replay.case_id.as_deref(), Some("case-1"));
                assert_eq!(replay.run_id.as_deref(), Some("proof-1"));
                assert_eq!(replay.args, vec!["--runner-flag"]);
            }
            _ => panic!("expected fuzz replay command"),
        }
    }

    #[test]
    fn fuzz_replay_resolves_campaign_metadata_without_executing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("fuzz-results.json");
        let campaign = serde_json::json!({
            "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
            "version": homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
            "id": "campaign-1",
            "safety_class": "read_only",
            "cases": [
                {
                    "schema": homeboy::core::fuzz::FUZZ_CASE_SCHEMA,
                    "id": "case-1",
                    "replay_id": "replay-1"
                }
            ],
            "replay": {
                "schema": homeboy::core::fuzz::FUZZ_REPLAY_SCHEMA,
                "id": "replay-1",
                "seed": "1234",
                "artifact_id": "case-artifact"
            }
        });
        std::fs::write(&path, serde_json::to_string(&campaign).unwrap()).expect("write campaign");

        let output = run_replay(FuzzReplayArgs {
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            args: vec!["--runner-flag".to_string()],
        })
        .expect("resolve replay");

        assert_eq!(output.status, "dry_run");
        assert_eq!(output.campaign_id.as_deref(), Some("campaign-1"));
        assert_eq!(output.case_id.as_deref(), Some("case-1"));
        assert_eq!(
            output.replay.as_ref().map(|replay| replay.id.as_str()),
            Some("replay-1")
        );
        assert!(output.env.iter().any(|env| {
            env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE"
                && env.value == path.to_string_lossy().to_string()
        }));
        assert!(output
            .env
            .iter()
            .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_CASE_ID" && env.value == "case-1" }));
        assert!(output
            .env
            .iter()
            .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_SEED" && env.value == "1234" }));
        assert_eq!(output.passthrough_args, vec!["--runner-flag"]);
    }

    #[test]
    fn fuzz_output_contract_includes_results_file_and_parsed_campaign() {
        let results = FuzzCampaign {
            schema: homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA.to_string(),
            version: homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
            id: "campaign-1".to_string(),
            title: None,
            safety_class: homeboy::core::fuzz::FuzzSafetyClass::ReadOnly,
            surfaces: Vec::new(),
            targets: Vec::new(),
            workloads: Vec::new(),
            cases: Vec::new(),
            seeds: Vec::new(),
            coverage: Vec::new(),
            coverage_summary: None,
            findings: Vec::new(),
            artifacts: Vec::new(),
            thresholds: Vec::new(),
            provenance: None,
            replay: None,
            metadata: serde_json::Value::Null,
            extra: std::collections::BTreeMap::new(),
        };
        let run = serde_json::to_value(FuzzOutput::Run(FuzzRunOutput {
            kind: "fuzz".to_string(),
            command: "fuzz.run".to_string(),
            component: "component-a".to_string(),
            rig_id: None,
            status: "passed".to_string(),
            workload_id: None,
            workload_path: None,
            run_id: None,
            seed: None,
            inventory_file: None,
            max_duration: None,
            passthrough_args: Vec::new(),
            target_inventory: None,
            execution: Some(FuzzExecutionOutput {
                kind: "fuzz".to_string(),
                extension_id: "generic".to_string(),
                exit_code: 0,
                success: true,
                run_dir: "/tmp/homeboy-run".to_string(),
                results_file: "/tmp/homeboy-run/fuzz-results.json".to_string(),
                stdout: String::new(),
                stderr: String::new(),
            }),
            results: Some(results),
            runner_contract: FuzzRunnerContract {
                capability: "fuzz".to_string(),
                extension_script_required: true,
                env: vec!["HOMEBOY_FUZZ_RESULTS_FILE"],
            },
            evidence_followups: Vec::new(),
        }))
        .unwrap();

        assert_eq!(
            run["execution"]["results_file"],
            "/tmp/homeboy-run/fuzz-results.json"
        );
        assert_eq!(
            run["results"]["schema"],
            homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA
        );
        assert_eq!(run["results"]["id"], "campaign-1");
        assert_eq!(
            run["runner_contract"]["env"][0],
            "HOMEBOY_FUZZ_RESULTS_FILE"
        );
    }

    #[test]
    fn select_workload_requires_explicit_id_for_ambiguous_fuzz_workloads() {
        let workloads = vec![
            FuzzWorkloadOutput {
                id: "parser".to_string(),
                label: None,
                description: None,
                source: "extension:generic".to_string(),
                manifest_path: None,
            },
            FuzzWorkloadOutput {
                id: "serializer".to_string(),
                label: None,
                description: None,
                source: "extension:generic".to_string(),
                manifest_path: None,
            },
        ];

        let err = select_workload(&workloads, None).expect_err("ambiguous workload");

        assert!(err.message.contains("Multiple fuzz workloads"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("parser, serializer")));
    }

    #[test]
    fn select_workload_rejects_empty_fuzz_selection() {
        let err = select_workload(&[], None).expect_err("empty workload selection");

        assert!(err.message.contains("No fuzz workloads"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("fuzz list")));
    }

    #[test]
    fn fuzz_command_tests_keep_core_fixtures_product_neutral() {
        let source = include_str!("fuzz.rs").to_ascii_lowercase();
        let forbidden = ["word", "press"].concat();
        assert!(!source.contains(&forbidden));
    }
}
