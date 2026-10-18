//! `agent-task recover`: perform the one correct next step for a stopped Cook
//! (#16049 slice 4, #15925).
//!
//! The step is decided once in `homeboy-agents` from the Cook's durable state.
//! This module only supplies the handlers that step names: the Cook
//! continuation handler (`run::continue_cook` / `run::preflight_continue_cook`),
//! `retry --run` (exact attempt) and `reconcile --apply`, so recovery adds no orchestration of
//! its own. It is the only public Cook continuation command.

use serde_json::Value;

use homeboy::agents::agent_task_service::{self, CookRecoveryOverrides, CookRecoveryPerformer};

use super::super::CmdResult;
use super::run::CookContinueArgs;
use super::{RecoverArgs, RetryArgs};

pub(super) fn recover(args: RecoverArgs) -> CmdResult<Value> {
    let performer = CliCookRecoveryPerformer { full: args.full };
    recover_with(args, &performer)
}

pub(super) fn recover_with(
    args: RecoverArgs,
    performer: &dyn CookRecoveryPerformer,
) -> CmdResult<Value> {
    let overrides = args.overrides();
    let (report, exit_code) = agent_task_service::recover_cook(
        &args.id,
        args.preflight,
        args.artifact_id.as_deref(),
        &overrides,
        performer,
    )?;
    Ok((
        serde_json::to_value(report)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?,
        exit_code,
    ))
}

/// Cook continuation arguments for the continuation a step names. Recovery
/// never changes provider route or timeout on its own: only explicit operator
/// `overrides` do.
pub(super) fn continuation_args(
    run_id: &str,
    rearm: bool,
    artifact_id: Option<&str>,
    overrides: &CookRecoveryOverrides,
) -> CookContinueArgs {
    CookContinueArgs {
        cook_or_attempt_id: run_id.to_string(),
        rearm,
        artifact_id: artifact_id.map(str::to_string),
        timeout_ms: overrides.timeout_ms,
        review_form_timeout_ms: overrides.review_form_timeout_ms,
        backend: overrides.backend.clone(),
        selector: overrides.selector.clone(),
        model: overrides.model.clone(),
        acknowledge_model_override: overrides.acknowledge_model_override,
        full: false,
    }
}

struct CliCookRecoveryPerformer {
    full: bool,
}

impl CookRecoveryPerformer for CliCookRecoveryPerformer {
    fn continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        artifact_id: Option<&str>,
        overrides: &CookRecoveryOverrides,
    ) -> homeboy::core::Result<(Value, i32)> {
        let mut args = continuation_args(run_id, rearm, artifact_id, overrides);
        args.full = self.full;
        super::run::continue_cook(args)
    }

    fn preflight_continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        artifact_id: Option<&str>,
    ) -> homeboy::core::Result<(Value, i32)> {
        super::run::preflight_continue_cook(continuation_args(
            run_id,
            rearm,
            artifact_id,
            &CookRecoveryOverrides::default(),
        ))
    }

    fn retry(&self, run_id: &str) -> homeboy::core::Result<(Value, i32)> {
        super::run::retry_exact(RetryArgs {
            run_id: run_id.to_string(),
            new_run_id: None,
            run: true,
            force: false,
            idempotency_key: None,
        })
    }

    fn reconcile(&self, run_id: &str) -> homeboy::core::Result<(Value, i32)> {
        super::status::reconcile_run(super::args::ReconcileArgs {
            run_id: run_id.to_string(),
            dry_run: false,
            apply: true,
            idempotency_key: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli_surface::{Cli, Commands};
    use crate::commands::agent_task::AgentTaskCommand;

    #[test]
    fn recover_parses_its_id_preflight_and_artifact_selection() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-attempt-1",
            "--preflight",
            "--artifact-id",
            "patch-a",
        ])
        .expect("recover parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent task");
        };
        let AgentTaskCommand::Recover(args) = agent_task.command else {
            panic!("recover");
        };
        assert_eq!(args.id, "cook-attempt-1");
        assert!(args.preflight);
        assert_eq!(args.artifact_id.as_deref(), Some("patch-a"));
        assert!(args.overrides().is_empty());
    }

    #[test]
    fn recover_accepts_explicit_overrides_but_never_with_preflight_or_artifact() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-attempt-1",
            "--backend",
            "replacement",
            "--selector",
            "replacement-selector",
            "--model",
            "replacement-model",
            "--acknowledge-model-override",
            "--full",
        ])
        .expect("route override parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent task");
        };
        let AgentTaskCommand::Recover(args) = agent_task.command else {
            panic!("recover");
        };
        let overrides = args.overrides();
        assert!(overrides.has_route_override());
        assert_eq!(overrides.model.as_deref(), Some("replacement-model"));
        assert!(overrides.acknowledge_model_override);
        assert!(args.full);

        for rejected in [
            vec!["--preflight", "--timeout-ms", "1000"],
            vec!["--artifact-id", "patch-a", "--timeout-ms", "1000"],
            vec!["--timeout-ms", "1000", "--review-form-timeout-ms", "1000"],
            vec!["--preflight", "--model", "m"],
            vec!["--acknowledge-model-override"],
        ] {
            let mut argv = vec!["homeboy", "agent-task", "recover", "cook-attempt-1"];
            argv.extend(rejected.iter().copied());
            assert!(Cli::try_parse_from(argv).is_err(), "{rejected:?}");
        }
    }

    #[test]
    fn retired_cook_continuation_verb_is_not_a_command() {
        for retired in [
            vec!["cook-continue", "cook-1"],
            vec!["cook-continue", "cook-1", "--preflight"],
            vec!["cook-continue", "cook-1", "--rearm"],
            vec!["cook-continue", "cook-1", "--artifact-id", "patch-a"],
        ] {
            let mut argv = vec!["homeboy", "agent-task"];
            argv.extend(retired.iter().copied());
            assert!(Cli::try_parse_from(argv).is_err(), "{retired:?}");
        }
    }

    /// The remaining recovery-adjacent verbs serve non-Cook runs or an
    /// explicit operator input `recover` never supplies on its own (#15925),
    /// so they stay commands. `resume` and `retry` hand a Cook ID with a
    /// recovery step to `recover` instead of deciding one.
    #[test]
    fn recovery_adjacent_verbs_recover_does_not_perform_remain_commands() {
        for kept in [
            vec!["resume", "run-1"],
            vec!["retry", "run-1", "--run"],
            vec!["reconcile", "run-1", "--apply"],
            vec!["adopt", "cook-1", "--candidate-ref", "abc123"],
            vec!["promote", "run-1", "--to-worktree", "wt"],
            vec!["runtime-recover", "run-1", "--artifact", "/tmp/homeboy"],
            vec!["finalize-pr", "--recover", "cook-1"],
            vec![
                "record-replacement-gate-proof",
                "cook-1-attempt-1",
                "--promotion",
                "{}",
            ],
        ] {
            let mut argv = vec!["homeboy", "agent-task"];
            argv.extend(kept.iter().copied());
            if let Err(error) = Cli::try_parse_from(argv) {
                panic!("{kept:?}: {error}");
            }
        }
    }
}
