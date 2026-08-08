use clap::{Args, Subcommand};
use std::collections::BTreeMap;

use homeboy::agents::agent_task_scheduler::AgentTaskCandidateCompletionPolicy;
use homeboy::agents::agent_tasks::gate::{
    AgentTaskGateEnvironmentMode, AgentTaskGateEnvironmentPolicy, AgentTaskGateExecutionPolicy,
    AgentTaskGateExtensionInput, AgentTaskGatePackageArtifactRequirement,
    AgentTaskGateRevealPolicy, AgentTaskGateToolchainRequirement, VerifyGateOptions,
};

use super::super::super::super::agent_task_dispatch::DispatchArgs;
use super::super::super::review;

#[derive(Args, Debug, Clone)]
pub struct VerifyGateArgs {
    /// Deterministic verification command that must pass before the cook
    /// promotes its work (e.g. `--verify "cargo fmt --check"`). Required unless
    /// `--private-verify` is given — a cook that cannot verify its work cannot
    /// promote it. Runs in the destination worktree. Repeat to require multiple
    /// gates; every one must pass. Its output is included in the review evidence.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,
    /// Like `--verify`, but the command's output is treated as private: only a
    /// pass/fail summary is revealed by default (see `--private-gate-reveal`).
    /// Satisfies the same mandatory-gate requirement as `--verify`. Use for
    /// gates whose logs may contain secrets. Repeatable.
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
    /// Gate scheduling policy: `ordered-fail-fast` (default) skips downstream
    /// gates after the first failure; `continue-all` runs every declared gate.
    #[arg(
        long = "gate-execution-policy",
        default_value = "ordered-fail-fast",
        value_name = "POLICY"
    )]
    #[arg(value_parser = ["ordered-fail-fast", "continue-all"])]
    pub gate_execution_policy: String,
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
    /// Maximum time, in seconds, a gate may run without a structured
    /// `HOMEBOY_PROGRESS` marker (default 300 = 5 min).
    #[arg(
        long = "gate-no-progress-timeout-seconds",
        default_value_t = 5 * 60,
        value_name = "SECONDS"
    )]
    pub gate_no_progress_timeout_seconds: u64,
    /// Re-run gates that already recorded a passing result on a previous
    /// attempt instead of reusing the recorded pass. Off by default.
    #[arg(long = "rerun-completed-gates")]
    pub rerun_completed_gates: bool,
    /// Finalize only when an inherited required-gate failure was reproduced on
    /// the immutable baseline. The gate remains reported as baseline-red.
    #[arg(long = "accept-inherited-failures")]
    pub accept_inherited_failures: bool,
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
    /// Preserve a required toolchain setting from the host as `NAME=SOURCE` or
    /// `NAME=SOURCE/relative/path`. The mapping is retained in gate evidence.
    #[arg(long = "gate-env-from", value_name = "NAME=SOURCE[/PATH]", value_parser = parse_gate_environment)]
    pub gate_environment_preserve: Vec<(String, String)>,
    /// Required executable to initialize before provider execution. Its probe is
    /// `COMMAND --version` in the final isolated gate environment. Repeatable.
    #[arg(long = "gate-toolchain", value_name = "COMMAND")]
    pub gate_toolchains: Vec<String>,
    /// Caller-declared package resource readiness as a JSON object. The object
    /// defines its environment mapping, required paths or digests, and opaque
    /// remediation metadata. Repeat for multiple resources.
    #[arg(long = "gate-package-artifact", value_name = "JSON", value_parser = parse_gate_package_artifact)]
    pub gate_package_artifacts: Vec<AgentTaskGatePackageArtifactRequirement>,
    /// Explicit extension input as a JSON object with `id` and absolute
    /// `source`. Only selected inputs are copied into isolated HOME.
    #[arg(long = "gate-extension-input", value_name = "JSON", value_parser = parse_gate_extension_input)]
    pub gate_extension_inputs: Vec<AgentTaskGateExtensionInput>,
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
            execution_policy: match args.gate_execution_policy.as_str() {
                "continue-all" => AgentTaskGateExecutionPolicy::ContinueAll,
                _ => AgentTaskGateExecutionPolicy::OrderedFailFast,
            },
            gate_timeout_seconds: args.gate_timeout_seconds,
            gate_heartbeat_interval_seconds: args.gate_heartbeat_interval_seconds,
            gate_no_progress_timeout_seconds: args.gate_no_progress_timeout_seconds,
            rerun_completed_gates: args.rerun_completed_gates,
            accept_inherited_failures: args.accept_inherited_failures,
            gate_environment: AgentTaskGateEnvironmentPolicy {
                mode: match args.gate_environment_mode.as_str() {
                    "replace" => AgentTaskGateEnvironmentMode::Replace,
                    _ => AgentTaskGateEnvironmentMode::Inherit,
                },
                variables: args
                    .gate_environment
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
                preserve: args
                    .gate_environment_preserve
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
                isolate_home: args.isolate_gate_home,
                isolate_xdg: args.isolate_gate_xdg,
                extension_inputs: args.gate_extension_inputs,
            },
            gate_toolchains: args
                .gate_toolchains
                .into_iter()
                .map(|command| AgentTaskGateToolchainRequirement {
                    command,
                    probe_arguments: vec!["--version".to_string()],
                })
                .collect(),
            gate_package_artifacts: args.gate_package_artifacts,
            gate_diagnostic_sidecars: Vec::new(),
            hydrate_dependencies: true,
        }
    }
}

fn parse_gate_package_artifact(
    value: &str,
) -> Result<AgentTaskGatePackageArtifactRequirement, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("invalid gate package artifact declaration: {error}"))
}

fn parse_gate_extension_input(value: &str) -> Result<AgentTaskGateExtensionInput, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("invalid gate extension input declaration: {error}"))
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
        assert_eq!(defaults.gate_no_progress_timeout_seconds, 5 * 60);
        assert!(!defaults.rerun_completed_gates);
        assert!(defaults.hydrate_dependencies);

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-timeout-seconds",
            "42",
            "--gate-heartbeat-interval-seconds",
            "7",
            "--gate-no-progress-timeout-seconds",
            "11",
            "--rerun-completed-gates",
        ])
        .expect("parse configured gate policy")
        .gates
        .into();
        assert_eq!(options.gate_timeout_seconds, 42);
        assert_eq!(options.gate_heartbeat_interval_seconds, 7);
        assert_eq!(options.gate_no_progress_timeout_seconds, 11);
        assert!(options.rerun_completed_gates);
        assert!(options.gate_environment.isolate_home);
        assert!(options.gate_environment.isolate_xdg);

        let options: VerifyGateOptions =
            TestCli::try_parse_from(["homeboy", "--gate-execution-policy", "continue-all"])
                .expect("parse continue-all gate policy")
                .gates
                .into();
        assert_eq!(
            options.execution_policy,
            AgentTaskGateExecutionPolicy::ContinueAll
        );

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-environment-mode",
            "replace",
            "--gate-env",
            "FEATURE=enabled",
            "--gate-extension-input",
            r#"{"id":"wordpress","source":"/opt/extensions/wordpress","identity":"sha256:content"}"#,
        ])
        .expect("parse gate environment")
        .gates
        .into();
        assert_eq!(
            options.gate_environment.mode,
            AgentTaskGateEnvironmentMode::Replace
        );
        assert_eq!(options.gate_environment.variables["FEATURE"], "enabled");
        assert_eq!(
            options.gate_environment.extension_inputs,
            vec![AgentTaskGateExtensionInput {
                id: "wordpress".to_string(),
                source: "/opt/extensions/wordpress".to_string(),
                identity: Some("sha256:content".to_string()),
            }]
        );

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
        assert!(help.contains("Workspace handle the cook edits"), "{help}");
        assert!(
            help.contains("Deterministic verification command"),
            "{help}"
        );
        assert!(help.contains("before opening the pull request"), "{help}");
    }

    #[test]
    fn cook_help_documents_explicit_execution_cap_precedence_over_configured_rotations() {
        let help = rendered_cook_help();
        assert!(
            help.contains("--max-attempts 1 --max-provider-executions 1"),
            "{help}"
        );
        assert!(
            help.contains("explicit `--max-provider-rotations`"),
            "{help}"
        );
    }

    #[test]
    fn cook_parser_preserves_an_explicit_execution_cap_without_a_rotation_override() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--max-attempts",
            "1",
            "--max-provider-executions",
            "1",
            "--no-finalize",
            "--prompt",
            "test",
            "--to-worktree",
            "repo@branch",
        ])
        .expect("parse an explicitly bounded Cook");
        let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("Cook command");
        };

        assert_eq!(cook.max_attempts, 1);
        assert_eq!(cook.dispatch.core.attempts, Some(1));
        assert_eq!(cook.dispatch.core.provider_rotations, None);
    }

    #[test]
    fn cook_help_exposes_quiet_progress_for_orchestration() {
        let help = rendered_cook_help();
        assert!(help.contains("--no-progress"), "{help}");
        assert!(
            help.contains("Suppress intermediate Cook progress"),
            "{help}"
        );
    }

    #[test]
    fn cook_cli_preflight_explains_the_default_provider_budget_conflict() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--max-attempts",
            "3",
            "--no-finalize",
            "--prompt",
            "test",
            "--to-worktree",
            "repo@branch",
        ])
        .expect("parse Cook with the default provider budget");
        let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("Cook command");
        };
        // Resolve through the real budget resolver with no configured rotation:
        // that is the shape this error message exists for.
        let core: homeboy::agents::agent_task_dispatch_service::DispatchCoreInputs =
            cook.dispatch.core.clone().into();
        let budget =
            homeboy::agents::agent_task_dispatch_plan::resolve_execution_budget(&core, None);
        assert_eq!(budget.max_provider_executions, 1);
        assert_eq!(budget.max_provider_rotations, 0);

        let error = homeboy::agents::agent_task_service::validate_effective_cook_budget(
            cook.max_attempts,
            &budget,
        )
        .expect_err("default provider budget must not silently discard Cook retries");
        assert!(
            error
                .message
                .contains("--max-provider-executions 3 --max-same-provider-retries 2"),
            "{}",
            error.message
        );
    }

    #[test]
    fn cook_help_advertises_one_prompt_source_not_wave_inputs() {
        let help = rendered_cook_help();
        assert!(help.contains("--prompt"), "{help}");
        assert!(!help.contains("--task <"), "{help}");
        assert!(!help.contains("--tasks <"), "{help}");
    }

    #[test]
    fn cook_accepts_issue_backed_destination_derivation_and_explicit_override() {
        let derived = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/11225",
            "--no-finalize",
        ])
        .expect("issue-backed Cook parses without an explicit destination");
        let crate::cli_surface::Commands::AgentTask(agent_task) = derived.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(derived) = agent_task.command else {
            panic!("Cook command");
        };
        assert_eq!(derived.to_worktree, None);

        let explicit = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "homeboy@existing",
            "--no-finalize",
        ])
        .expect("explicit Cook destination parses");
        let crate::cli_surface::Commands::AgentTask(agent_task) = explicit.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(explicit) = agent_task.command else {
            panic!("Cook command");
        };
        assert_eq!(explicit.to_worktree.as_deref(), Some("homeboy@existing"));

        let draft = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "homeboy@existing",
            "--draft-pr",
        ])
        .expect("draft Cook parses");
        let crate::cli_surface::Commands::AgentTask(agent_task) = draft.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(draft) = agent_task.command else {
            panic!("Cook command");
        };
        assert!(draft.draft_pr);

        assert!(crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "homeboy@existing",
            "--no-finalize",
            "--draft-pr",
        ])
        .is_err());
    }
}

#[derive(Args, Debug, Clone)]
pub struct AgentTaskCookArgs {
    #[command(flatten)]
    pub dispatch: DispatchArgs,
    /// Completion rule for isolated candidates: wait for all results (default)
    /// or promote the first successful candidate.
    #[arg(long, default_value_t = AgentTaskCandidateCompletionPolicy::WaitAll, value_name = "POLICY")]
    pub candidate_completion: AgentTaskCandidateCompletionPolicy,
    #[arg(long, hide = true)]
    pub attempt_run_id: Option<String>,
    #[arg(long, hide = true)]
    pub attempt_plan: Option<String>,
    /// One-line statement of what a successful cook must achieve. Recorded as
    /// framing metadata for the provider task and used for review. Without
    /// --prompt, it supplies the one provider task.
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,
    /// Workspace handle the cook edits, verifies, and finalizes into. The handle
    /// is `<repo>@<branch-slug>`, where the slug replaces every character of
    /// --head outside [A-Za-z0-9_-] with `-`, so branch `fix/1234-x` is handle
    /// `repo@fix-1234-x`. Existing destinations are reused. Creating a missing
    /// one is not a built-in capability: it requires an enabled worktree
    /// provider with a `commands.ensure` argv template, and without one you must
    /// create the destination first with `homeboy worktree create`. When
    /// omitted, --repo plus --task-url derives an issue-owned destination
    /// through that same configured provider. An explicit --workspace or --cwd
    /// Git checkout can infer --repo when its remote maps to exactly one
    /// configured component; an explicit --repo must match that checkout.
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
        long_help = "Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
    )]
    pub provider_argv: Vec<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Maximum Cook attempts before giving up. Each attempt re-runs the agent
    /// and gates; a later attempt can recover from a transient failure. This
    /// derives provider execution and same-provider remediation budgets. A
    /// configured provider rotation receives its own additional execution
    /// allowance unless an advanced budget flag explicitly caps it (default 3).
    #[arg(
        long = "max-attempts",
        default_value_t = 3,
        value_parser = clap::value_parser!(u32).range(1..),
        value_name = "N"
    )]
    pub max_attempts: u32,
    /// Stop after the work is verified but before opening the pull request,
    /// leaving the committed change on the worktree branch for manual review or
    /// a later `agent-task review`/finalize.
    #[arg(long = "no-finalize")]
    pub no_finalize: bool,
    /// Complete normal verified finalization but create a draft pull request.
    /// Existing pull requests retain their current draft or ready state.
    #[arg(long = "draft-pr", conflicts_with = "no_finalize")]
    pub draft_pr: bool,
    /// Return the complete cook report, including nested promotion and gate evidence.
    #[arg(long)]
    pub full: bool,
    /// Suppress intermediate Cook progress lines after the durable run identity.
    /// The final result still contains status and evidence commands for orchestration.
    #[arg(long)]
    pub no_progress: bool,
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
    /// Require a separate durable acceptance verdict before PR finalization.
    #[arg(long)]
    pub require_acceptance: bool,
    /// Authority allowed to issue the acceptance verdict.
    #[arg(long, requires = "require_acceptance")]
    pub acceptance_authority: Option<String>,
    /// Policy the acceptance authority applies.
    #[arg(long, requires = "require_acceptance")]
    pub acceptance_policy: Option<String>,
    /// Controller-resolved repository identity for a supplied checkout. This is
    /// not caller input: Cook persists it with the compiled plan.
    #[arg(skip)]
    pub repository_identity: Option<serde_json::Value>,
}

#[derive(Args, Clone, Debug)]
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
    /// Define or update a durable loop from a spec.
    ///
    /// `--on`/`--off` set whether the loop runs; `--revolution-limit` bounds how
    /// many revolutions it may take before it stops on its own.
    Define(AgentTaskLoopDefineArgs),
    /// Read durable loop state: on/off, revolutions taken, and continuation policy.
    Status(AgentTaskLoopStatusArgs),
    /// Resume a stopped or exhausted durable loop, optionally raising its
    /// revolution limit.
    Resume(AgentTaskLoopResumeArgs),
    /// Stop a durable loop and record the handoff.
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
