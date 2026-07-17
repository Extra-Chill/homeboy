use clap::Args;

use super::super::super::review;
use super::cook::VerifyGateArgs;

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}
#[derive(Args, Debug)]
pub struct RunArgs {
    pub run_id: String,
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}
#[derive(Args, Debug)]
pub struct SubmitArgs {
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}
#[derive(Args, Debug)]
pub struct StatusArgs {
    pub run_id: String,
    #[arg(long)]
    pub bridge: bool,
    #[arg(long, value_name = "CURSOR", requires = "bridge")]
    pub since_cursor: Option<u64>,
    #[arg(long, conflicts_with = "bridge")]
    pub full: bool,
}
#[derive(Args, Debug)]
pub struct EvidenceArgs {
    pub run_id: String,
    #[arg(long = "kind", value_name = "KIND")]
    pub kind: Option<String>,
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,
    #[arg(long = "failure-only")]
    pub failure_only: bool,
}
#[derive(Args, Debug)]
pub struct DiagnoseArgs {
    pub run_id: String,
}
#[derive(Args, Debug)]
pub struct ReplayProviderBoundaryArgs {
    pub run_id: String,
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,
}
#[derive(Args, Debug)]
pub struct RetryArgs {
    pub run_id: String,
    #[arg(long, value_name = "ID")]
    pub new_run_id: Option<String>,
    #[arg(long)]
    pub run: bool,
}
#[derive(Args, Debug)]
pub struct CancelArgs {
    pub run_id: String,
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}
#[derive(Args, Debug)]
pub struct ReviewArgs {
    pub run_id: String,
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: Option<String>,
    #[arg(long, value_name = "COMMAND")]
    pub provider_command: Option<String>,
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command"
    )]
    pub provider_argv: Vec<String>,
}
#[derive(Args, Debug)]
pub struct PromoteArgs {
    pub source: String,
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,
    /// Declared base branch resolved immediately before promotion gates run.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,
    #[arg(long, value_name = "COMMAND")]
    pub provider_command: Option<String>,
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command"
    )]
    pub provider_argv: Vec<String>,
    #[arg(long, value_name = "TASK_ID")]
    pub task_id: Option<String>,
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub artifact_id: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
}
#[derive(Args, Debug)]
pub struct FinalizePrArgs {
    #[arg(long, value_name = "ID")]
    pub run_id: String,
    #[arg(long, value_name = "PATH")]
    pub path: String,
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,
    /// Immutable base commit SHA recorded before the declared verification gates ran.
    #[arg(long, value_name = "SHA")]
    pub verified_base_sha: Option<String>,
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,
    #[arg(long, value_name = "TEXT")]
    pub title: String,
    #[arg(long, value_name = "TEXT")]
    pub commit_message: String,
    #[command(flatten)]
    pub evidence: review::FinalizePrEvidenceArgs,
    #[arg(long = "gate-result", value_name = "NAME=STATUS[:DETAIL]")]
    pub gate_results: Vec<String>,
    #[arg(long = "changed-file", value_name = "PATH")]
    pub changed_files: Vec<String>,
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,
    #[arg(
        long,
        default_value = "Drafted implementation and tests; Chris reviews and owns the change.",
        value_name = "TEXT"
    )]
    pub ai_used_for: String,
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,
    #[arg(long = "what-changed", value_name = "TEXT")]
    pub what_changed: Vec<String>,
    #[arg(long = "test-step", value_name = "TEXT")]
    pub test_steps: Vec<String>,
    #[arg(long, value_name = "TEXT")]
    pub compatibility: Option<String>,
    #[arg(long = "closes", value_name = "REF")]
    pub closes: Vec<String>,
    #[arg(long = "relates-to", value_name = "REF")]
    pub relates_to: Vec<String>,
    #[arg(long = "review-override", value_name = "TARGET=VALUE@PROVENANCE")]
    pub review_overrides: Vec<String>,
    #[arg(long)]
    pub manual_finalization: bool,
}
#[derive(Args, Debug)]
pub struct GateFeedbackArgs {
    #[arg(long, value_name = "PATH")]
    pub promotion: String,
    #[arg(long = "source-task", value_name = "PATH")]
    pub source_task: String,
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub attempt: u32,
    #[arg(long = "max-attempts", default_value_t = 3, value_name = "N")]
    pub max_attempts: u32,
    #[arg(long = "source-run-id", value_name = "ID")]
    pub source_run_id: Option<String>,
    #[arg(long = "current-diff", value_name = "SPEC")]
    pub current_diff: Option<String>,
}
