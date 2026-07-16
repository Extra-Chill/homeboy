use clap::{Args, Subcommand, ValueEnum};
use homeboy::core::agent_task_service::AgentTaskDiscoveryOptions;

use super::super::super::prompts::AgentTaskPromptsArgs;
use super::super::super::tool::AgentTaskToolArgs;
use super::cook::{AgentTaskCookArgs, AgentTaskLoopArgs, PromotionProviderArgs};
use super::fanout::AgentTaskFanoutArgs;
use super::lifecycle::{
    CancelArgs, DiagnoseArgs, EvidenceArgs, FinalizePrArgs, GateFeedbackArgs, PromoteArgs,
    ReplayProviderBoundaryArgs, RetryArgs, ReviewArgs, RunArgs, RunPlanArgs, RuntimeRecoverArgs,
    RuntimeValidateArgs, StatusArgs, SubmitArgs,
};

pub use super::super::auth::{
    AgentTaskAuthCommand, AgentTaskAuthMapEnvArgs, AgentTaskAuthMapKeychainBundleArgs,
    AgentTaskAuthRemoveArgs, AgentTaskAuthSetConfigArgs, AgentTaskAuthSetKeychainArgs,
    AgentTaskAuthSetKeychainBundleArgs, AgentTaskAuthStatusArgs,
};
pub use super::super::controller::{
    AgentTaskControllerApplyEventArgs, AgentTaskControllerCommand, AgentTaskControllerDispatchArgs,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerInitArgs,
    AgentTaskControllerMarkHumanReadyArgs, AgentTaskControllerMaterializeArgs,
    AgentTaskControllerPlanArgs, AgentTaskControllerProofArgs, AgentTaskControllerRunArgs,
    AgentTaskControllerRunFromSpecArgs, AgentTaskControllerRunNextArgs,
    AgentTaskControllerStatusArgs, AgentTaskControllerValidateProofArgs,
};

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    Doctor(AgentTaskDoctorArgs),
    Cook(AgentTaskCookArgs),
    Loop(AgentTaskLoopArgs),
    RunPlan(RunPlanArgs),
    Run(RunArgs),
    RunNext,
    Submit(SubmitArgs),
    Status(StatusArgs),
    List(ListArgs),
    Active(ActiveArgs),
    ReconcileRecords(ReconcileRecordsArgs),
    Latest(LatestArgs),
    Logs(StatusArgs),
    Artifacts(StatusArgs),
    Evidence(EvidenceArgs),
    Diagnose(DiagnoseArgs),
    /// Recover a missing or corrupted immutable controller runtime pin.
    RuntimeRecover(RuntimeRecoverArgs),
    /// Validate controller runtime eligibility without executing provider work.
    RuntimeValidate(RuntimeValidateArgs),
    ReplayProviderBoundary(ReplayProviderBoundaryArgs),
    Cancel(CancelArgs),
    Resume(StatusArgs),
    Retry(RetryArgs),
    Fanout(AgentTaskFanoutArgs),
    Review(ReviewArgs),
    Promote(PromoteArgs),
    #[command(hide = true)]
    PromotionProvider(PromotionProviderArgs),
    FinalizePr(FinalizePrArgs),
    GateFeedback(GateFeedbackArgs),
    Providers(ProvidersArgs),
    Prompts(AgentTaskPromptsArgs),
    Contract(ContractArgs),
    CompileLoop(CompileLoopArgs),
    Auth(AgentTaskAuthArgs),
    Controller(AgentTaskControllerArgs),
    #[command(hide = true)]
    Tool(AgentTaskToolArgs),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}
#[derive(Args, Debug)]
pub struct ActiveArgs {
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
    #[arg(long = "reconcile")]
    pub reconcile: bool,
    #[arg(long = "dry-run", requires = "reconcile")]
    pub dry_run: bool,
}
#[derive(Args, Debug)]
pub struct ReconcileRecordsArgs {
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}
#[derive(Args, Debug)]
pub struct LatestArgs {
    #[arg(long = "limit", value_name = "N")]
    pub limit: Option<usize>,
}
impl From<ListArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ListArgs) -> Self {
        Self { limit: args.limit }
    }
}
impl From<ActiveArgs> for AgentTaskDiscoveryOptions {
    fn from(args: ActiveArgs) -> Self {
        Self { limit: args.limit }
    }
}
impl From<LatestArgs> for AgentTaskDiscoveryOptions {
    fn from(args: LatestArgs) -> Self {
        Self { limit: args.limit }
    }
}
#[derive(Args, Debug)]
pub struct AgentTaskAuthArgs {
    #[command(subcommand)]
    pub command: AgentTaskAuthCommand,
}
#[derive(Args, Debug)]
pub struct AgentTaskControllerArgs {
    #[command(subcommand)]
    pub command: AgentTaskControllerCommand,
}
#[derive(Args, Debug)]
pub struct ProvidersArgs {
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    #[arg(long = "validate-readiness")]
    pub validate_readiness: bool,
    #[arg(long = "refresh")]
    pub refresh: bool,
}
#[derive(Args, Debug)]
pub struct AgentTaskDoctorArgs {
    #[arg(long, value_name = "RUNNER")]
    pub runner: String,
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(long, visible_alias = "provider-id", value_name = "PROVIDER_ID")]
    pub selector: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
    #[arg(long = "extension", value_name = "EXTENSION")]
    pub extensions: Vec<String>,
    #[arg(long = "require-tool", value_name = "TOOL")]
    pub required_tools: Vec<String>,
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    #[arg(long)]
    pub repair: bool,
}
#[derive(Args, Debug)]
pub struct ContractArgs {
    #[arg(long, default_value = "json", value_enum)]
    pub format: ContractFormat,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ContractFormat {
    Json,
}
#[derive(Args, Debug)]
pub struct CompileLoopArgs {
    #[arg(long, value_name = "SPEC")]
    pub definition: String,
}
