//! `agent-task recover`: perform the one correct next step for a stopped Cook
//! (#16049 slice 4, #15925).
//!
//! The step is decided once in `homeboy-agents` from the Cook's durable state.
//! This module only supplies the handlers that step names: the Cook
//! continuation handler (`run::continue_cook` / `run::preflight_continue_cook`),
//! `retry --run` (exact attempt) and `reconcile --apply`, so recovery adds no orchestration of
//! its own. It is the only public Cook continuation command.

use serde_json::Value;

use homeboy::agents::agent_task_service::{
    self, CookPublishRequest, CookRecoveryOverrides, CookRecoveryPerformer, CookRecoveryRequest,
    CookReplaceGatesRequest, CookReplacementProof,
};

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
    let id = args.id.clone();
    let request = recovery_request(args)?;
    let (report, exit_code) = agent_task_service::recover_cook(&id, request, performer)?;
    Ok((
        serde_json::to_value(report)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?,
        exit_code,
    ))
}

/// Translate `recover` flags into the typed request the step decision
/// validates. Replacement-gate and publication inputs are parsed here so a
/// malformed input fails before any durable state is read.
fn recovery_request(mut args: RecoverArgs) -> homeboy::core::Result<CookRecoveryRequest> {
    let overrides = args.overrides();
    let replace_gates = replace_gates_request(&mut args)?;
    let publish = CookPublishRequest {
        review_form: super::review::supplied_review_form(
            args.review_form.as_deref(),
            args.review_form_tool.as_deref(),
            args.review_form_model.as_deref(),
            args.review_form_author.as_deref(),
        )?,
        component: args.component.take(),
        review_overrides: super::review::parse_review_overrides(&args.review_overrides)?,
    };
    Ok(CookRecoveryRequest {
        preflight: args.preflight,
        overrides,
        replace_gates,
        publish,
    })
}

/// The replace-gates inputs: an explicit authorization plus either corrected
/// gates executed against the applied candidate or external candidate-bound
/// proof. Any one of them without the others is refused, never ignored.
fn replace_gates_request(
    args: &mut RecoverArgs,
) -> homeboy::core::Result<Option<CookReplaceGatesRequest>> {
    let has_gates = args.gates.has_deterministic_gate();
    if args.authorize_external_proof.is_none()
        && args.promotion.is_none()
        && !has_gates
        && !args.gates.accept_inherited_failures
    {
        return Ok(None);
    }
    let invalid = |message: &str| {
        homeboy::core::Error::validation_invalid_argument(
            "replace-gates",
            message.to_string(),
            Some(args.id.clone()),
            None,
        )
    };
    let Some(authorization) = args.authorize_external_proof.clone() else {
        return Err(invalid(
            "replacement gates and replacement proof require --authorize-external-proof",
        ));
    };
    let proof = match (args.promotion.as_deref(), has_gates) {
        (Some(_), true) => {
            return Err(invalid(
                "--promotion records external proof; it cannot be combined with corrected gates",
            ))
        }
        (Some(spec), false) => CookReplacementProof::External {
            promotion: Box::new(super::review::replacement_gate_proof(spec)?),
            accept_inherited_failures: args.gates.accept_inherited_failures,
        },
        (None, true) => {
            args.gates.snapshot_file_inputs()?;
            CookReplacementProof::Gates(std::mem::take(&mut args.gates).into())
        }
        (None, false) => {
            return Err(invalid(
                "--authorize-external-proof needs corrected gates (--verify/--verify-file) or candidate-bound proof (--promotion)",
            ))
        }
    };
    Ok(Some(CookReplaceGatesRequest {
        proof,
        authorize_external_proof: authorization,
        authorize_interrupted_rerun: args.authorize_interrupted_rerun.clone(),
    }))
}

/// Cook continuation arguments for the continuation a step names. Recovery
/// never changes provider route or timeout on its own: only explicit operator
/// `overrides` do.
pub(super) fn continuation_args(
    run_id: &str,
    rearm: bool,
    overrides: &CookRecoveryOverrides,
) -> CookContinueArgs {
    CookContinueArgs {
        cook_or_attempt_id: run_id.to_string(),
        rearm,
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
        overrides: &CookRecoveryOverrides,
    ) -> homeboy::core::Result<(Value, i32)> {
        let mut args = continuation_args(run_id, rearm, overrides);
        args.full = self.full;
        super::run::continue_cook(args)
    }

    fn preflight_continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
    ) -> homeboy::core::Result<(Value, i32)> {
        super::run::preflight_continue_cook(continuation_args(
            run_id,
            rearm,
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

    fn adopt(&self, run_id: &str, candidate_ref: &str) -> homeboy::core::Result<(Value, i32)> {
        let result = homeboy::agents::agent_task_service::
            adopt_cook_candidate_with_options_dispatcher_and_executor_for_attempt(
                run_id,
                None,
                candidate_ref,
                homeboy::agents::agent_task_service::AgentTaskCandidateAdoptionOptions::default(),
                crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
                std::sync::Arc::new(
                    homeboy::agents::agent_task_provider::ExtensionProviderAgentTaskExecutor::discover(),
                ),
            )?;
        let value = serde_json::to_value(result.value)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        Ok((value, result.exit_code))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli_surface::{Cli, Commands};
    use crate::commands::agent_task::AgentTaskCommand;

    #[test]
    fn recover_parses_its_id_and_preflight() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-attempt-1",
            "--preflight",
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
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-attempt-1",
            "--artifact-id",
            "patch-a"
        ])
        .is_err());
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

    /// The remaining recovery-adjacent verbs serve non-Cook runs (#15925), so
    /// they stay commands. Replacement gates and Cook publication are `recover`
    /// steps. `resume` and `retry` hand a Cook ID with a
    /// recovery step to `recover` instead of deciding one.
    #[test]
    fn recovery_adjacent_verbs_recover_does_not_perform_remain_commands() {
        for kept in [
            vec!["resume", "run-1"],
            vec!["retry", "run-1", "--run"],
            vec!["reconcile", "run-1", "--apply"],
            vec!["adopt", "cook-1", "--candidate-ref", "abc123"],
            vec!["promote", "run-1", "--to-worktree", "wt"],
            vec![
                "finalize-pr",
                "--run-id",
                "run-1",
                "--path",
                "/tmp/run-1",
                "--title",
                "Run 1",
                "--commit-message",
                "run 1",
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
