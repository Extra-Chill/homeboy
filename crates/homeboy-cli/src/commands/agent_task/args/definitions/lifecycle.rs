use clap::Args;

use super::super::super::review;
use super::cook::VerifyGateArgs;

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// Agent-task plan as a JSON spec: inline JSON, `@FILE` to read a file, or
    /// `-` to read stdin. A bare path is NOT accepted — use `@/path/plan.json`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub plan: String,
    /// Durable run ID to record for this planned lifecycle.
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
    /// Maximum execution time in milliseconds.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Exact durable run id to execute. Use this to bypass older queued work.
    pub run_id: String,
    /// Maximum execution time in milliseconds.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,
}
#[derive(Args, Debug)]
pub struct RunNextArgs {
    /// Claim only queued child runs belonging to this durable fanout.
    #[arg(long, value_name = "ID")]
    pub fanout: Option<String>,
}
#[derive(Args, Debug)]
pub struct SubmitArgs {
    /// Agent-task plan as a JSON spec: inline JSON, `@FILE` to read a file, or
    /// `-` to read stdin. A bare path is NOT accepted — use `@/path/plan.json`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub plan: String,
    /// Optional durable run ID for the submitted plan.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}
#[derive(Args, Debug)]
pub struct ValidatePlanArgs {
    /// Agent-task plan as inline JSON, `@FILE`, or `-`. Validation creates no lifecycle record.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub plan: String,
}
#[derive(Args, Debug)]
pub struct LifecycleReadArgs {
    /// Durable run or Cook ID to inspect.
    pub run_id: String,
    /// Return complete lifecycle details instead of the bounded summary.
    #[arg(long)]
    pub full: bool,
}

#[derive(Args, Debug)]
pub struct ResumeArgs {
    /// Durable run or Cook ID to resume.
    pub run_id: String,
    /// Return complete lifecycle details instead of the bounded summary.
    #[arg(long)]
    pub full: bool,
    /// Stable key used to replay this resume without executing it twice.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}

#[cfg_attr(test, derive(Default))]
#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Durable run or Cook ID whose status to inspect.
    pub run_id: String,
    /// Inspect this exact lifecycle record instead of resolving a Cook ID to its current attempt.
    #[arg(long)]
    pub exact: bool,
    /// Exit nonzero when the inspected Cook needs follow-up action.
    ///
    /// Normal status reads report their own success independently from the
    /// subject lifecycle state. This preserves the former exit-code behavior
    /// for scripts that deliberately gate on an actionable Cook.
    #[arg(long)]
    pub strict_subject_exit: bool,
    /// Follow this durable status until it reaches a terminal state or the timeout expires.
    #[arg(long)]
    pub watch: bool,
    /// Delay between status reads while following. Accepts ms, s, m, h, or d.
    #[arg(
        long,
        default_value = "5s",
        value_name = "DURATION",
        requires = "watch"
    )]
    pub interval: String,
    /// Total time to follow before returning the latest partial status. Accepts ms, s, m, h, or d.
    #[arg(
        long,
        default_value = "30m",
        value_name = "DURATION",
        requires = "watch"
    )]
    pub timeout: String,
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    /// Durable run or Cook ID whose logs to retrieve.
    pub run_id: String,
    /// Resume events after this opaque cursor.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
}
#[derive(Args, Debug)]
pub struct EvidenceArgs {
    /// Durable run or Cook ID whose evidence to retrieve.
    pub run_id: String,
    /// Restrict results to this evidence kind.
    #[arg(long = "kind", value_name = "KIND")]
    pub kind: Option<String>,
    /// Restrict results to this task ID.
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,
    /// Return only evidence associated with failures.
    #[arg(long = "failure-only")]
    pub failure_only: bool,
    /// Return every matching evidence record rather than the bounded preview.
    #[arg(long)]
    pub full: bool,
}
#[derive(Args, Debug)]
pub struct DiagnoseArgs {
    /// Durable run or Cook ID to diagnose.
    pub run_id: String,
    /// Hydrate every available evidence summary rather than the bounded preview.
    #[arg(long)]
    pub full: bool,
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
    use clap::{CommandFactory, Parser};

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
    fn model_parses_canonically_for_adoption_and_ai_model_remains_compatible() {
        for flag in ["--model", "--ai-model"] {
            let cli = Cli::try_parse_from([
                "homeboy",
                "agent-task",
                "adopt",
                "cook-a",
                "--candidate-ref",
                "0123456789abcdef0123456789abcdef01234567",
                flag,
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
    }

    #[test]
    fn model_parses_canonically_for_manual_finalization_and_ai_model_remains_compatible() {
        for flag in ["--model", "--ai-model"] {
            let cli = Cli::try_parse_from([
                "homeboy",
                "agent-task",
                "finalize-pr",
                "--manual-finalization",
                "--run-id",
                "manual-model",
                "--path",
                "/tmp/manual-model",
                "--title",
                "Manual model",
                "--commit-message",
                "record model",
                flag,
                "openai/gpt-5.6-sol",
            ])
            .expect("manual finalization model parses");
            let Commands::AgentTask(agent_task) = cli.command else {
                panic!("expected agent-task command");
            };
            let AgentTaskCommand::FinalizePr(args) = agent_task.command else {
                panic!("expected finalize-pr command");
            };
            assert_eq!(
                args.evidence.ai_model.as_deref(),
                Some("openai/gpt-5.6-sol")
            );
        }
    }

    #[test]
    fn recovery_rejects_model_overrides_so_durable_provenance_wins() {
        for flag in ["--model", "--ai-model"] {
            assert!(
                Cli::try_parse_from([
                    "homeboy",
                    "agent-task",
                    "finalize-pr",
                    "--recover",
                    "cook-a",
                    flag,
                    "openai/gpt-5.6-sol",
                ])
                .is_err(),
                "recovery must reject {flag} instead of silently discarding it"
            );
        }
    }

    #[test]
    fn manual_finalization_parses_component_selector_and_recovery_rejects_it() {
        let help = Cli::command()
            .find_subcommand("agent-task")
            .expect("agent-task command")
            .find_subcommand("finalize-pr")
            .expect("finalize-pr command")
            .clone()
            .render_long_help()
            .to_string();
        assert!(help.contains("--component <COMPONENT_ID>"), "{help}");
        assert!(help.contains("shared-repository worktree"), "{help}");

        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "finalize-pr",
            "--manual-finalization",
            "--run-id",
            "manual-component",
            "--path",
            "/tmp/manual-component",
            "--component",
            "nested-component",
            "--title",
            "Manual component",
            "--commit-message",
            "record component",
        ])
        .expect("manual finalization component parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::FinalizePr(args) = agent_task.command else {
            panic!("expected finalize-pr command");
        };
        assert_eq!(args.component.as_deref(), Some("nested-component"));

        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "finalize-pr",
            "--recover",
            "cook-a",
            "--component",
            "nested-component",
        ])
        .is_err());
    }

    #[test]
    fn status_exact_selects_concrete_record_and_cannot_bridge() {
        let cli = Cli::try_parse_from(["homeboy", "agent-task", "status", "cook-a", "--exact"])
            .expect("exact status parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Status(args) = agent_task.command else {
            panic!("expected status command");
        };
        assert!(args.exact);
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "cook-a",
            "--exact",
            "--bridge",
        ])
        .is_err());
    }

    #[test]
    fn status_rejects_the_removed_bridge_flag() {
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "cook-attempt-2",
            "--bridge",
        ])
        .is_err());
    }

    #[test]
    fn status_rejects_removed_projection_flags() {
        for flag in ["--full", "--bounded", "--no-runner-probe", "--since-cursor"] {
            let mut argv = vec!["homeboy", "agent-task", "status", "run-a", flag];
            if flag == "--since-cursor" {
                argv.push("1");
            }
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "accepted removed {flag}"
            );
        }
    }

    #[test]
    fn strict_subject_exit_is_status_only() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--strict-subject-exit",
        ])
        .expect("status compatibility flag parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Status(args) = agent_task.command else {
            panic!("expected status command");
        };
        assert!(args.strict_subject_exit);

        for command in ["artifacts", "resume"] {
            assert!(
                Cli::try_parse_from([
                    "homeboy",
                    "agent-task",
                    command,
                    "run-a",
                    "--strict-subject-exit",
                ])
                .is_err(),
                "{command} must reject the status-only compatibility flag"
            );
        }
    }

    #[test]
    fn status_watch_uses_bounded_duration_flags() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--watch",
            "--interval",
            "250ms",
            "--timeout",
            "2m",
        ])
        .expect("watch status parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Status(args) = agent_task.command else {
            panic!("expected status command");
        };
        assert!(args.watch);
        assert_eq!(args.interval, "250ms");
        assert_eq!(args.timeout, "2m");
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--interval",
            "1s",
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "status",
            "run-a",
            "--timeout",
            "1m",
        ])
        .is_err());
    }

    #[test]
    fn promote_and_finalize_pr_parse_full_output() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "promote",
            "run-a",
            "--to-worktree",
            "repo@task",
            "--full",
            "--idempotency-key",
            "promote-1",
        ])
        .expect("promote full parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent task")
        };
        let AgentTaskCommand::Promote(args) = agent_task.command else {
            panic!("promote")
        };
        assert!(args.full);
        assert_eq!(args.idempotency_key.as_deref(), Some("promote-1"));

        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "finalize-pr",
            "--recover",
            "run-a",
            "--full",
        ])
        .expect("finalize full parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent task")
        };
        let AgentTaskCommand::FinalizePr(args) = agent_task.command else {
            panic!("finalize")
        };
        assert!(args.full);
    }
}
#[derive(Args, Debug)]
pub struct ReplayProviderBoundaryArgs {
    /// Durable run whose provider boundary to replay.
    pub run_id: String,
    /// Restrict the replay to this task ID.
    #[arg(long = "task", value_name = "TASK_ID")]
    pub task: Option<String>,
}
#[derive(Args, Debug)]
pub struct RetryArgs {
    /// Durable run or Cook ID to retry.
    pub run_id: String,
    /// Durable ID to assign to the new retry run.
    #[arg(long, value_name = "ID")]
    pub new_run_id: Option<String>,
    /// Execute the retry immediately after creating it.
    #[arg(long)]
    pub run: bool,
    /// Permit a new retry after every prior retry in this lineage is terminal.
    #[arg(long, visible_alias = "allow-duplicate")]
    pub force: bool,
    /// Stable caller key for safely replaying this retry reservation.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
    /// Backend for the next Cook attempt. This explicit route change is recorded
    /// with its prior route and operator authority in the Cook lineage.
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,
    /// Provider-specific selector for the next Cook attempt.
    #[arg(long, visible_alias = "provider-id", value_name = "SELECTOR")]
    pub selector: Option<String>,
    /// Model for the next Cook attempt. A model override pins provider rotation
    /// unless --allow-provider-rotation or a positive --provider-rotations is also supplied.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,
    /// Re-enable configured provider/model rotation for this overridden route.
    #[arg(long)]
    pub allow_provider_rotation: bool,
    /// Explicit cross-provider/model rotations available after this override.
    #[arg(long, value_name = "N")]
    pub provider_rotations: Option<u32>,
}

#[cfg(test)]
mod retry_tests {
    use clap::{CommandFactory, Parser};

    use crate::{
        cli_surface::{Cli, Commands},
        commands::agent_task::AgentTaskCommand,
    };

    #[test]
    fn retry_help_and_parser_expose_cook_route_recovery() {
        let help = Cli::command()
            .find_subcommand("agent-task")
            .expect("agent-task command")
            .find_subcommand("retry")
            .expect("retry command")
            .clone()
            .render_long_help()
            .to_string();
        assert!(help.contains("--backend"), "{help}");
        assert!(help.contains("--allow-provider-rotation"), "{help}");
        assert!(help.contains("operator authority"), "{help}");

        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "retry",
            "cook-a",
            "--model",
            "replacement-model",
            "--provider-rotations",
            "2",
        ])
        .expect("route override parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Retry(args) = agent_task.command else {
            panic!("expected retry command");
        };
        assert_eq!(args.model.as_deref(), Some("replacement-model"));
        assert_eq!(args.provider_rotations, Some(2));
    }
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Durable run or Cook ID to cancel.
    pub run_id: String,
    /// Optional explanation recorded with the cancellation.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Stable caller key for safely replaying this cancellation request.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}
#[derive(Args, Debug)]
pub struct QuarantineArgs {
    /// Exact durable run id. Cook aliases are not accepted for mutations.
    pub run_id: String,
    /// Explanation recorded with the quarantine action.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,
}
#[derive(Args, Debug)]
pub struct RearmArgs {
    /// Exact durable run id. Cook aliases are not accepted for mutations.
    pub run_id: String,
}
#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// Durable run or Cook ID to review.
    pub run_id: String,
    /// Include complete lifecycle, promotion, and gate evidence. The default
    /// keeps one actionable candidate and bounded gate findings.
    #[arg(long)]
    pub full: bool,
    /// Target managed worktree handle for the review candidate.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: Option<String>,
    /// Deprecated shell command for the promotion apply provider.
    #[arg(
        long,
        value_name = "COMMAND",
        long_help = "Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`."
    )]
    pub provider_command: Option<String>,
    /// Exact argv element for the promotion apply provider; repeat per element.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command",
        long_help = "Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
    )]
    pub provider_argv: Vec<String>,
}
#[derive(Args, Debug)]
pub struct PromoteArgs {
    /// Durable run or Cook ID that supplies the promotion candidate.
    pub source: String,
    /// Target managed worktree handle for the promoted candidate.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,
    /// Declared base branch resolved immediately before promotion gates run.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,
    /// Deprecated shell command for the promotion apply provider.
    #[arg(
        long,
        value_name = "COMMAND",
        long_help = "Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`."
    )]
    pub provider_command: Option<String>,
    /// Exact argv element for the promotion apply provider; repeat per element.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command",
        long_help = "Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
    )]
    pub provider_argv: Vec<String>,
    /// Restrict promotion to this task ID.
    #[arg(long, value_name = "TASK_ID")]
    pub task_id: Option<String>,
    /// Restrict promotion to this artifact ID.
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub artifact_id: Option<String>,
    /// Validate the promotion without applying it.
    #[arg(long)]
    pub dry_run: bool,
    /// Include complete promotion and gate evidence.
    #[arg(long)]
    pub full: bool,
    /// Stable key used to replay this promotion without applying it twice.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
    /// Replay the exact gate policy from the source run's durable Cook recipe.
    /// Homeboy-generated review commands use this reference so private gate
    /// programs remain outside reviewer-facing command output.
    #[arg(long = "gates-from-cook-recipe")]
    pub gates_from_cook_recipe: bool,
    /// Verification gate configuration to run before promotion.
    #[command(flatten)]
    pub gates: VerifyGateArgs,
}
#[derive(Args, Debug)]
pub struct AdoptArgs {
    /// Existing durable Cook id or one of its declared attempt run ids whose recipe owns the candidate lifecycle.
    #[arg(value_name = "RUN_OR_COOK_ID")]
    pub run_or_cook_id: String,
    /// Select an exact durable attempt from the resolved Cook recipe. Required when attempts use different policies.
    #[arg(long, value_name = "N")]
    pub attempt: Option<u32>,
    /// Immutable commit revision in the recorded source worktree.
    #[arg(long, value_name = "SHA")]
    pub candidate_ref: String,
    /// Concrete model that prepared the externally supplied candidate. Use
    /// `--model`; `--ai-model` is a deprecated compatibility alias and will be
    /// removed in the next minor release.
    #[arg(long = "model", alias = "ai-model", value_name = "MODEL")]
    pub ai_model: Option<String>,
    /// Replace a stale interrupted adoption while retaining its lifecycle evidence.
    #[arg(long)]
    pub replace_interrupted: bool,
    /// Permit finalization only when a failed recorded gate reproduces with the
    /// same bounded fingerprint on the immutable candidate base. New or changed
    /// failures remain blocking and inherited-red evidence remains in the report.
    #[arg(long)]
    pub accept_inherited_failures: bool,
    /// Return the complete cook adoption report, including nested gate evidence.
    #[arg(long)]
    pub full: bool,
}
#[derive(Args, Debug)]
pub struct FinalizePrArgs {
    /// Include complete finalization and gate evidence.
    #[arg(long)]
    pub full: bool,
    /// Hydrate finalization from a durable Cook recipe or a validated manual-finalization record.
    #[arg(
        long,
        value_name = "RUN_OR_COOK_ID",
        conflicts_with = "manual_finalization"
    )]
    pub recover: Option<String>,
    /// Durable run ID for a manual finalization record.
    #[arg(long, value_name = "ID", required_unless_present = "recover")]
    pub run_id: Option<String>,
    /// Worktree path containing the manual finalization candidate.
    #[arg(long, value_name = "PATH", required_unless_present = "recover")]
    pub path: Option<String>,
    /// Registered component identity for disambiguating a shared-repository worktree.
    #[arg(long, value_name = "COMPONENT_ID", conflicts_with = "recover")]
    pub component: Option<String>,
    /// Base branch for the manual finalization candidate.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,
    /// Immutable base commit SHA recorded before the declared verification gates ran.
    #[arg(long, value_name = "SHA")]
    pub verified_base_sha: Option<String>,
    /// Head branch for the manual finalization candidate.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,
    /// Pull request title for the manual finalization candidate.
    #[arg(long, value_name = "TEXT", required_unless_present = "recover")]
    pub title: Option<String>,
    /// Commit message for the manual finalization candidate.
    #[arg(long, value_name = "TEXT", required_unless_present = "recover")]
    pub commit_message: Option<String>,
    /// Reviewer-facing evidence for the finalization dossier.
    #[command(flatten)]
    pub evidence: review::FinalizePrEvidenceArgs,
    /// Recorded result for a verification gate: `NAME=STATUS[:DETAIL]`.
    #[arg(long = "gate-result", value_name = "NAME=STATUS[:DETAIL]")]
    pub gate_results: Vec<String>,
    /// Execute a deterministic verification command against the committed manual candidate. Repeat for multiple gates.
    #[arg(long, value_name = "COMMAND")]
    pub verify: Vec<String>,
    /// Changed file path to include in the finalization dossier.
    #[arg(long = "changed-file", value_name = "PATH")]
    pub changed_files: Vec<String>,
    /// Branch that must not be updated by finalization; repeat for multiple branches.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,
    /// Description of how AI was used for the finalization.
    #[arg(long, default_value = "", value_name = "TEXT")]
    pub ai_used_for: String,
    /// Summary of the finalization candidate.
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,
    /// User-visible change description; repeat for multiple entries.
    #[arg(long = "what-changed", value_name = "TEXT")]
    pub what_changed: Vec<String>,
    /// Reviewer test step. Strict shape: COMMAND=>EXPECTED.
    #[arg(long = "test-step", value_name = "COMMAND=>EXPECTED")]
    pub test_steps: Vec<String>,
    /// Compatibility notes for the finalization candidate.
    #[arg(long, value_name = "TEXT")]
    pub compatibility: Option<String>,
    /// Closing issue reference: #NUMBER, OWNER/REPO#NUMBER, or a github.com issue URL.
    #[arg(long = "closes", value_name = "ISSUE_REF")]
    pub closes: Vec<String>,
    /// Related issue reference: #NUMBER, OWNER/REPO#NUMBER, or a github.com issue URL.
    #[arg(long = "relates-to", value_name = "ISSUE_REF")]
    pub relates_to: Vec<String>,
    /// Explicit reviewer override in `TARGET=VALUE@PROVENANCE` form.
    #[arg(long = "review-override", value_name = "TARGET=VALUE@PROVENANCE")]
    pub review_overrides: Vec<String>,
    /// Validate the complete hydrated dossier and candidate without publishing.
    #[arg(long)]
    pub preflight: bool,
    /// Publish corrected, independently verified work without a promotion lineage. The ID must identify a failed attempt (a Cook ID resolves to its newest attempt, which must be failed), or be unused so Homeboy can reserve a durable manual-finalization record for its intent and receipt.
    #[arg(long)]
    pub manual_finalization: bool,
}
#[derive(Args, Debug)]
pub struct RecordReplacementGateProofArgs {
    /// Durable Cook attempt whose applied candidate has infrastructure-invalid gates.
    pub run_id: String,
    /// Complete typed promotion report from the replacement gate executor: inline JSON, `@FILE`, or `-`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub promotion: String,
    /// Explicit operator authorization for externally produced proof.
    #[arg(long, value_name = "TEXT")]
    pub authorize_external_proof: Option<String>,
    /// Accept candidate failures proven identical against the immutable base.
    #[arg(long)]
    pub accept_inherited_failures: bool,
}
#[derive(Args, Debug)]
pub struct VerifyReplacementArgs {
    /// Durable Cook id or exact attempt whose applied candidate has failed gates.
    #[arg(value_name = "COOK_OR_ATTEMPT_ID")]
    pub cook_or_attempt_id: String,
    /// Explicit operator authorization for the replacement proof recorded by this command.
    #[arg(long, value_name = "TEXT")]
    pub authorize_external_proof: String,
    /// Verification gate configuration for the replacement candidate.
    #[command(flatten)]
    pub gates: VerifyGateArgs,
}
#[derive(Args, Debug)]
pub struct GateFeedbackArgs {
    /// Promotion report as a JSON spec: inline JSON, `@FILE` to read a file, or
    /// `-` to read stdin. A bare path is NOT accepted — use
    /// `@/path/promotion.json`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub promotion: String,
    /// Source task as a JSON spec: inline JSON, `@FILE` to read a file, or `-`
    /// to read stdin. A bare path is NOT accepted — use `@/path/task.json`.
    #[arg(long = "source-task", value_name = "JSON|@FILE|-")]
    pub source_task: String,
    /// Current feedback attempt number.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub attempt: u32,
    /// Maximum feedback attempts before stopping.
    #[arg(long = "max-attempts", default_value_t = 3, value_name = "N")]
    pub max_attempts: u32,
    /// Durable source run ID associated with the feedback.
    #[arg(long = "source-run-id", value_name = "ID")]
    pub source_run_id: Option<String>,
    /// Current candidate diff as an inline or file-backed specification.
    #[arg(long = "current-diff", value_name = "SPEC")]
    pub current_diff: Option<String>,
}
