//! `agent-task recover`: perform the one correct next step for a durable run
//! (#16049 slice 4, #15925).
//!
//! The step is decided once in `homeboy-agents` from the Cook's durable state.
//! Continuation, regate and rearm are frozen into the existing daemon execution
//! outbox. Only its internal execution-step adapter runs those handlers; the
//! public command returns admission, or optionally observes status/watch.

use serde_json::Value;

use homeboy::agents::agent_task_service::{
    self, CookPublishRequest, CookRecoveryOverrides, CookRecoveryPerformer, CookRecoveryRequest,
    CookRegateInputs, CookReplaceGatesRequest, CookReplacementProof,
};

use super::super::CmdResult;
use super::run::CookContinueArgs;
use super::RecoverArgs;

pub(crate) fn recovery_preflight(id: &str, rearm: bool) -> homeboy::core::Result<(Value, i32)> {
    super::run::preflight_continue_cook(continuation_args(
        id,
        rearm,
        &CookRecoveryOverrides::default(),
    ))
}

pub(super) fn recover(args: RecoverArgs) -> CmdResult<Value> {
    // Every wait this client performs reports what it waits for, honours
    // SIGINT/SIGTERM and the optional admission bound, and withdraws queued
    // gate work when it stops (#18042).
    let policy = homeboy::core::foreground_wait::ForegroundWaitPolicy {
        admission_timeout: args
            .wait_timeout_seconds
            .map(std::time::Duration::from_secs),
    };
    let id = args.id.clone();
    let (report, exit_code) =
        match homeboy::core::foreground_wait::with_foreground_wait(policy, || recover_action(args))
        {
            Ok(result) => result,
            // Keep the error's exit-code class (2 for an unknown or invalid
            // ID) while still answering with a structured failure report.
            Err(error) => {
                let exit_code = crate::commands::utils::response::exit_code_for_error(error.code);
                (
                    serde_json::json!({
                        "run_id": id,
                        "failure": {
                            "code": error.code.as_str(),
                            "message": error.message,
                            "details": error.details,
                        },
                    }),
                    exit_code,
                )
            }
        };
    let exit_code = foreground_stop_exit_code(&report).unwrap_or(exit_code);
    Ok((
        agent_task_service::ensure_recover_failure_contract(report, exit_code, &id),
        exit_code,
    ))
}

/// A recovery the client stopped exits with the stop's conventional status:
/// 124 for `--wait-timeout-seconds`, 128+N for signal N (#18042).
fn foreground_stop_exit_code(report: &Value) -> Option<i32> {
    [
        "/failure/details/foreground_wait/exit_code",
        "/details/foreground_wait/exit_code",
    ]
    .iter()
    .find_map(|pointer| report.pointer(pointer))
    .and_then(Value::as_i64)
    .and_then(|code| i32::try_from(code).ok())
    .filter(|code| *code != 0)
}

fn recover_action(args: RecoverArgs) -> CmdResult<Value> {
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
    use homeboy_control_plane_contract::*;
    let id = args.id.clone();
    let key = args
        .idempotency_key
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let candidate_ref = args.candidate_ref.clone();
    let attempt = args.attempt;
    let adoption_model = args.adoption_model.clone();
    let replace_interrupted = args.replace_interrupted;
    let reason = args.reason.clone();
    let command = "agent-task recover";
    let preflight = args.preflight;
    let recovery = recovery_request(args)?;
    let mut replayed = false;
    let parameters = homeboy::agents::recovery_action::RecoverParameters {
        preflight: recovery.preflight,
        regate: recovery.regate,
        unblock: recovery.unblock,
        backend: recovery.overrides.backend,
        selector: recovery.overrides.selector,
        model: recovery.overrides.model,
        acknowledge_model_override: recovery.overrides.acknowledge_model_override,
        timeout_ms: recovery.overrides.timeout_ms,
        candidate_ref,
        attempt,
        adoption_model,
        replace_interrupted,
        // The contract carries regate replacement commands in
        // `corrected_gates`; `regate` disambiguates the two uses.
        corrected_gates: recovery.regate_inputs.gates.or(recovery.corrected_gates),
        replace_gates: recovery.replace_gates,
        publish: Some(recovery.publish),
        gate_timeout_seconds: recovery.regate_inputs.gate_timeout_seconds,
        gate_no_progress_timeout_seconds: recovery.regate_inputs.gate_no_progress_timeout_seconds,
    };

    // A run with no lifecycle record (an unknown ID or a recipe-only Cook)
    // has no control-plane resource, so the Cook planner answers directly.
    let (report, code) =
        if homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?
            .read_record(&id)
            .is_err()
        {
            homeboy::agents::recovery_action::execute(&id, parameters)?
        } else {
            let request = ControlPlaneActionRequest {
                schema: CONTROL_PLANE_ACTION_REQUEST_SCHEMA.into(),
                effect_id: action_effect_id("cli", &id, "recover", &key),
                action: ControlPlaneAction::Recover,
                idempotency_key: key,
                actor: "cli".into(),
                expected_updated_at: None,
                confirmed: true,
                parameters: ControlPlaneActionPayload {
                    schema: CONTROL_PLANE_RECOVER_PARAMETERS_SCHEMA.into(),
                    data: serde_json::to_value(parameters)
                        .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?,
                },
            };
            let mut ack = homeboy::agents::orchestration::execute_action_from_current_environment(
                &id, &request,
            )?;
            replayed = ack.outcome == ControlPlaneActionOutcome::AlreadySatisfied;
            match ack.result.data["exit_code"].as_i64() {
                Some(code) => (ack.result.data["report"].take(), code as i32),
                None => (
                    serde_json::json!({
                        "run_id": id,
                        "status": ack.resource.state,
                        "failure": {
                            "code": "agent_task.recover_refused",
                            "message": ack.message,
                        },
                    }),
                    1,
                ),
            }
        };
    // `recover` never exits nonzero without a structured cause (#18845).
    let mut report = agent_task_service::ensure_recover_failure_contract(report, code, &id);
    if !preflight
        && !replayed
        && code == 0
        && (report["admission"].is_object() || report["result"].is_object())
    {
        homeboy::core::observation::store::record_intervention(
            &homeboy::core::observation::ObservationStore::open_initialized()?,
            report["run_id"].as_str().unwrap_or(&id),
            command,
            reason.as_deref(),
        )?;
    }
    if wait && report["admission"].is_object() {
        homeboy::core::foreground_wait::observe_signals();
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

#[cfg(test)]
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
    // Gate budgets alone only change a regate; refuse them rather than
    // silently ignoring them (#18556).
    let budgets_only = !args.gates.has_deterministic_gate()
        && (args.gates.gate_timeout_seconds.is_some()
            || args.gates.gate_no_progress_timeout_seconds.is_some());
    if budgets_only && !args.regate {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "regate",
            "--gate-timeout-seconds and --gate-no-progress-timeout-seconds change the budgets of a regate; add --regate",
            Some(args.id.clone()),
            Some(vec![format!(
                "homeboy agent-task recover {} --regate --gate-timeout-seconds <SECONDS> --gate-no-progress-timeout-seconds <SECONDS>",
                args.id
            )]),
        ));
    }
    // `--regate` re-runs gates on the existing candidate; its verify commands
    // and gate budgets replace the declared ones for that regate (#17802).
    let regate_inputs = if args.regate {
        regate_inputs(&mut args)?
    } else {
        CookRegateInputs::default()
    };
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
        regate_inputs,
        unblock: args.unblock,
        overrides,
        replace_gates,
        corrected_gates,
        publish,
    })
}

/// `--regate` replacements. Explicit verify commands replace the declared
/// commands; an explicitly passed `--gate-timeout-seconds` or
/// `--gate-no-progress-timeout-seconds` (`0` disables it) replaces the
/// recorded per-gate budget, even when it equals the CLI default. Everything
/// else keeps the recorded gate policy.
fn regate_inputs(args: &mut RecoverArgs) -> homeboy::core::Result<CookRegateInputs> {
    let gate_timeout_seconds = args.gates.gate_timeout_seconds;
    let gate_no_progress_timeout_seconds = args.gates.gate_no_progress_timeout_seconds;
    let gates = if args.gates.has_deterministic_gate() {
        args.gates.snapshot_file_inputs()?;
        Some(std::mem::take(&mut args.gates).into())
    } else {
        None
    };
    Ok(CookRegateInputs {
        gates,
        gate_timeout_seconds,
        gate_no_progress_timeout_seconds,
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

/// Execute only the step frozen by the daemon intent; never admit recursively.
/// The step's report is returned with its exit code rather than dropped, so the
/// execution-step child can publish the step's declared failure as its cause.
pub(crate) fn execute_admitted_step(
    run_id: &str,
    step: &agent_task_service::CookRecoveryStep,
    overrides: &CookRecoveryOverrides,
    regate: &CookRegateInputs,
) -> homeboy::core::Result<(serde_json::Value, i32)> {
    // The daemon-owned step reports its waits to its log and stops on the
    // daemon's SIGTERM, withdrawing queued gate work (#18042).
    homeboy::core::foreground_wait::with_foreground_wait(Default::default(), || {
        execute_admitted_step_with(
            run_id,
            step,
            overrides,
            regate,
            super::run::run_cook_continuation,
        )
    })
}

/// The admitted-step adapter with the Cook continuation handler it runs
/// continuation, rearm and override steps through.
pub(super) fn execute_admitted_step_with(
    run_id: &str,
    step: &agent_task_service::CookRecoveryStep,
    overrides: &CookRecoveryOverrides,
    regate: &CookRegateInputs,
    continuation: impl Fn(CookContinueArgs) -> CmdResult<Value>,
) -> homeboy::core::Result<(serde_json::Value, i32)> {
    let performer = homeboy::agents::recovery_action::ControlPlaneRecoveryPerformer;
    if !overrides.is_empty() {
        return continuation(continuation_args(
            run_id,
            overrides.has_route_override(),
            overrides,
        ));
    }
    match step {
        agent_task_service::CookRecoveryStep::Regate => performer.regate(run_id, regate),
        agent_task_service::CookRecoveryStep::Continue
        | agent_task_service::CookRecoveryStep::Rearm => {
            if *step == agent_task_service::CookRecoveryStep::Continue {
                agent_task_service::prepare_admitted_recovery_continuation(run_id)?;
            }
            continuation(continuation_args(
                run_id,
                *step == agent_task_service::CookRecoveryStep::Rearm,
                overrides,
            ))
        }
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "recovery",
            "unsupported admitted step",
            None,
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli_surface::{Cli, Commands};
    use crate::commands::agent_task::AgentTaskCommand;

    /// A Cook whose admission failed before materializing is relaunched from
    /// its recorded admission inputs under a fresh run id, and replaying
    /// `recover` returns that same relaunch instead of admitting another.
    #[test]
    fn recover_relaunches_an_unmaterialized_cook_from_its_admission_inputs() {
        use homeboy::agents::agent_task_lifecycle;
        use sha2::{Digest, Sha256};
        let digest = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
        crate::test_support::with_isolated_home(|home| {
            let cook_id = "cook-recover-admission";
            let prompt = home.path().join("prompt.txt");
            std::fs::write(&prompt, "Fix it.").unwrap();
            let route = home.path().join("route.txt");
            std::fs::write(&route, "discord:v1:thread:1").unwrap();
            let input_refs = serde_json::json!([
                { "kind": "prompt", "path": prompt, "sha256": digest(b"Fix it."), "argv_token": "homeboy-admission-ref:0:p" },
                { "kind": "notification-route", "path": route, "sha256": digest(b"discord:v1:thread:1"), "argv_token": "homeboy-admission-ref:1:r" },
            ]);
            let manifest_path = home.path().join("manifest.json");
            let manifest = serde_json::to_vec(&input_refs).unwrap();
            std::fs::write(&manifest_path, &manifest).unwrap();
            let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &store,
                cook_id,
                serde_json::json!({
                    "schema": "homeboy/unmaterialized-cook-binding/v1",
                    "request_ref": "sha256:request",
                    "worktree_ref": "repo@branch",
                    "placement": { "requested": "local" },
                    "admission_inputs": {
                        "schema": "homeboy/cook-admission-inputs/v1",
                        "cook_id": cook_id,
                        "argv": ["agent-task", "cook", "--issue", "https://github.com/Extra-Chill/homeboy/issues/18096", "--prompt", "homeboy-admission-ref:0:p", "--notification-route=homeboy-admission-ref:1:r", "--run-id", cook_id],
                        "input_refs": input_refs,
                        "input_manifest": { "kind": "manifest", "path": manifest_path, "sha256": digest(&manifest) },
                    },
                }),
                "blocked_runner_unavailable",
                "runner cannot accept jobs (reasons: pressure)",
            )
            .unwrap();
            store
                .mutate_record(cook_id, |record| {
                    record.metadata["detached_cook_handoff"]["admission_state"] =
                        serde_json::json!("failed");
                    record.metadata["execution_step_failure"] = serde_json::json!({
                        "code": "internal.io_error",
                        "message": "admission inputs could not be read after upgrade",
                    });
                    record
                        .set_state_for_test(homeboy_lifecycle_contract::RunExecutionState::Failed);
                    true
                })
                .unwrap();

            // #18845: diagnose must name a command the public recover entry
            // accepts even though admission never wrote a recipe.
            let cli = Cli::try_parse_from(["homeboy", "agent-task", "diagnose", cook_id]).unwrap();
            let Commands::AgentTask(task) = cli.command else {
                panic!("agent-task")
            };
            let AgentTaskCommand::Diagnose(args) = task.command else {
                panic!("diagnose")
            };
            let (diagnosis, _) = super::super::status::diagnose(args).unwrap();
            assert_eq!(
                diagnosis["recovery_step"]["kind"], "relaunch",
                "{diagnosis}"
            );
            assert!(diagnosis["next_commands"][0]
                .as_str()
                .unwrap()
                .contains("agent-task recover"));

            let recover = || {
                let cli = Cli::try_parse_from([
                    "homeboy",
                    "--placement",
                    "local",
                    "agent-task",
                    "recover",
                    cook_id,
                ])
                .unwrap();
                let Commands::AgentTask(task) = cli.command else {
                    panic!("agent-task")
                };
                let AgentTaskCommand::Recover(args) = task.command else {
                    panic!("recover")
                };
                homeboy::agents::agent_task_service::with_scheduling_daemon_start(
                    || Ok(()),
                    || super::recover(args),
                )
                .expect("recover relaunches the Cook")
            };
            let (report, exit_code) = recover();
            assert_eq!(exit_code, 0, "{report}");
            assert_eq!(report["step"]["kind"], "relaunch", "{report}");
            let relaunch_id = report["run_id"].as_str().unwrap().to_string();
            assert_ne!(relaunch_id, cook_id);

            let source = store.read_record(cook_id).unwrap();
            let relaunch = store.read_record(&relaunch_id).unwrap();
            assert_eq!(
                source.state(),
                homeboy_lifecycle_contract::RunExecutionState::Failed
            );
            assert_eq!(source.metadata["recovered_by"]["run_id"], relaunch_id);
            assert_eq!(relaunch.metadata["recovered_from"]["run_id"], cook_id);
            // The relaunch admits the same request: its admission inputs equal
            // the source's except for the run identity.
            let without_run_id = |record: &agent_task_lifecycle::AgentTaskRunRecord, id: &str| {
                let mut inputs = record.metadata["unmaterialized_cook_admission"]["binding"]
                    ["admission_inputs"]
                    .clone();
                inputs["cook_id"] = serde_json::Value::Null;
                for argument in inputs["argv"].as_array_mut().unwrap() {
                    if argument == id {
                        *argument = serde_json::Value::Null;
                    }
                }
                inputs
            };
            assert_eq!(
                without_run_id(&relaunch, &relaunch_id),
                without_run_id(&source, cook_id)
            );

            let (replay, exit_code) = recover();
            assert_eq!(exit_code, 0, "{replay}");
            assert_eq!(replay["run_id"], relaunch_id.as_str());
            assert_eq!(replay["result"]["replayed"], true);
            assert_eq!(store.read_records().unwrap().len(), 2, "one relaunch only");
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

    /// An explicit budget always applies, even at the CLI default; without
    /// `--regate` it is refused, never silently ignored (#18556).
    #[test]
    fn recover_regate_gate_budgets_apply_when_passed() {
        let request = |extra: &[&str]| {
            let mut argv = vec!["homeboy", "agent-task", "recover", "cook-1"];
            argv.extend_from_slice(extra);
            let Commands::AgentTask(task) = Cli::try_parse_from(argv).unwrap().command else {
                panic!("agent-task")
            };
            let AgentTaskCommand::Recover(args) = task.command else {
                panic!("recover")
            };
            super::recovery_request(args)
        };
        let regate = request(&[
            "--regate",
            "--gate-timeout-seconds",
            "1800",
            "--gate-no-progress-seconds",
            "0",
        ])
        .unwrap()
        .regate_inputs;
        assert_eq!(regate.gate_timeout_seconds, Some(1800));
        assert_eq!(regate.gate_no_progress_timeout_seconds, Some(0));
        let recorded = request(&["--regate"]).unwrap().regate_inputs;
        assert!(recorded.is_empty(), "{recorded:?}");
        assert!(request(&["--gate-no-progress-timeout-seconds", "600"]).is_err());
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

    /// Recover covers non-Cook runs too. Explicit retry, reconciliation and
    /// manual finalization stay available independently of recovery.
    #[test]
    fn recovery_adjacent_verbs_recover_does_not_perform_remain_commands() {
        for kept in [
            vec!["retry", "run-1", "--run"],
            vec!["reconcile", "run-1", "--apply"],
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
