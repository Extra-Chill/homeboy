//! `agent-task recover`: perform the one correct next step for a stopped Cook
//! (#16049 slice 4, #15925).
//!
//! The step is decided once in `homeboy-agents` from the Cook's durable state.
//! This module only supplies the verbs that step names, by calling the exact
//! handlers `cook-continue`, `retry --run` and `reconcile --apply` already run,
//! so recovery adds no orchestration of its own.

use serde_json::Value;

use homeboy::agents::agent_task_service::{self, CookRecoveryPerformer};

use super::super::CmdResult;
use super::{CookContinueArgs, RecoverArgs, RetryArgs};

pub(super) fn recover(args: RecoverArgs) -> CmdResult<Value> {
    recover_with(args, &CliCookRecoveryPerformer)
}

pub(super) fn recover_with(
    args: RecoverArgs,
    performer: &dyn CookRecoveryPerformer,
) -> CmdResult<Value> {
    let (report, exit_code) = agent_task_service::recover_cook(
        &args.id,
        args.preflight,
        args.artifact_id.as_deref(),
        performer,
    )?;
    Ok((
        serde_json::to_value(report)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?,
        exit_code,
    ))
}

/// `cook-continue` arguments for the continuation a step names. Every other
/// flag stays at its operator default: recovery never changes provider route
/// or timeout on its own.
pub(super) fn continuation_args(
    run_id: &str,
    rearm: bool,
    artifact_id: Option<&str>,
) -> CookContinueArgs {
    CookContinueArgs {
        cook_or_attempt_id: run_id.to_string(),
        preflight: false,
        rearm,
        artifact_id: artifact_id.map(str::to_string),
        timeout_ms: None,
        review_form_timeout_ms: None,
        backend: None,
        selector: None,
        model: None,
        acknowledge_model_override: false,
        full: false,
    }
}

struct CliCookRecoveryPerformer;

impl CookRecoveryPerformer for CliCookRecoveryPerformer {
    fn continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        artifact_id: Option<&str>,
    ) -> homeboy::core::Result<(Value, i32)> {
        super::run::continue_cook(continuation_args(run_id, rearm, artifact_id))
    }

    fn retry(&self, run_id: &str) -> homeboy::core::Result<(Value, i32)> {
        super::run::retry(RetryArgs {
            run_id: run_id.to_string(),
            new_run_id: None,
            run: true,
            force: false,
            idempotency_key: None,
            backend: None,
            selector: None,
            model: None,
            acknowledge_model_override: false,
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
    }
}
