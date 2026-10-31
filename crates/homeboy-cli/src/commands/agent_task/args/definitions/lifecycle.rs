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
    /// Display the contents of a specific artifact by id.
    #[arg(long, value_name = "ARTIFACT_ID", conflicts_with = "full")]
    pub show: Option<String>,
    /// Display only the final N lines of the selected artifact.
    #[arg(long, value_name = "N", requires = "show")]
    pub tail: Option<usize>,
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

#[derive(Args, Debug)]
pub struct PlacementUpdateArgs {
    /// Durable queued Cook ID whose unexecuted placement will be changed.
    pub run_id: String,
    /// Record the operator's confirmation of this execution-route change.
    #[arg(long)]
    pub confirm: bool,
    /// Stable key used to replay this placement update without changing it twice.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}

#[cfg_attr(test, derive(Default))]
#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Durable run or Cook ID whose status to inspect.
    pub run_id: String,
    /// Include the complete lifecycle and eligibility report.
    #[arg(long)]
    pub full: bool,
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
#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use crate::{
        cli_surface::{Cli, Commands},
        commands::agent_task::AgentTaskCommand,
    };

    #[test]
    fn placement_update_requires_explicit_local_confirmation() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "placement-update",
            "cook-a",
            "--confirm",
        ])
        .expect("local placement update parses");
        assert_eq!(cli.placement, crate::cli_surface::Placement::Local);
    }

    #[test]
    fn quarantine_and_rearm_expose_explicit_confirmation() {
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "quarantine",
            "run-a",
            "--reason",
            "hold"
        ])
        .is_ok());
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "quarantine",
            "run-a",
            "--reason",
            "hold",
            "--confirm",
        ])
        .expect("confirmed quarantine parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Quarantine(args) = agent_task.command else {
            panic!("expected quarantine command");
        };
        assert!(args.confirm);

        let cli = Cli::try_parse_from(["homeboy", "agent-task", "rearm", "run-a", "--confirm"])
            .expect("confirmed rearm parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Rearm(args) = agent_task.command else {
            panic!("expected rearm command");
        };
        assert!(args.confirm);
    }

    #[test]
    fn model_parses_canonically_for_adoption() {
        let adopt = |flag: &str| {
            Cli::try_parse_from([
                "homeboy",
                "agent-task",
                "adopt",
                "cook-a",
                "--candidate-ref",
                "0123456789abcdef0123456789abcdef01234567",
                flag,
                "openai/gpt-5.6-sol",
            ])
        };
        let cli = adopt("--model").expect("adoption model parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Adopt(args) = agent_task.command else {
            panic!("expected adoption command");
        };
        assert_eq!(args.ai_model.as_deref(), Some("openai/gpt-5.6-sol"));
        // The deprecated `--ai-model` alias was deleted for adoption (#15742).
        assert!(adopt("--ai-model").is_err());
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
    fn finalize_pr_no_longer_recovers_a_durable_cook() {
        for argv in [
            vec!["finalize-pr", "--recover", "cook-a"],
            vec!["finalize-pr", "--recover", "cook-a", "--preflight"],
            vec!["finalize-pr", "--recover", "cook-a", "--review-form", "{}"],
        ] {
            let mut full = vec!["homeboy", "agent-task"];
            full.extend(argv.iter().copied());
            assert!(
                Cli::try_parse_from(&full).is_err(),
                "{argv:?} must be rejected; a stopped Cook publishes through `recover`"
            );
        }
    }

    #[test]
    fn manual_finalization_parses_component_selector_and_recovery_accepts_it_as_fallback() {
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

        // #14728: the component fallback for a durable record that recorded no
        // identity is a `recover` publication input.
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-a",
            "--component",
            "nested-component",
        ])
        .expect("recovery with a fallback --component parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Recover(args) = agent_task.command else {
            panic!("expected recover command");
        };
        assert_eq!(args.id, "cook-a");
        assert_eq!(args.component.as_deref(), Some("nested-component"));
    }

    #[test]
    fn status_exact_selects_concrete_record() {
        let cli = Cli::try_parse_from(["homeboy", "agent-task", "status", "cook-a", "--exact"])
            .expect("exact status parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let AgentTaskCommand::Status(args) = agent_task.command else {
            panic!("expected status command");
        };
        assert!(args.exact);
        assert_eq!(args.run_id, "cook-a");
    }

    #[test]
    fn artifacts_can_show_a_selected_artifact_and_tail_its_lines() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "artifacts",
            "run-a",
            "--show",
            "transcript",
            "--tail",
            "12",
        ])
        .expect("artifact show and tail parse");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task")
        };
        let AgentTaskCommand::Artifacts(args) = agent_task.command else {
            panic!("artifacts")
        };
        assert_eq!(args.show.as_deref(), Some("transcript"));
        assert_eq!(args.tail, Some(12));
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "artifacts",
            "run-a",
            "--tail",
            "12"
        ])
        .is_err());
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
            "--run-id",
            "run-a",
            "--path",
            "/tmp/run-a",
            "--title",
            "Run A",
            "--commit-message",
            "run a",
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
#[derive(Args, Debug, Default)]
pub struct RecoverArgs {
    /// Durable Cook ID, Cook attempt ID or batch-child run ID.
    pub id: String,
    /// Report the step recovery would perform without performing it. A
    /// continuation step also validates continuation admission read-only.
    #[arg(long)]
    pub preflight: bool,

    /// Explicitly increase the provider timeout for a new retry attempt of a
    /// terminal provider timeout. The override and its operator authority are
    /// retained in the Cook recipe.
    #[arg(
        long = "timeout-ms",
        value_name = "MS",
        conflicts_with_all = ["preflight"]
    )]
    pub timeout_ms: Option<u64>,
    /// Explicitly increase the optional review-form deadline for a new retry
    /// attempt of a timed-out review form. Distinct from `--timeout-ms`;
    /// capped at 600000ms.
    #[arg(
        long = "review-form-timeout-ms",
        value_name = "MS",
        conflicts_with_all = ["preflight", "timeout_ms"]
    )]
    pub review_form_timeout_ms: Option<u64>,
    /// Backend for a rearmed retry of a terminal attempt. The original Cook
    /// recipe remains authoritative for prompt, gates, worktree, notification
    /// and disclosure.
    #[arg(long, value_name = "BACKEND", conflicts_with_all = ["preflight", "timeout_ms", "review_form_timeout_ms"])]
    pub backend: Option<String>,
    /// Provider-specific selector for a rearmed retry of a terminal attempt.
    #[arg(long, visible_alias = "provider-id", value_name = "SELECTOR", conflicts_with_all = ["preflight", "timeout_ms", "review_form_timeout_ms"])]
    pub selector: Option<String>,
    /// Model for a rearmed retry of a terminal attempt. Confirm the pinned
    /// route with --acknowledge-model-override.
    #[arg(long, value_name = "MODEL", conflicts_with_all = ["preflight", "timeout_ms", "review_form_timeout_ms"])]
    pub model: Option<String>,
    /// Confirm an explicit model override that pins this retry.
    #[arg(long, requires = "model")]
    pub acknowledge_model_override: bool,
    /// Include the complete Cook report rather than the compact lifecycle view.
    #[arg(long)]
    pub full: bool,
    /// Replace-gates step: explicit operator authorization for the replacement
    /// proof recorded for an applied candidate whose gates failed. Required
    /// with corrected gates (`--verify`/`--verify-file`) or `--promotion`.
    #[arg(long, value_name = "TEXT")]
    pub authorize_external_proof: Option<String>,
    /// Replace-gates step: explicit authorization to rerun corrected gates
    /// after an interrupted replacement execution. Homeboy still refuses while
    /// the original operation lease is live.
    #[arg(long, value_name = "TEXT", requires = "authorize_external_proof")]
    pub authorize_interrupted_rerun: Option<String>,
    /// Replace-gates step: complete typed promotion report produced by an
    /// external, candidate-bound gate executor: inline JSON, `@FILE`, or `-`.
    #[arg(
        long,
        value_name = "JSON|@FILE|-",
        requires = "authorize_external_proof",
        conflicts_with = "authorize_interrupted_rerun"
    )]
    pub promotion: Option<String>,
    /// Replace-gates step: corrected verification gates executed against the
    /// already-applied candidate.
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Publish step: complete typed AI review form for a candidate whose
    /// provider outcome recorded none (or declared the task incomplete):
    /// inline JSON, `@FILE`, or `-`.
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub review_form: Option<String>,
    /// Tool that authored the supplied review form.
    #[arg(long, value_name = "TOOL", requires = "review_form")]
    pub review_form_tool: Option<String>,
    /// Concrete model that authored the supplied review form.
    #[arg(long, value_name = "MODEL", requires = "review_form")]
    pub review_form_model: Option<String>,
    /// Operator or agent identity submitting the supplied review form.
    #[arg(long, value_name = "IDENTITY", requires = "review_form")]
    pub review_form_author: Option<String>,
    /// Publish step: explicit reviewer override in `TARGET=VALUE@PROVENANCE`
    /// form. Overrides require a recorded or supplied review form.
    #[arg(long = "review-override", value_name = "TARGET=VALUE@PROVENANCE")]
    pub review_overrides: Vec<String>,
    /// Publish step: registered component identity, consulted only when the
    /// durable record carries no recorded component identity.
    #[arg(long, value_name = "COMPONENT_ID")]
    pub component: Option<String>,
}

impl RecoverArgs {
    /// The explicit operator overrides this invocation supplies.
    pub fn overrides(&self) -> homeboy::agents::agent_task_service::CookRecoveryOverrides {
        homeboy::agents::agent_task_service::CookRecoveryOverrides {
            timeout_ms: self.timeout_ms,
            review_form_timeout_ms: self.review_form_timeout_ms,
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            model: self.model.clone(),
            acknowledge_model_override: self.acknowledge_model_override,
        }
    }
}
#[derive(Args, Debug)]
pub struct RetryArgs {
    /// Durable run ID to retry. A stopped Cook is recovered with
    /// `agent-task recover <cook-or-attempt-id>`, which also takes explicit
    /// provider-route overrides for its next attempt.
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
}

#[cfg(test)]
mod retry_tests {
    use clap::Parser;

    use crate::cli_surface::Cli;

    /// A Cook's next-attempt provider route is an explicit `recover`
    /// override; `retry` carries no Cook route surface of its own.
    #[test]
    fn retry_has_no_cook_route_override_surface() {
        for flag in [
            vec!["--backend", "replacement"],
            vec!["--selector", "replacement-selector"],
            vec!["--provider-id", "replacement-selector"],
            vec![
                "--model",
                "replacement-model",
                "--acknowledge-model-override",
            ],
        ] {
            let mut argv = vec!["homeboy", "agent-task", "retry", "cook-a"];
            argv.extend(flag.iter().copied());
            assert!(Cli::try_parse_from(argv).is_err(), "{flag:?}");
        }
        assert!(Cli::try_parse_from(["homeboy", "agent-task", "retry", "run-a", "--run"]).is_ok());
    }
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Durable run or Cook ID to cancel.
    pub run_id: String,
    /// Include the complete, redacted lifecycle record instead of the compact
    /// cancellation acknowledgement.
    #[arg(long)]
    pub full: bool,
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
    /// Record the operator's confirmation before quarantining this queued run.
    #[arg(long)]
    pub confirm: bool,
    /// Stable caller key for safely replaying this quarantine request.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
}
#[derive(Args, Debug)]
pub struct RearmArgs {
    /// Exact durable run id. Cook aliases are not accepted for mutations.
    pub run_id: String,
    /// Record the operator's confirmation before re-arming this queued run.
    #[arg(long)]
    pub confirm: bool,
    /// Stable caller key for safely replaying this rearm request.
    #[arg(long, value_name = "KEY")]
    pub idempotency_key: Option<String>,
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
    /// Exact argv element for the promotion apply provider; repeat per element.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
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
    /// Exact argv element for the promotion apply provider; repeat per element.
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
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
    #[arg(
        long = "gates-from-cook-recipe",
        conflicts_with = "gates_from_resume_contract"
    )]
    pub gates_from_cook_recipe: bool,
    /// Replay the exact gate policy from the source run's durable promotion
    /// resume contract without exposing private gate programs in command output.
    #[arg(long = "gates-from-resume-contract")]
    pub gates_from_resume_contract: bool,
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
    /// Concrete model that prepared the externally supplied candidate.
    #[arg(long = "model", value_name = "MODEL")]
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
    /// Durable run ID for a manual finalization record.
    #[arg(long, value_name = "ID")]
    pub run_id: String,
    /// Worktree path containing the manual finalization candidate.
    #[arg(long, value_name = "PATH")]
    pub path: String,
    /// Registered component identity for disambiguating a shared-repository worktree.
    /// A stopped Cook publishes through `agent-task recover`, which takes the same fallback.
    #[arg(long, value_name = "COMPONENT_ID")]
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
    #[arg(long, value_name = "TEXT")]
    pub title: String,
    /// Commit message for the manual finalization candidate.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: String,
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
