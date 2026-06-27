use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use homeboy::core::evidence_manifest::TrackerRef;

use super::super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
    FUZZ_LAB_LABEL,
};
use homeboy::core::fuzz::FuzzGateProfile;

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
        self.is_lab_offload_command()
            .then(|| LabCommandContract::portable_workload(FUZZ_LAB_LABEL, None, true, &[]))
    }

    pub fn is_run_invocation(&self) -> bool {
        matches!(self.command, None | Some(FuzzCommand::Run(_)))
    }

    /// Fuzz subcommands that offload to the configured Lab runner. `run`
    /// executes the workload remotely; `list` enumerates the runner-resident
    /// rig/extension fuzz workloads so the operator sees the same inventory the
    /// runner would execute rather than the (possibly empty) local one. This
    /// mirrors `bench`'s `is_lab_offload_command`, which routes its discovery
    /// subcommands to the runner alongside `run`.
    pub fn is_lab_offload_command(&self) -> bool {
        matches!(
            self.command,
            None | Some(FuzzCommand::Run(_)) | Some(FuzzCommand::List(_))
        )
    }

    pub fn extension_override_ids(&self) -> &[String] {
        match &self.command {
            Some(FuzzCommand::List(list)) => list.extension_override.extensions.as_slice(),
            _ => self.run.extension_override.extensions.as_slice(),
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum FuzzCommand {
    /// Print the product-neutral fuzz schema contract
    Contract,
    /// Diagnose active fuzz runtime provenance and installed extension revision
    Doctor(FuzzDoctorArgs),
    /// Normalize and merge discovered fuzz target inventory artifacts
    Discover(FuzzDiscoverArgs),
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
    /// Compare two persisted fuzz result envelopes
    Compare(FuzzCompareArgs),
    /// Resolve replay metadata for persisted fuzz cases
    Replay(FuzzReplayArgs),
    /// Print the raw fuzz runner result for a run without spelunking runner logs
    Inspect(FuzzInspectArgs),
}

#[derive(Args, Clone)]
pub(crate) struct FuzzDoctorArgs {
    /// Extension whose active install should be diagnosed.
    #[arg(long = "extension", value_name = "ID", required = true)]
    pub(crate) extension_id: String,
}

#[derive(Args, Clone)]
pub(crate) struct FuzzInspectArgs {
    /// Homeboy run id whose raw fuzz result should be inspected. Accepts the
    /// fuzz run id or the Lab runner-exec run id that offloaded it.
    #[arg(value_name = "RUN_ID")]
    pub(crate) run_id: String,

    /// Print the result body as raw bytes/text instead of pretty JSON.
    #[arg(long = "raw")]
    pub(crate) raw: bool,
}

#[derive(Args, Clone)]
pub(crate) struct FuzzDiscoverArgs {
    /// Existing fuzz target inventory artifact to ingest.
    #[arg(long = "inventory", value_name = "PATH", required = true)]
    pub(crate) inventories: Vec<PathBuf>,

    /// Stable id for the merged inventory artifact.
    #[arg(long = "inventory-id", value_name = "ID")]
    pub(crate) inventory_id: Option<String>,

    /// Human-readable source label recorded in merged provenance.
    #[arg(
        long = "source-label",
        value_name = "LABEL",
        default_value = "artifact"
    )]
    pub(crate) source_label: String,
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

    /// Product-agnostic tracker anchor for this fuzz run. Repeatable. Format: KIND:ID.
    #[arg(long = "tracker-ref", value_name = "KIND:ID", value_parser = parse_tracker_ref)]
    pub(crate) tracker_refs: Vec<TrackerRef>,

    /// Deterministic seed forwarded by future fuzz runners.
    #[arg(long, value_name = "SEED")]
    pub(crate) seed: Option<String>,

    /// Product-neutral fuzz target inventory JSON discovered before execution.
    #[arg(long = "inventory", value_name = "PATH")]
    pub(crate) inventory: Option<PathBuf>,

    /// Fail the run unless the campaign links case-level execution evidence.
    #[arg(long = "require-case-log")]
    pub(crate) require_case_log: bool,

    /// Fail the run unless the campaign includes or links a coverage summary.
    #[arg(long = "require-coverage-summary")]
    pub(crate) require_coverage_summary: bool,

    /// Fail the run unless the campaign links a result-envelope artifact.
    #[arg(long = "require-result-envelope")]
    pub(crate) require_result_envelope: bool,

    /// Maximum runtime budget forwarded by future fuzz runners, e.g. 60s or 5m.
    #[arg(long, value_name = "DURATION")]
    pub(crate) max_duration: Option<String>,

    /// Required artifact and gate profile to request from the fuzz runner.
    #[arg(long = "gate-profile", value_enum, default_value_t = FuzzGateProfileArg::Measurement)]
    pub(crate) gate_profile: FuzzGateProfileArg,

    /// Require a numeric metric emitted by the fuzz campaign to equal this value.
    /// Repeatable. Format: `--expect-metric metric_name=2`.
    #[arg(long = "expect-metric", value_name = "METRIC=VALUE", value_parser = crate::commands::parse_key_val)]
    pub(crate) expect_metric: Vec<(String, String)>,

    /// Additional runner arguments reserved for the fuzz extension script.
    #[arg(last = true)]
    pub(crate) args: Vec<String>,
}

fn parse_tracker_ref(raw: &str) -> Result<TrackerRef, String> {
    let (kind, id) = raw
        .split_once(':')
        .ok_or_else(|| format!("invalid tracker ref `{raw}`; expected KIND:ID"))?;
    let kind = kind.trim();
    let id = id.trim();
    if kind.is_empty() || id.is_empty() {
        return Err(format!(
            "invalid tracker ref `{raw}`; kind and id must be non-empty"
        ));
    }
    Ok(TrackerRef {
        kind: kind.to_string(),
        id: id.to_string(),
        url: None,
        title: None,
        state: None,
    })
}

#[derive(Args, Clone)]
pub(crate) struct FuzzPlanArgs {
    #[command(flatten)]
    pub(crate) run: FuzzRunArgs,

    /// Stable request id. Defaults to --run-id, then the selected workload id.
    #[arg(long = "request-id", value_name = "ID")]
    pub(crate) request_id: Option<String>,

    /// Inventory selection strategy.
    #[arg(long, value_enum, default_value_t = FuzzPlanStrategy::All)]
    pub(crate) strategy: FuzzPlanStrategy,

    /// Select operations by canonical family, operation kind, or operation id.
    #[arg(long = "operation", value_name = "FILTER")]
    pub(crate) operations: Vec<String>,

    /// Select operations by canonical family.
    #[arg(long = "operation-family", value_name = "FAMILY")]
    pub(crate) operation_families: Vec<String>,

    /// Maximum number of cases the downstream runner should generate.
    #[arg(long = "case-budget", value_name = "COUNT")]
    pub(crate) case_budget: Option<u64>,

    /// Maximum execution budget in seconds for downstream runners.
    #[arg(long = "duration-budget-seconds", value_name = "SECONDS")]
    pub(crate) duration_budget_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum FuzzPlanStrategy {
    All,
    ReadOnly,
    Crud,
    CoverageGaps,
}

impl FuzzPlanStrategy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ReadOnly => "read-only",
            Self::Crud => "crud",
            Self::CoverageGaps => "coverage-gaps",
        }
    }
}

#[derive(Args, Clone)]
pub(crate) struct FuzzValidateArgs {
    /// Fuzz campaign JSON file emitted by a runner.
    #[arg(value_name = "RESULTS_FILE")]
    pub(crate) results_file: PathBuf,

    /// Canonical fuzz case log JSONL/JSON artifact to validate.
    #[arg(long = "case-log", value_name = "PATH")]
    pub(crate) case_logs: Vec<PathBuf>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum FuzzGateProfileArg {
    /// Preserve measurement evidence without default threshold gates.
    Measurement,
    /// Require replayable evidence without complete coverage gates.
    Evidence,
    /// Require declared target and operation coverage completeness.
    CoverageComplete,
    /// Require evidence and complete coverage gates.
    Strict,
}

impl FuzzGateProfileArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Measurement => "measurement",
            Self::Evidence => "evidence",
            Self::CoverageComplete => "coverage-complete",
            Self::Strict => "strict",
        }
    }

    pub(crate) fn as_core(self) -> FuzzGateProfile {
        match self {
            Self::Measurement => FuzzGateProfile::Measurement,
            Self::Evidence => FuzzGateProfile::Evidence,
            Self::CoverageComplete => FuzzGateProfile::CoverageComplete,
            Self::Strict => FuzzGateProfile::Strict,
        }
    }
}

#[derive(Args, Clone)]
pub(crate) struct FuzzCompareArgs {
    /// Baseline fuzz result envelope JSON file.
    #[arg(value_name = "BASELINE_ENVELOPE")]
    pub(crate) baseline: PathBuf,

    /// Candidate fuzz result envelope JSON file.
    #[arg(value_name = "CANDIDATE_ENVELOPE")]
    pub(crate) candidate: PathBuf,

    /// How relative hotspot regressions affect the blocking compare status.
    #[arg(long = "hotspot-policy", value_enum, default_value_t = FuzzCompareHotspotPolicy::Advisory)]
    pub(crate) hotspot_policy: FuzzCompareHotspotPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FuzzCompareHotspotPolicy {
    /// Measure and report hotspot regressions without failing the compare.
    Advisory,
    /// Treat relative hotspot regressions as blocking compare regressions.
    Blocking,
    /// Measure hotspot deltas without classifying regressions.
    Off,
}

impl FuzzCompareHotspotPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Blocking => "blocking",
            Self::Off => "off",
        }
    }
}

#[derive(Args, Clone)]
pub(crate) struct FuzzReplayArgs {
    /// Component ID used to resolve the extension replay_command.
    #[arg(long = "component", value_name = "ID")]
    pub(crate) component: Option<String>,

    /// Override the component checkout path for replay command execution.
    #[arg(long)]
    pub(crate) path: Option<String>,

    /// Resolve replay through a rig's component path and extension config.
    #[arg(long, value_name = "RIG_ID")]
    pub(crate) rig: Option<String>,

    #[command(flatten)]
    pub(crate) extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    pub(crate) setting_args: SettingArgs,

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

    /// Resolve replay metadata and command environment without executing replay_command.
    #[arg(long = "dry-run")]
    pub(crate) dry_run: bool,

    /// Additional arguments passed to the extension replay command.
    #[arg(last = true)]
    pub(crate) args: Vec<String>,
}

pub use super::types_extra::{
    FuzzArtifactPostprocessOutput, FuzzCampaignContract, FuzzCompareDeltas,
    FuzzCompareHotspotDelta, FuzzCompareHotspotSnapshot, FuzzCompareHotspotSummary,
    FuzzCompareOutput, FuzzCompareSnapshot, FuzzContractGateProfileOutput, FuzzContractOutput,
    FuzzCoverageCompletenessOutput, FuzzCoverageSelectorSummaryOutput, FuzzDiscoverOutput,
    FuzzDiscoverSummary, FuzzExecutionOutput, FuzzGateEvaluation, FuzzGateStatusChange,
    FuzzInspectCandidate, FuzzInspectOutput, FuzzListOutput, FuzzOutput, FuzzPlanOutput,
    FuzzReplayEnv, FuzzReplayExecution, FuzzReplayOutput, FuzzReportOutput, FuzzRunOutput,
    FuzzRunnerContract, FuzzValidateOutput, FuzzWorkloadOutput,
};
