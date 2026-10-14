use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::super::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
    WORKLOAD_LAB_LABEL,
};

#[derive(Args)]
pub struct WorkloadArgs {
    #[command(subcommand)]
    pub(crate) command: WorkloadCommand,
}

impl WorkloadArgs {
    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(CommandJsonFamily::Quality, output_file_mode)
    }

    /// `workload run` executes on the configured Lab runner like any portable
    /// workload; everything else reads controller-local state.
    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        matches!(self.command, WorkloadCommand::Run(_))
            .then(|| LabCommandContract::portable_workload(WORKLOAD_LAB_LABEL, None, true, &[]))
    }

    /// The kind's provider must be installed on the runner too.
    pub(crate) fn lab_route_required_extension_ids(&self) -> Vec<String> {
        match &self.command {
            WorkloadCommand::Run(run) => run.target.extension_override.extensions.clone(),
            _ => Vec::new(),
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum WorkloadCommand {
    /// List the workloads an extension declares for a kind
    List(WorkloadListArgs),
    /// Run one workload of a kind, persist it as a run and gate it against the kind's baseline
    Run(WorkloadRunArgs),
    /// Compare two persisted runs of a kind by its declared metrics and findings
    Compare(WorkloadCompareArgs),
    /// Replay one case of a persisted run through the kind's declared replay_command
    Replay(WorkloadCaseArgs),
    /// Minimize one case of a persisted run through the kind's declared minimize_command
    Minimize(WorkloadCaseArgs),
    /// Show a persisted workload run: status, cases, metrics, findings and artifacts
    Inspect(WorkloadInspectArgs),
}

/// Where a kind runs: a component (positional or CWD) or a rig component.
#[derive(Args, Clone)]
pub(crate) struct WorkloadTargetArgs {
    #[command(flatten)]
    pub(crate) comp: PositionalComponentArgs,

    /// Run against a rig's component path and extension config.
    #[arg(long, value_name = "RIG_ID")]
    pub(crate) rig: Option<String>,

    #[command(flatten)]
    pub(crate) extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    pub(crate) setting_args: SettingArgs,
}

#[derive(Args, Clone)]
pub(crate) struct WorkloadListArgs {
    /// Workload kind, as declared by an extension under `workloads.<kind>`.
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: String,

    #[command(flatten)]
    pub(crate) target: WorkloadTargetArgs,

    /// Skip the runner's script list probe and report only static and
    /// JSON-pointer declarations.
    #[arg(long)]
    pub(crate) no_script_probe: bool,
}

#[derive(Args, Clone)]
pub(crate) struct WorkloadRunArgs {
    /// Workload kind, as declared by an extension under `workloads.<kind>`.
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: String,

    #[command(flatten)]
    pub(crate) target: WorkloadTargetArgs,

    /// Workload id to run, forwarded as HOMEBOY_WORKLOAD_ID.
    #[arg(long, value_name = "ID")]
    pub(crate) workload: Option<String>,

    /// Stable caller-owned run id. Re-running it restarts the same run.
    #[arg(long = "run-id", value_name = "ID")]
    pub(crate) run_id: Option<String>,

    /// Deterministic seed, forwarded as HOMEBOY_WORKLOAD_SEED.
    #[arg(long, value_name = "SEED")]
    pub(crate) seed: Option<String>,

    /// Runner timeout, e.g. 60s or 5m.
    #[arg(long, value_name = "DURATION")]
    pub(crate) max_duration: Option<String>,

    #[command(flatten)]
    pub(crate) baseline_args: BaselineArgs,

    /// Permit a kind declared `safety_class: destructive`. Requires --isolation-proof.
    #[arg(long = "allow-destructive", requires = "isolation_proof")]
    pub(crate) allow_destructive: bool,

    /// homeboy/isolation-proof/v1 JSON proving the destructive run is disposable.
    #[arg(long = "isolation-proof", value_name = "PATH")]
    pub(crate) isolation_proof: Option<PathBuf>,

    /// Extra arguments forwarded to the kind's runner script.
    #[arg(last = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Args, Clone)]
pub(crate) struct WorkloadCompareArgs {
    /// Workload kind both runs must be.
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: String,

    /// Baseline run id.
    #[arg(long = "from-run", value_name = "RUN_ID")]
    pub(crate) from_run: String,

    /// Candidate run id.
    #[arg(long = "to-run", value_name = "RUN_ID")]
    pub(crate) to_run: String,
}

#[derive(Args, Clone)]
pub(crate) struct WorkloadCaseArgs {
    /// Workload kind of the source run.
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: String,

    /// Persisted run holding the case.
    #[arg(long = "run-id", value_name = "RUN_ID")]
    pub(crate) run_id: String,

    /// Case to replay or minimize.
    #[arg(long = "case-id", value_name = "CASE_ID")]
    pub(crate) case_id: String,

    /// Component and settings. Defaults to the source run's component and checkout.
    #[command(flatten)]
    pub(crate) target: WorkloadTargetArgs,

    /// Resolve the command and case env without executing it.
    #[arg(long)]
    pub(crate) dry_run: bool,

    /// Command timeout, e.g. 60s or 5m.
    #[arg(long, value_name = "DURATION")]
    pub(crate) max_duration: Option<String>,

    /// Permit a kind declared `safety_class: destructive`. Requires --isolation-proof.
    #[arg(long = "allow-destructive", requires = "isolation_proof")]
    pub(crate) allow_destructive: bool,

    /// homeboy/isolation-proof/v1 JSON proving the destructive run is disposable.
    #[arg(long = "isolation-proof", value_name = "PATH")]
    pub(crate) isolation_proof: Option<PathBuf>,

    /// Extra arguments appended to the declared command.
    #[arg(last = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Args, Clone)]
pub(crate) struct WorkloadInspectArgs {
    /// Persisted workload run id.
    #[arg(value_name = "RUN_ID")]
    pub(crate) run_id: String,

    /// Require the run to be of this kind.
    #[arg(long, value_name = "KIND")]
    pub(crate) kind: Option<String>,

    /// Include the full envelope (findings and opaque payload).
    #[arg(long)]
    pub(crate) full: bool,
}
