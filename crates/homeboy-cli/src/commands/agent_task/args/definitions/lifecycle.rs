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
pub struct LogsArgs {
    pub run_id: String,
    /// Include unprojected runner transport frames under `raw_events` for diagnostics.
    #[arg(long)]
    pub raw: bool,
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
pub struct RuntimeRecoverArgs {
    /// Durable run whose exact controller executable should be rematerialized.
    pub run_id: String,
    /// Trusted source checkout used to rebuild the recorded runtime revision.
    #[arg(
        long,
        value_name = "PATH",
        required_unless_present = "artifact",
        conflicts_with = "artifact"
    )]
    pub source: Option<String>,
    /// Exact prebuilt controller executable. Its hash and self identity must match the durable pin.
    #[arg(
        long,
        value_name = "PATH",
        required_unless_present = "source",
        conflicts_with = "source"
    )]
    pub artifact: Option<String>,
}
#[derive(Args, Debug)]
pub struct RuntimeValidateArgs {
    /// Durable run to validate without executing its provider lifecycle.
    pub run_id: String,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::{
        cli_surface::{Cli, Commands},
        commands::agent_task::AgentTaskCommand,
    };

    #[test]
    fn runtime_recovery_requires_one_trusted_input() {
        assert!(
            Cli::try_parse_from(["homeboy", "agent-task", "runtime-recover", "run-a"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "runtime-recover",
            "run-a",
            "--artifact",
            "/trusted/homeboy",
            "--source",
            "/trusted/source",
        ])
        .is_err());

        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "runtime-recover",
            "run-a",
            "--artifact",
            "/trusted/homeboy",
        ])
        .expect("artifact recovery parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::RuntimeRecover(args) = agent_task.command else {
            panic!("expected runtime recovery command");
        };
        assert_eq!(args.run_id, "run-a");
        assert_eq!(args.artifact.as_deref(), Some("/trusted/homeboy"));
        assert!(args.source.is_none());
    }

    #[test]
    fn adoption_parses_external_candidate_model() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "adopt",
            "cook-a",
            "--candidate-ref",
            "0123456789abcdef0123456789abcdef01234567",
            "--ai-model",
            "openai/gpt-5.6-sol",
        ])
        .expect("adoption model parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Adopt(args) = agent_task.command else {
            panic!("expected adoption command");
        };
        assert_eq!(args.ai_model.as_deref(), Some("openai/gpt-5.6-sol"));
    }

    #[test]
    fn promotion_provider_help_documents_argv_contract_for_cook_review_and_promote() {
        for command in ["cook", "review", "promote"] {
            let Err(error) = Cli::try_parse_from(["homeboy", "agent-task", command, "--help"])
            else {
                panic!("help exits after rendering");
            };
            let help = error.to_string();

            assert!(help.contains("promotion apply-provider"), "{help}");
            assert!(
                help.contains("Repeat once per exact argv element"),
                "{help}"
            );
            assert!(help.contains("never shell-split"), "{help}");
            assert!(
                help.contains("homeboy/agent-task-promotion-apply-request/v1"),
                "{help}"
            );
            assert!(
                help.contains("homeboy/agent-task-promotion-apply-response/v1"),
                "{help}"
            );
            assert!(help.contains("Migrate `--provider-command"), "{help}");
        }
    }
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
    #[arg(
        long,
        value_name = "COMMAND",
        long_help = "Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`."
    )]
    pub provider_command: Option<String>,
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command",
        long_help = "Promotion apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
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
    #[arg(
        long,
        value_name = "COMMAND",
        long_help = "Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`."
    )]
    pub provider_command: Option<String>,
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command",
        long_help = "Promotion apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
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
pub struct AdoptArgs {
    /// Existing durable run id or cook id whose recipe owns the candidate lifecycle.
    pub run_or_cook_id: String,
    /// Immutable commit revision in the recorded source worktree.
    #[arg(long, value_name = "SHA")]
    pub candidate_ref: String,
    /// Concrete model that prepared the externally supplied candidate.
    #[arg(long, value_name = "MODEL")]
    pub ai_model: Option<String>,
    /// Return the complete cook adoption report, including nested gate evidence.
    #[arg(long)]
    pub full: bool,
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
    #[arg(long, default_value = "", value_name = "TEXT")]
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
