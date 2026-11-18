//! `agent-task recover`: perform the one correct next step for a stopped Cook
//! (#16049 slice 4, #15925).
//!
//! The step is decided once in `homeboy-agents` from the Cook's durable state.
//! Continuation, regate and rearm are frozen into the existing daemon execution
//! outbox. Only its internal execution-step adapter runs those handlers; the
//! public command returns admission, or optionally observes status/watch.

use serde_json::Value;

use homeboy::agents::agent_task_service::{
    self, CookPublishRequest, CookRecoveryOverrides, CookRecoveryPerformer, CookRecoveryRequest,
    CookReplaceGatesRequest, CookReplacementProof,
};

use super::super::CmdResult;
use super::run::CookContinueArgs;
use super::RecoverArgs;

pub(super) fn recover(args: RecoverArgs) -> CmdResult<Value> {
    let wait = args.wait;
    let full = args.full;
    let timeout = args.timeout.clone();
    let duration = crate::commands::utils::watch::parse_duration(
        "--timeout",
        if timeout.is_empty() { "30m" } else { &timeout },
    )?;
    if !args.preflight {
        let id = agent_task_service::resolve_cook_continuation_run_id(&args.id)
            .unwrap_or_else(|_| args.id.clone());
        if let Some(receipt) = agent_task_service::recovery_step_in_flight(&id)? {
            return Ok((receipt, 0));
        }
    }
    let performer = CliCookRecoveryPerformer;
    let (mut report, code) = recover_with(args, &performer)?;
    if wait && report["admission"].is_object() {
        let run_id = report["run_id"].as_str().unwrap().to_string();
        let (status, exit_code) = super::status::wait_for_recovery_status(&run_id, full, duration)?;
        if status["terminal"] != true {
            report["message"] = serde_json::json!(format!("Timed out; the step stays owned by the daemon. homeboy agent-task status {run_id} --watch"));
            return Ok((report, 1));
        }
        let step_exit = agent_task_service::recovery_step_exit_code(&run_id)?.unwrap_or(exit_code);
        let mut status = status;
        if step_exit != 0 {
            if let Some(failure) = agent_task_service::recovery_step_failure(&run_id)? {
                status["failure"] = failure;
            }
        }
        return Ok((status, step_exit));
    }
    Ok((report, code))
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
    // Without external-proof authorization, verification corrects the recipe
    // before provider execution; authorized candidate replacement is unchanged.
    let corrected_gates = if args.authorize_external_proof.is_none()
        && args.promotion.is_none()
        && args.gates.has_deterministic_gate()
    {
        args.gates.snapshot_file_inputs()?;
        Some(std::mem::take(&mut args.gates).into())
    } else {
        None
    };
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
        regate: args.regate,
        unblock: args.unblock,
        overrides,
        replace_gates,
        corrected_gates,
        publish,
    })
}

/// The replace-gates inputs: an explicit authorization plus either corrected
/// gates executed against the applied candidate or external candidate-bound
/// proof. Any one of them without the others is refused, never ignored.
pub(super) fn replace_gates_request(
    args: &mut RecoverArgs,
) -> homeboy::core::Result<Option<CookReplaceGatesRequest>> {
    let has_gates = args.gates.has_deterministic_gate();
    // Authorization does not replace already durable, eligible gate evidence.
    if args.promotion.is_none() && !has_gates {
        return Ok(None);
    }
    let id = args.id.clone();
    let invalid = |message: &str| {
        homeboy::core::Error::validation_invalid_argument(
            "replace-gates",
            message.to_string(),
            Some(id.clone()),
            Some(vec![
                format!("homeboy agent-task recover {id} --authorize-external-proof <AUTHORIZATION> --verify '<CANDIDATE-BOUND COMMAND>'"),
                format!("homeboy agent-task recover {id} --authorize-external-proof <AUTHORIZATION> --promotion @replacement.json"),
            ]),
        )
    };
    let Some(authorization) = args.authorize_external_proof.clone() else {
        return Err(invalid(&format!(
            "replacement gates and replacement proof require --authorize-external-proof <AUTHORIZATION>; a candidate whose declared gates passed and whose other failures are proven inherited baseline failures needs no replacement and publishes with `homeboy agent-task recover {id}`"
        )));
    };
    let proof = match (args.promotion.as_deref(), has_gates) {
        (Some(_), true) => {
            return Err(invalid(
                "--promotion records external proof; it cannot be combined with corrected gates",
            ))
        }
        (Some(spec), false) => CookReplacementProof::External {
            promotion: Box::new(super::review::replacement_gate_proof(spec)?),

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
        backend: overrides.backend.clone(),
        selector: overrides.selector.clone(),
        model: overrides.model.clone(),
        acknowledge_model_override: overrides.acknowledge_model_override,
        full: false,
    }
}

struct CliCookRecoveryPerformer;

impl CookRecoveryPerformer for CliCookRecoveryPerformer {
    fn admit_step(
        &self,
        report: &agent_task_service::CookRecoveryReport,
        overrides: &CookRecoveryOverrides,
    ) -> homeboy::core::Result<Option<Value>> {
        agent_task_service::admit_recovery_execution_step(report, overrides).map(Some)
    }
    fn continue_cook(
        &self,
        run_id: &str,
        rearm: bool,
        overrides: &CookRecoveryOverrides,
    ) -> homeboy::core::Result<(Value, i32)> {
        let _ = (run_id, rearm, overrides);
        Err(homeboy::core::Error::internal_unexpected(
            "recovery continuation must be admitted as a daemon execution step",
        ))
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

    /// Return once the successor is admitted; never wait on the retry (#17768).
    fn retry(&self, run_id: &str) -> homeboy::core::Result<(Value, i32)> {
        super::run::admit_retry_for_recovery(run_id)
    }

    fn reconcile(&self, run_id: &str) -> homeboy::core::Result<(Value, i32)> {
        super::status::reconcile_run(super::args::ReconcileArgs {
            all: false,
            run_id: Some(run_id.to_string()),
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

/// Execute only the step frozen by the daemon intent; never admit recursively.
pub(crate) fn execute_admitted_step(
    run_id: &str,
    step: &agent_task_service::CookRecoveryStep,
    overrides: &CookRecoveryOverrides,
) -> homeboy::core::Result<i32> {
    let performer = CliCookRecoveryPerformer;
    if !overrides.is_empty() {
        return super::run::continue_cook(continuation_args(
            run_id,
            overrides.has_route_override(),
            overrides,
        ))
        .map(|(_, code)| code);
    }
    let (_, code) = match step {
        agent_task_service::CookRecoveryStep::Regate => performer.regate(run_id)?,
        agent_task_service::CookRecoveryStep::Continue
        | agent_task_service::CookRecoveryStep::Rearm => {
            if *step == agent_task_service::CookRecoveryStep::Continue {
                agent_task_service::prepare_admitted_recovery_continuation(run_id)?;
            }
            super::run::continue_cook(continuation_args(
                run_id,
                *step == agent_task_service::CookRecoveryStep::Rearm,
                overrides,
            ))?
        }
        _ => {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "recovery",
                "unsupported admitted step",
                None,
                None,
            ))
        }
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli_surface::{Cli, Commands};
    use crate::commands::agent_task::AgentTaskCommand;

    /// A pressure-refused admission must not produce an ownerless queued retry.
    #[test]
    fn recover_refuses_pressure_failed_unmaterialized_admission_without_a_successor() {
        use homeboy::agents::agent_task_lifecycle;
        crate::test_support::with_isolated_home(|home| {
            let cook_id = "cook-recover-admission";
            let prompt = home.path().join("prompt.txt");
            std::fs::write(&prompt, "Fix it.").unwrap();
            let route = home.path().join("route.txt");
            std::fs::write(&route, "discord:v1:thread:1").unwrap();
            let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &store,
                cook_id,
                serde_json::json!({
                    "schema": "homeboy/unmaterialized-cook-binding/v1",
                    "request_ref": "sha256:request",
                    "worktree_ref": "repo@branch",
                    "admission_inputs": {
                        "cook_id": cook_id,
                        "argv": ["agent-task", "cook", "--issue", "https://github.com/Extra-Chill/homeboy/issues/18096", "--prompt", "homeboy-admission-ref:0:p", "--notification-route=homeboy-admission-ref:1:r", "--run-id", cook_id],
                        "input_refs": [
                            { "kind": "prompt", "path": prompt, "sha256": "sha256:p", "argv_token": "homeboy-admission-ref:0:p" },
                            { "kind": "notification-route", "path": route, "sha256": "sha256:r", "argv_token": "homeboy-admission-ref:1:r" },
                        ],
                    },
                }),
                "blocked_runner_unavailable",
                "runner cannot accept jobs (reasons: pressure)",
            )
            .unwrap();
            store
                .mutate_record(cook_id, |record| {
                    record.metadata["detached_cook_handoff"]["state"] =
                        serde_json::json!("exited_before_handoff");
                    record
                        .set_state_for_test(homeboy_lifecycle_contract::RunExecutionState::Failed);
                    true
                })
                .unwrap();

            let cli = Cli::try_parse_from(["homeboy", "agent-task", "recover", cook_id]).unwrap();
            let Commands::AgentTask(task) = cli.command else {
                panic!("agent-task")
            };
            let AgentTaskCommand::Recover(args) = task.command else {
                panic!("recover")
            };
            let (report, exit_code) =
                homeboy::agents::agent_task_service::with_scheduling_daemon_start(
                    || Ok(()),
                    || super::recover(args),
                )
                .expect("recover returns a terminal failure report");

            assert_eq!(exit_code, 1);
            assert_eq!(report["status"], "failed");
            assert_eq!(report["failure"]["code"], "agent_task.recover_no_step");
            let message = report["failure"]["message"].as_str().unwrap();
            assert!(message.contains("no replayable materialization identity"));
            // The relaunch is runnable: admission tokens resolve to the stored
            // prompt file and the recorded route, and the run identity is dropped.
            assert!(message.contains(&format!(
                "homeboy agent-task cook --issue https://github.com/Extra-Chill/homeboy/issues/18096 --prompt @{} --notification-route=discord:v1:thread:1",
                prompt.display()
            )), "{message}");
            assert!(!message.contains("homeboy-admission-ref"));
            assert!(!message.contains(&format!("--run-id {cook_id}")));
            let records = store.read_records().unwrap();
            assert!(
                records.iter().all(|record| record.run_id == cook_id),
                "no ownerless successor may be persisted"
            );
            assert_eq!(
                store.read_record(cook_id).unwrap().state(),
                homeboy_lifecycle_contract::RunExecutionState::Failed
            );
        });
    }

    #[test]
    fn recover_regate_reaches_the_service_request_with_or_without_preflight() {
        for preflight in [false, true] {
            let mut argv = vec!["homeboy", "agent-task", "recover", "cook-1", "--regate"];
            if preflight {
                argv.push("--preflight");
            }
            let cli = Cli::try_parse_from(argv).unwrap();
            let Commands::AgentTask(task) = cli.command else {
                panic!("agent-task")
            };
            let AgentTaskCommand::Recover(args) = task.command else {
                panic!("recover")
            };
            let request = super::recovery_request(args).unwrap();
            assert!(request.regate);
            assert_eq!(request.preflight, preflight);
        }
    }

    #[test]
    fn recover_wait_is_opt_in_and_timeout_requires_wait() {
        for argv in [
            vec!["homeboy", "agent-task", "recover", "cook-1"],
            vec![
                "homeboy",
                "agent-task",
                "recover",
                "cook-1",
                "--wait",
                "--timeout",
                "2s",
            ],
        ] {
            assert!(Cli::try_parse_from(argv).is_ok());
        }
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-1",
            "--timeout",
            "2s"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "recover",
            "cook-1",
            "--preflight",
            "--wait"
        ])
        .is_err());
    }

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
