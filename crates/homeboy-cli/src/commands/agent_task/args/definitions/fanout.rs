use clap::{Args, Subcommand};

use super::cook::VerifyGateArgs;

#[derive(Args, Debug)]
pub struct AgentTaskFanoutArgs {
    #[command(subcommand)]
    pub command: AgentTaskFanoutCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskFanoutCommand {
    CookBatch(AgentTaskFanoutCookBatchArgs),
    Plan(AgentTaskFanoutPlanArgs),
    Submit(AgentTaskFanoutSubmitArgs),
    SubmitBatch(AgentTaskFanoutSubmitBatchArgs),
    Status(AgentTaskFanoutBatchStatusArgs),
    Artifacts(AgentTaskFanoutBatchStatusArgs),
    RunPlan(AgentTaskFanoutRunPlanArgs),
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutCookBatchArgs {
    #[arg(value_name = "ISSUE_URL", required = true)]
    pub issues: Vec<String>,
    #[arg(long = "repo", value_name = "REPO")]
    pub repo: String,
    #[arg(long = "from", default_value = "origin/main", value_name = "REF")]
    pub from: String,
    #[arg(long = "base", default_value = "main", value_name = "BRANCH")]
    pub base: String,
    #[arg(long = "branch-prefix", default_value = "fix", value_name = "PREFIX")]
    pub branch_prefix: String,
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    #[arg(long = "prompt-template", value_name = "TEXT")]
    pub prompt_template: Option<String>,
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
    #[arg(long = "provider-profile", value_name = "PROFILE")]
    pub provider_profile: Option<String>,
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    #[arg(long = "run-plan")]
    pub run_plan: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutInputArgs {
    #[arg(long = "input", value_name = "SPEC")]
    pub input: String,
    #[arg(long = "fanout-id", value_name = "ID")]
    pub fanout_id: Option<String>,
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,
    #[arg(long = "model", value_name = "MODEL")]
    pub model: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutPlanArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    #[arg(long = "run-id", value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutSubmitBatchArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    #[arg(long = "batch-id", value_name = "ID")]
    pub batch_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskFanoutBatchStatusArgs {
    pub batch_id: String,
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskFanoutRunPlanArgs {
    #[command(flatten)]
    pub input: AgentTaskFanoutInputArgs,
    #[arg(long = "record-run-id", value_name = "ID")]
    pub record_run_id: Option<String>,
}
