use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Args, Subcommand};

use homeboy::core::fuzz::{
    FuzzCampaign, FuzzExecutionRequest, FuzzGate, FuzzReplayMetadata, FuzzRequiredArtifact,
    FuzzResultEnvelope, FuzzTargetInventory,
};

use super::super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
    FUZZ_LAB_LABEL,
};

#[derive(Args)]
pub struct FuzzArgs {
    #[command(subcommand)]
    pub(crate) command: Option<FuzzCommand>,

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
pub(crate) enum FuzzCommand {
    /// Print the product-neutral fuzz schema contract
    Contract,
    /// List declared fuzz workloads without executing them
    List(FuzzListArgs),
    /// Build a fuzz execution request without executing it
    Plan(FuzzPlanArgs),
    /// Execute the selected fuzz workload, persist fuzz evidence, and surface its campaign contract
    Run(FuzzRunArgs),
    /// Validate a fuzz result campaign file
    Validate(FuzzValidateArgs),
    /// Persist a result envelope from a fuzz campaign file
    Report(FuzzReportArgs),
    /// Resolve replay metadata for persisted fuzz cases
    Replay(FuzzReplayArgs),
}

#[derive(Args, Clone)]
pub(crate) struct FuzzListArgs {
    #[command(flatten)]
    pub(crate) comp: PositionalComponentArgs,

    /// Discover workloads using a rig's component path, extension config, and
    /// rig-declared fuzz workloads.
    #[arg(long, value_name = "RIG_ID")]
    pub(crate) rig: Option<String>,

    #[command(flatten)]
    pub(crate) extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    pub(crate) setting_args: SettingArgs,
}

#[derive(Args, Clone)]
pub struct FuzzRunArgs {
    #[command(flatten)]
    pub(crate) comp: PositionalComponentArgs,

    /// Run against a rig's component path, extension config, and rig-declared
    /// fuzz workloads.
    #[arg(long, value_name = "RIG_ID")]
    pub(crate) rig: Option<String>,

    #[command(flatten)]
    pub(crate) extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    pub(crate) setting_args: SettingArgs,

    /// Extension-declared workload id to select.
    #[arg(long = "workload", value_name = "ID")]
    pub(crate) workload_id: Option<String>,

    /// Stable caller-supplied proof label for downstream fuzz runners.
    #[arg(long = "run-id", value_name = "ID")]
    pub(crate) run_id: Option<String>,

    /// Deterministic seed forwarded by future fuzz runners.
    #[arg(long, value_name = "SEED")]
    pub(crate) seed: Option<String>,

    /// Product-neutral fuzz target inventory JSON discovered before execution.
    #[arg(long = "inventory", value_name = "PATH")]
    pub(crate) inventory: Option<PathBuf>,

    /// Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m.
    #[arg(long, value_name = "DURATION")]
    pub(crate) max_duration: Option<String>,

    /// Additional runner arguments reserved for the fuzz extension script.
    #[arg(last = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Args, Clone)]
pub(crate) struct FuzzPlanArgs {
    #[command(flatten)]
    pub(crate) run: FuzzRunArgs,

    /// Stable request id. Defaults to --run-id, then the selected workload id.
    #[arg(long = "request-id", value_name = "ID")]
    pub(crate) request_id: Option<String>,
}

#[derive(Args, Clone)]
pub(crate) struct FuzzValidateArgs {
    /// Fuzz campaign JSON file emitted by a runner.
    #[arg(value_name = "RESULTS_FILE")]
    pub(crate) results_file: PathBuf,
}

#[derive(Args, Clone)]
pub(crate) struct FuzzReportArgs {
    /// Fuzz campaign JSON file emitted by a runner.
    #[arg(value_name = "RESULTS_FILE")]
    pub(crate) results_file: PathBuf,

    #[command(flatten)]
    pub(crate) run: FuzzRunArgs,

    /// Persist the result envelope JSON to this path.
    #[arg(long = "output-envelope", value_name = "PATH")]
    pub(crate) output_envelope: Option<PathBuf>,

    /// Stable envelope id. Defaults to --run-id, then the campaign id.
    #[arg(long = "envelope-id", value_name = "ID")]
    pub(crate) envelope_id: Option<String>,
}

#[derive(Args, Clone)]
pub(crate) struct FuzzReplayArgs {
    /// Fuzz campaign/result envelope path, or a case id when --artifact is used.
    #[arg(value_name = "ARTIFACT_OR_CASE")]
    pub(crate) artifact_or_case: Option<String>,

    /// Fuzz campaign or result envelope artifact to inspect for replay metadata.
    #[arg(long = "artifact", value_name = "PATH")]
    pub(crate) artifact: Option<PathBuf>,

    /// Case id to replay from the campaign/envelope artifact.
    #[arg(long = "case-id", value_name = "ID")]
    pub(crate) case_id: Option<String>,

    /// Stable Homeboy run id associated with the persisted fuzz evidence.
    #[arg(long = "run-id", value_name = "ID")]
    pub(crate) run_id: Option<String>,

    /// Additional runner arguments reserved for future fuzz replay support.
    #[arg(last = true)]
    pub(crate) args: Vec<String>,
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
    pub campaign_contract: FuzzCampaignContract,
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

#[derive(Serialize)]
pub struct FuzzCampaignContract {
    pub case_artifact: Option<String>,
    pub corpus_artifacts: Vec<String>,
    pub seed: Option<String>,
    pub replay_command: Option<String>,
    pub minimize_command: Option<String>,
    pub result_schema: String,
    pub artifact_retention: Option<String>,
    pub unsupported: Vec<&'static str>,
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
    pub skipped_reason_counts: BTreeMap<String, usize>,
    pub surface_summaries: Vec<FuzzCoverageSelectorSummaryOutput>,
    pub kind_summaries: Vec<FuzzCoverageSelectorSummaryOutput>,
    pub artifact_ids: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct FuzzCoverageSelectorSummaryOutput {
    pub id: String,
    pub kind: String,
    pub label: Option<String>,
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
    pub skipped_reason_counts: BTreeMap<String, usize>,
}
