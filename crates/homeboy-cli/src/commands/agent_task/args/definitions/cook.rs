use clap::{Args, Subcommand};
use std::collections::BTreeMap;

use homeboy::agents::agent_tasks::gate::{
    AgentTaskGateEnvironmentMode, AgentTaskGateEnvironmentPolicy, AgentTaskGateRevealPolicy,
    VerifyGateOptions,
};

use super::super::super::super::agent_task_dispatch::DispatchArgs;
use super::super::super::review;

#[derive(Args, Debug, Clone)]
pub struct VerifyGateArgs {
    /// Deterministic verification command that must pass before the cook
    /// promotes its work (e.g. `--verify "cargo fmt --check"`). Runs in the
    /// destination worktree. Repeat to require multiple gates; every one must
    /// pass. Its output is included in the review evidence.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,
    /// Like `--verify`, but the command's output is treated as private: only a
    /// pass/fail summary is revealed by default (see `--private-gate-reveal`).
    /// Use for gates whose logs may contain secrets. Repeatable.
    #[arg(long = "private-verify", value_name = "COMMAND")]
    pub private_verify: Vec<String>,
    /// How much of a `--private-verify` gate's output to reveal: `summary-only`
    /// (default) shows just pass/fail; other policies expose more detail.
    #[arg(
        long = "private-gate-reveal",
        default_value = "summary-only",
        value_name = "POLICY"
    )]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
    /// Wall-clock timeout, in seconds, for each verification gate command
    /// (default 1800 = 30 min). A gate exceeding this fails.
    #[arg(long = "gate-timeout-seconds", default_value_t = 30 * 60, value_name = "SECONDS")]
    pub gate_timeout_seconds: u64,
    /// How often, in seconds, to emit a heartbeat while a gate runs so long
    /// gates are not mistaken for a stalled cook (default 5).
    #[arg(
        long = "gate-heartbeat-interval-seconds",
        default_value_t = 5,
        value_name = "SECONDS"
    )]
    pub gate_heartbeat_interval_seconds: u64,
    /// Re-run gates that already recorded a passing result on a previous
    /// attempt instead of reusing the recorded pass. Off by default.
    #[arg(long = "rerun-completed-gates")]
    pub rerun_completed_gates: bool,
    /// Environment for gate commands: `inherit` (default) extends the current
    /// environment; `replace` starts from an empty environment plus `--gate-env`.
    #[arg(
        long = "gate-environment-mode",
        default_value = "inherit",
        value_name = "MODE"
    )]
    #[arg(value_parser = ["inherit", "replace"])]
    pub gate_environment_mode: String,
    /// Extra environment variable for gate commands, as `NAME=VALUE`. Repeatable.
    #[arg(long = "gate-env", value_name = "NAME=VALUE", value_parser = parse_gate_environment)]
    pub gate_environment: Vec<(String, String)>,
    /// Run gates with an isolated `$HOME` so gate side effects do not touch the
    /// operator's home directory (default true).
    #[arg(
        long = "isolate-gate-home",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub isolate_gate_home: bool,
    /// Run gates with isolated XDG base directories so gate side effects do not
    /// touch the operator's config/cache/data dirs (default true).
    #[arg(
        long = "isolate-gate-xdg",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub isolate_gate_xdg: bool,
}
impl VerifyGateArgs {
    pub fn has_deterministic_gate(&self) -> bool {
        !self.verify.is_empty() || !self.private_verify.is_empty()
    }
}
impl From<VerifyGateArgs> for VerifyGateOptions {
    fn from(args: VerifyGateArgs) -> Self {
        Self {
            verify: args.verify,
            private_verify: args.private_verify,
            private_gate_reveal: args.private_gate_reveal,
            gate_timeout_seconds: args.gate_timeout_seconds,
            gate_heartbeat_interval_seconds: args.gate_heartbeat_interval_seconds,
            rerun_completed_gates: args.rerun_completed_gates,
            gate_environment: AgentTaskGateEnvironmentPolicy {
                mode: match args.gate_environment_mode.as_str() {
                    "replace" => AgentTaskGateEnvironmentMode::Replace,
                    _ => AgentTaskGateEnvironmentMode::Inherit,
                },
                variables: args
                    .gate_environment
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
                isolate_home: args.isolate_gate_home,
                isolate_xdg: args.isolate_gate_xdg,
            },
        }
    }
}

fn parse_gate_environment(value: &str) -> Result<(String, String), String> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| "expected NAME=VALUE".to_string())?;
    if name.is_empty() || name.contains('=') {
        return Err("environment variable name must not be empty or contain '='".to_string());
    }
    Ok((name.to_string(), value.to_string()))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        gates: VerifyGateArgs,
    }

    #[test]
    fn gate_policy_cli_defaults_and_overrides_round_trip_to_typed_options() {
        let defaults = TestCli::try_parse_from(["homeboy"])
            .expect("parse default gate policy")
            .gates;
        let defaults: VerifyGateOptions = defaults.into();
        assert_eq!(defaults.gate_timeout_seconds, 30 * 60);
        assert_eq!(defaults.gate_heartbeat_interval_seconds, 5);
        assert!(!defaults.rerun_completed_gates);

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-timeout-seconds",
            "42",
            "--gate-heartbeat-interval-seconds",
            "7",
            "--rerun-completed-gates",
        ])
        .expect("parse configured gate policy")
        .gates
        .into();
        assert_eq!(options.gate_timeout_seconds, 42);
        assert_eq!(options.gate_heartbeat_interval_seconds, 7);
        assert!(options.rerun_completed_gates);
        assert!(options.gate_environment.isolate_home);
        assert!(options.gate_environment.isolate_xdg);

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-environment-mode",
            "replace",
            "--gate-env",
            "FEATURE=enabled",
        ])
        .expect("parse gate environment")
        .gates
        .into();
        assert_eq!(
            options.gate_environment.mode,
            AgentTaskGateEnvironmentMode::Replace
        );
        assert_eq!(options.gate_environment.variables["FEATURE"], "enabled");

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--isolate-gate-home",
            "false",
            "--isolate-gate-xdg",
            "false",
        ])
        .expect("parse gate isolation opt-outs")
        .gates
        .into();
        assert!(!options.gate_environment.isolate_home);
        assert!(!options.gate_environment.isolate_xdg);
    }

    #[derive(Parser)]
    struct CookHelpCli {
        #[command(flatten)]
        cook: AgentTaskCookArgs,
    }

    fn rendered_cook_help() -> String {
        use clap::CommandFactory;
        CookHelpCli::command().render_long_help().to_string()
    }

    #[test]
    fn cook_help_does_not_leak_internal_refactoring_prose() {
        // #9898/#9907: help must describe the operator contract, never the Rust
        // refactor behind the flags.
        let help = rendered_cook_help();
        for leaked in [
            "Flattened into",
            "#[arg] attributes",
            "DispatchArgs",
            "field group is declared once",
            "reproduce the original flag",
            "CLI surface for the dispatch inputs",
        ] {
            assert!(
                !help.contains(leaked),
                "cook help leaked internal prose {leaked:?}:\n{help}"
            );
        }
    }

    #[test]
    fn cook_help_documents_core_workflow_flags() {
        let help = rendered_cook_help();
        // Each core flag renders with operator-facing help, not a blank line.
        assert!(help.contains("--goal"), "{help}");
        assert!(
            help.contains("Workspace handle of the existing worktree"),
            "{help}"
        );
        assert!(
            help.contains("Deterministic verification command"),
            "{help}"
        );
        assert!(help.contains("before opening the pull request"), "{help}");
    }
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskCookArgs {
    #[command(flatten)]
    pub dispatch: DispatchArgs,
    #[arg(long, hide = true)]
    pub attempt_run_id: Option<String>,
    #[arg(long, hide = true)]
    pub attempt_plan: Option<String>,
    /// One-line statement of what a successful cook must achieve. Recorded with
    /// the run and used to frame the agent's task and the review.
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,
    /// Workspace handle of the existing worktree the cook edits, verifies, and
    /// finalizes into (e.g. `repo@branch-slug`). This checkout is authoritative:
    /// the agent's changes, the `--verify` gates, and the resulting PR all
    /// operate on it. Create it first with `workspace worktree add`.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,
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
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Maximum cook attempts before giving up. Each attempt re-runs the agent
    /// and gates; a later attempt can recover from a transient failure
    /// (default 3).
    #[arg(long = "max-attempts", default_value_t = 3, value_name = "N")]
    pub max_attempts: u32,
    /// Stop after the work is verified but before opening the pull request,
    /// leaving the committed change on the worktree branch for manual review or
    /// a later `agent-task review`/finalize.
    #[arg(long = "no-finalize")]
    pub no_finalize: bool,
    /// Return the complete cook report, including nested promotion and gate evidence.
    #[arg(long)]
    pub full: bool,
    /// Base branch the finalized pull request targets and the branch changes are
    /// diffed against (default `main`).
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,
    /// Head branch to push and open the PR from. Defaults to the branch the
    /// destination worktree is already on.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,
    /// Title for the finalized pull request. Defaults to a title derived from
    /// the goal / commit.
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Commit message for the cook's committed change. Defaults to a message
    /// derived from the goal.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: Option<String>,
    /// Branch names the cook refuses to push to or target directly, as a safety
    /// guard. Repeatable; defaults to the standard protected set.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,
    /// AI tool disclosure recorded in the PR's assistance attribution
    /// (default `AI-assisted`).
    #[arg(long, default_value = "AI-assisted", value_name = "TEXT")]
    pub ai_tool: String,
    /// Legacy AI-usage disclosure. The reviewer-facing "Used for" text is now
    /// authored by the agent's `review_form.used_for` (a self-reflective process
    /// description) and validated by the cook loop's review-form gate; this flag
    /// no longer feeds the PR body. Retained only for recipe back-compatibility
    /// and defaults empty (no canned platitude).
    #[arg(long, default_value = "", value_name = "TEXT")]
    pub ai_used_for: String,
}

#[derive(Args, Debug)]
pub struct PromotionProviderArgs {
    #[arg(long, value_name = "PATH")]
    pub workspace: String,
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopArgs {
    #[command(subcommand)]
    pub command: AgentTaskLoopCommand,
}
#[derive(Subcommand, Debug)]
pub enum AgentTaskLoopCommand {
    Define(AgentTaskLoopDefineArgs),
    Status(AgentTaskLoopStatusArgs),
    Resume(AgentTaskLoopResumeArgs),
    Stop(AgentTaskLoopStatusArgs),
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopDefineArgs {
    #[arg(value_name = "SPEC")]
    pub spec: String,
    #[arg(long, conflicts_with = "off")]
    pub on: bool,
    #[arg(long, conflicts_with = "on")]
    pub off: bool,
    #[arg(long = "revolution-limit", value_name = "N")]
    pub revolution_limit: Option<u32>,
    #[arg(long)]
    pub resume: bool,
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopStatusArgs {
    pub loop_id: String,
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopResumeArgs {
    pub loop_id: String,
    #[arg(long = "revolution-limit", value_name = "N")]
    pub revolution_limit: Option<u32>,
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}
