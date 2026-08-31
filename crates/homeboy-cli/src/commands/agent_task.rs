//! `agent-task` command tree root.
//!
//! This module is a thin dispatcher. The CLI contract (arg/subcommand types)
//! lives in [`args`], and each command family is handled by a focused sibling
//! module: [`auth`], [`controller`], [`run`], [`status`], and [`review`].

use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

use super::CmdResult;

pub mod args;
pub mod auth;
pub(crate) mod candidate;
pub mod contract;
pub mod controller;
pub(crate) mod default_branch;
pub mod doctor;
pub mod fanout;
pub(crate) mod gate_contract;
pub mod loop_definition;
pub mod prompts;
pub mod retained_artifacts;
pub mod review;
pub mod run;
pub mod status;
pub mod tool;

pub use args::{
    AcceptArgs, ActiveArgs, AdoptArgs, AgentTaskArgs, AgentTaskAuthArgs, AgentTaskAuthCommand,
    AgentTaskCommand, AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs,
    AgentTaskControllerCommand, AgentTaskControllerDispatchArgs, AgentTaskControllerFromSpecArgs,
    AgentTaskControllerInitArgs, AgentTaskControllerMarkHumanReadyArgs,
    AgentTaskControllerMaterializeArgs, AgentTaskControllerRunArgs,
    AgentTaskControllerRunFromSpecArgs, AgentTaskControllerRunNextArgs,
    AgentTaskControllerStatusArgs, AgentTaskControllerValidateProofArgs, AgentTaskCookArgs,
    AgentTaskDoctorArgs, AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs,
    AgentTaskFanoutCommand, AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs,
    AgentTaskFanoutPlanArgs, AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs,
    AgentTaskFanoutSubmitBatchArgs, AgentTaskLoopArgs, AgentTaskLoopCommand,
    AgentTaskLoopDefineArgs, AgentTaskLoopResumeArgs, AgentTaskLoopStatusArgs, CancelArgs,
    CompileLoopArgs, ContractArgs, ContractFormat, CookContinueArgs, DiagnoseArgs, EvidenceArgs,
    FinalizePrArgs, GateFeedbackArgs, LatestArgs, ListArgs, LogsArgs, PromoteArgs,
    PromotionProviderArgs, ProvidersArgs, QuarantineArgs, RearmArgs, ReconcileRecordsArgs,
    RecordReplacementGateProofArgs, ReplayProviderBoundaryArgs, RetainedArtifactsArgs,
    RetainedArtifactsCommand, RetryArgs, ReviewArgs, RunPlanArgs, RuntimeRecoverArgs,
    RuntimeValidateArgs, StatusArgs, SubmitArgs, ValidatePlanArgs, VerifyGateArgs,
    VerifyReplacementArgs,
};
pub(crate) use status::diagnostic_summary_from_aggregate;

pub(crate) type CookProgressCallback<'a> = dyn Fn(&str, Option<&str>, Option<&str>, Option<&str>, Option<&str>) -> homeboy::core::Result<()>
    + Send
    + Sync
    + 'a;
pub(crate) type CookProgress<'a> = Option<&'a CookProgressCallback<'a>>;

pub fn run(args: AgentTaskArgs) -> CmdResult<Value> {
    // Announce durable identity exactly once, on the first progress event that
    // carries a run id, and do it outside the TTY gate. Phase chatter stays
    // TTY-gated so non-interactive logs are not spammed, but the operator
    // handle itself must reach every caller — a non-TTY client that is
    // interrupted mid-cook otherwise has no way to answer "what did I just
    // start?" (#10419).
    let announced_identity = std::sync::atomic::AtomicBool::new(false);
    let no_progress = matches!(&args.command, AgentTaskCommand::Cook(cook) if cook.no_progress);
    let reporter = CookProgressReporter::new(no_progress);
    let progress = |phase: &str,
                    cook_id: Option<&str>,
                    run_id: Option<&str>,
                    activity: Option<&str>,
                    terminal_retry_command: Option<&str>| {
        if let Some(run_id) = run_id {
            if !announced_identity.swap(true, std::sync::atomic::Ordering::SeqCst) {
                run::announce_durable_cook_identity(cook_id, run_id);
            }
        }
        reporter.report(phase, cook_id, run_id, activity, terminal_retry_command);
        Ok(())
    };
    run_with_cook_progress(args, Some(&progress))
}

/// Global placement selects child provider attempts; fanout coordination itself
/// remains controller-local.
pub(crate) fn run_with_placement(
    args: AgentTaskArgs,
    placement: crate::cli_surface::Placement,
) -> CmdResult<Value> {
    if let AgentTaskCommand::Fanout(fanout_args) = args.command {
        return fanout::fanout_with_placement(fanout_args, placement);
    }
    run(args)
}

fn cook_progress_message(phase: &str, cook_id: Option<&str>, run_id: Option<&str>) -> String {
    let identity = match (cook_id, run_id) {
        (_, Some(run_id)) => format!(" [{run_id}]"),
        (Some(cook_id), None) => format!(" [{cook_id}]"),
        (None, None) => String::new(),
    };
    match run_id {
        Some(run_id) => format!(
            "Cook {phase}: durable run `{run_id}`. Follow: `homeboy agent-task status {run_id} --watch`. Status: `homeboy agent-task status {run_id}`. Evidence: `homeboy agent-task evidence {run_id} --full`."
        ),
        None => format!("Cook {phase}{identity}."),
    }
}

fn cook_terminal_progress_message(
    run_id: &str,
    outcome: Option<&str>,
    retry_command: Option<&str>,
) -> String {
    let mut message = format!(
        "Cook terminal: durable run `{run_id}`. Phase: terminal. Outcome: {}. Next: `homeboy agent-task status {run_id}`.",
        outcome.unwrap_or("terminal")
    );
    if outcome == Some("failed") {
        message.push_str(&format!(
            " Diagnose: `homeboy agent-task diagnose {run_id} --full`."
        ));
        if let Some(retry_command) = retry_command {
            message.push_str(&format!(" Retry: `{retry_command}`."));
        }
    }
    message
}

const MAX_MACHINE_PROGRESS_LINES: usize = 12;

#[derive(Debug, Default)]
struct CookProgressState {
    pointer_emitted: bool,
    emitted_lines: usize,
    heartbeat_count: usize,
    last_phase: Option<String>,
    /// The terminal report is allowed one line beyond the progress budget.
    terminal_emitted: bool,
    /// Last provider-activity sentence emitted, so an unchanged sample stays
    /// silent while a genuinely new one gets a line.
    last_activity: Option<String>,
}

/// Bounds foreground Cook progress independently from durable lifecycle events.
/// Durable status and evidence remain lossless in the final command envelope.
#[derive(Clone, Debug)]
pub(crate) struct CookProgressReporter {
    no_progress: bool,
    state: Arc<Mutex<CookProgressState>>,
}

impl CookProgressReporter {
    pub(crate) fn new(no_progress: bool) -> Self {
        Self {
            no_progress,
            state: Arc::new(Mutex::new(CookProgressState::default())),
        }
    }

    pub(crate) fn report(
        &self,
        phase: &str,
        cook_id: Option<&str>,
        run_id: Option<&str>,
        activity: Option<&str>,
        terminal_retry_command: Option<&str>,
    ) {
        if self.no_progress {
            return;
        }
        if crate::commands::utils::tty::is_stdout_tty() {
            if phase == "terminal" {
                if let Some(run_id) = run_id {
                    crate::commands::utils::tty::status(&cook_terminal_progress_message(
                        run_id,
                        activity,
                        terminal_retry_command,
                    ));
                    return;
                }
            }
            crate::commands::utils::tty::status(&format!(
                "cook: {phase}{}{}",
                run_id
                    .map(|run_id| format!(" [{run_id}]"))
                    .or_else(|| cook_id.map(|cook_id| format!(" [{cook_id}]")))
                    .unwrap_or_default(),
                activity
                    .map(|activity| format!(" — {activity}"))
                    .unwrap_or_default()
            ));
            return;
        }

        let mut state = self.state.lock().expect("cook progress state");
        if let Some(message) = next_machine_progress_message(
            &mut state,
            phase,
            cook_id,
            run_id,
            activity,
            terminal_retry_command,
        ) {
            eprintln!("{message}");
        }
    }
}

fn next_machine_progress_message(
    state: &mut CookProgressState,
    phase: &str,
    cook_id: Option<&str>,
    run_id: Option<&str>,
    activity: Option<&str>,
    terminal_retry_command: Option<&str>,
) -> Option<String> {
    // Preserve one final bounded outcome line even when heartbeat sampling has
    // consumed the progress budget. The initial pointer remains the stable
    // recovery command if the caller is interrupted before this point.
    let terminal = phase == "terminal";
    if (terminal && state.terminal_emitted)
        || (state.emitted_lines >= MAX_MACHINE_PROGRESS_LINES && !terminal)
    {
        return None;
    }
    let identity = run_id
        .map(|run_id| format!(" [{run_id}]"))
        .or_else(|| cook_id.map(|cook_id| format!(" [{cook_id}]")))
        .unwrap_or_default();
    if phase == "heartbeat" {
        state.heartbeat_count += 1;
        // Sampled liveness is emitted on a fixed schedule; a *changed* provider
        // activity earns an extra line because it is new information, not
        // repetition. Both stay under `MAX_MACHINE_PROGRESS_LINES`, so this
        // cannot become the heartbeat flood that cap exists to prevent — but it
        // does mean an operator watching a stalled cook sees the moment it
        // started compiling instead of a wall of byte-identical lines (#11482).
        let activity_changed = activity.is_some() && state.last_activity.as_deref() != activity;
        if activity_changed {
            state.last_activity = activity.map(str::to_string);
        }
        if !matches!(state.heartbeat_count, 1 | 4 | 8) && !activity_changed {
            return None;
        }
        state.emitted_lines += 1;
        let detail = match activity {
            Some(activity) => format!(" {activity}."),
            None => String::new(),
        };
        return Some(format!(
            "Cook heartbeat{identity}: still running (liveness sample {}).{detail}",
            state.heartbeat_count
        ));
    }
    state.heartbeat_count = 0;
    let message = if terminal {
        run_id.map(|run_id| {
            cook_terminal_progress_message(run_id, activity, terminal_retry_command)
        })?
    } else if !state.pointer_emitted && run_id.is_some() {
        state.pointer_emitted = true;
        cook_progress_message(phase, cook_id, run_id)
    } else if state.last_phase.as_deref() != Some(phase) {
        format!("Cook {phase}{identity}.")
    } else {
        return None;
    };
    state.last_phase = Some(phase.to_string());
    state.terminal_emitted |= terminal;
    state.emitted_lines += 1;
    Some(message)
}

pub(crate) fn run_with_cook_progress(
    args: AgentTaskArgs,
    progress: CookProgress<'_>,
) -> CmdResult<Value> {
    run_with_cook_progress_and_provenance(args, progress, None)
}

pub(crate) fn run_with_cook_progress_and_provenance(
    args: AgentTaskArgs,
    progress: CookProgress<'_>,
    provenance: Option<&crate::cli_surface::CommandArgumentProvenance>,
) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Doctor(doctor_args) => doctor::doctor(doctor_args),
        AgentTaskCommand::Cook(mut cook_args) => {
            // Consume a redirected prompt before routing can hand Cook to another
            // process. The captured value, rather than stdin, is then the input
            // every preview and execution path compiles.
            run::snapshot_cook_prompt(&mut cook_args)?;
            if cook_args.preview {
                return run::preview_cook(*cook_args, provenance);
            }
            // Reject unsupported Cook source shapes before discovering a provider
            // or preparing the local/Lab execution route.
            run::validate_cook_request_with_provenance(&cook_args, provenance)?;
            if progress.is_some() {
                run::run_cook_with_executor_and_dispatcher_with_progress(
                    *cook_args,
                    std::sync::Arc::new(
                        homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor::discover(),
                    ),
                    None,
                    progress,
                    provenance,
                )
            } else {
                run::run_cook(*cook_args)
            }
        }
        AgentTaskCommand::CookContinue(args) if args.preflight => {
            run::preflight_continue_cook(args)
        }
        AgentTaskCommand::CookContinue(args) => run::continue_cook(args),
        AgentTaskCommand::Loop(loop_args) => controller::loop_command(loop_args),
        AgentTaskCommand::RunPlan(run_args) => run::run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run::run_submitted(status_args),
        AgentTaskCommand::RunNext(args) => run::run_next(args),
        AgentTaskCommand::Submit(submit_args) => run::submit(submit_args),
        AgentTaskCommand::ValidatePlan(args) => run::validate_plan(args),
        AgentTaskCommand::Status(status_args) => status::status(status_args),
        // Alias, not a second watch loop: route straight into `activity watch`,
        // which already resolves cook ids, durable run ids, observation run ids
        // and runner job ids across every activity source (#W3-15).
        AgentTaskCommand::Watch(watch_args) => {
            let (output, exit_code) = crate::commands::activity::watch_alias(watch_args)?;
            Ok((
                serde_json::to_value(output).unwrap_or(Value::Null),
                exit_code,
            ))
        }
        AgentTaskCommand::List(list_args) => {
            if list_args.latest {
                status::list_filtered_latest_runs(list_args.into())
            } else {
                status::list_runs(
                    agent_task_service::AgentTaskDiscoveryFilter::All,
                    list_args.into(),
                )
            }
        }
        AgentTaskCommand::Active(active_args) => {
            if active_args.reconcile {
                status::reconcile_active(!active_args.apply)
            } else {
                status::list_active(active_args.into())
            }
        }
        AgentTaskCommand::Reconcile(args) => status::reconcile_run(args),
        AgentTaskCommand::ReconcileRecords(args) => status::reconcile_records(args.dry_run),
        AgentTaskCommand::Latest(latest_args) => status::list_runs(
            agent_task_service::AgentTaskDiscoveryFilter::Latest,
            latest_args.into(),
        ),
        AgentTaskCommand::Logs(status_args) => status::logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => status::artifacts(status_args),
        AgentTaskCommand::RetainedArtifacts(args) => retained_artifacts::run(args),
        AgentTaskCommand::Evidence(evidence_args) => status::evidence(evidence_args),
        AgentTaskCommand::Diagnose(diagnose_args) => status::diagnose(diagnose_args),
        AgentTaskCommand::RuntimeRecover(args) => status::recover_runtime(args),
        AgentTaskCommand::RuntimeValidate(args) => status::validate_runtime(args),
        AgentTaskCommand::ReplayProviderBoundary(replay_args) => {
            status::replay_provider_boundary(replay_args)
        }
        AgentTaskCommand::Cancel(cancel_args) => status::cancel(cancel_args),
        AgentTaskCommand::Quarantine(args) => status::quarantine(args),
        AgentTaskCommand::Rearm(args) => status::rearm(args),
        AgentTaskCommand::Resume(status_args) => run::resume(status_args),
        AgentTaskCommand::Retry(retry_args) => run::retry(retry_args),
        AgentTaskCommand::Fanout(fanout_args) => fanout::fanout(fanout_args),
        AgentTaskCommand::Review(review_args) => review::review(review_args),
        AgentTaskCommand::Promote(promote_args) => review::promote_artifact(*promote_args),
        AgentTaskCommand::Adopt(adopt_args) => review::adopt_candidate(adopt_args),
        AgentTaskCommand::PromotionProvider(provider_args) => {
            run::promotion_provider(provider_args)
        }
        AgentTaskCommand::FinalizePr(finalize_args) => {
            review::finalize_pull_request(*finalize_args)
        }
        AgentTaskCommand::RecordReplacementGateProof(args) => {
            review::record_replacement_gate_proof(args)
        }
        AgentTaskCommand::VerifyReplacement(args) => review::verify_replacement(args),
        AgentTaskCommand::Accept(args) => {
            let verdict = if args.verdict == "accepted" {
                homeboy::agents::agent_tasks::lifecycle::AgentTaskAcceptanceVerdict::Accepted
            } else {
                homeboy::agents::agent_tasks::lifecycle::AgentTaskAcceptanceVerdict::Rejected
            };
            let record =
                homeboy::agents::agent_tasks::lifecycle::record_acceptance_verdict_with_feedback_in_store(
                    &homeboy::agents::agent_tasks::lifecycle::AgentTaskLifecycleStore::from_current_environment()?,
                    &args.run_id,
                    verdict,
                    args.evidence_refs,
                    args.token,
                    args.feedback,
                )?;
            Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
        }
        AgentTaskCommand::GateFeedback(feedback_args) => review::gate_feedback(feedback_args),
        AgentTaskCommand::Providers(providers_args) => review::providers(providers_args),
        AgentTaskCommand::Prompts(prompts_args) => prompts::prompts(prompts_args),
        AgentTaskCommand::Contract(contract_args) => contract::contract(contract_args),
        AgentTaskCommand::CompileLoop(compile_args) => loop_definition::compile_loop(compile_args),
        AgentTaskCommand::Auth(auth_args) => auth::auth(auth_args),
        AgentTaskCommand::Controller(controller_args) => controller::controller(controller_args),
        AgentTaskCommand::Tool(tool_args) => match tool_args.command {
            tool::AgentTaskToolCommand::Dispatch(_) => {
                Err(homeboy::core::Error::validation_invalid_argument(
                    "agent-task tool dispatch",
                    "this internal bridge command is handled by the raw CLI runtime",
                    None,
                    None,
                ))
            }
        },
    }
}

#[cfg(test)]
mod progress_tests {
    use super::{
        cook_progress_message, cook_terminal_progress_message, next_machine_progress_message,
        CookProgressState, MAX_MACHINE_PROGRESS_LINES,
    };

    #[test]
    fn non_tty_cook_progress_includes_durable_reconnect_commands() {
        // The formatter is the non-TTY client contract: callers can reconnect
        // after an interrupted observation without guessing the run identity.
        let message = cook_progress_message("provider_ready", None, Some("run-123"));
        assert!(message.contains("agent-task status run-123"));
        assert!(message.contains("agent-task status run-123 --watch"));
        assert!(message.contains("agent-task evidence run-123 --full"));
    }

    #[test]
    fn long_unchanged_phase_has_bounded_machine_liveness_output() {
        let mut state = CookProgressState::default();
        let mut messages = Vec::new();
        for _ in 0..100 {
            if let Some(message) = next_machine_progress_message(
                &mut state,
                "heartbeat",
                None,
                Some("run-123"),
                None,
                None,
            ) {
                messages.push(message);
            }
        }

        assert_eq!(messages.len(), 3, "heartbeats must not scale with polls");
        assert!(messages
            .iter()
            .all(|message| message.contains("still running")));
    }

    #[test]
    fn heartbeats_carry_the_provider_activity_that_diagnoses_a_stalled_cook() {
        // The whole point of #11482: the heartbeat has to say what the agent is
        // doing, not merely that something is running.
        let mut state = CookProgressState::default();

        let message = next_machine_progress_message(
            &mut state,
            "heartbeat",
            None,
            Some("run-123"),
            Some("no files written yet, 6m12s in `cargo test -p homeboy-agents`"),
            None,
        )
        .expect("first heartbeat is emitted");

        assert!(message.contains("no files written yet"));
        assert!(message.contains("cargo test -p homeboy-agents"));
    }

    #[test]
    fn an_unchanged_activity_sample_does_not_add_heartbeat_lines() {
        // Repeating the same sentence is the flood the sampling cap exists to
        // prevent (#10455, #11087); only new information earns a line.
        let mut state = CookProgressState::default();
        let mut messages = Vec::new();
        for _ in 0..100 {
            if let Some(message) = next_machine_progress_message(
                &mut state,
                "heartbeat",
                None,
                Some("run-123"),
                Some("no files written yet"),
                None,
            ) {
                messages.push(message);
            }
        }

        assert_eq!(
            messages.len(),
            3,
            "a static sample keeps the sampled cadence"
        );
    }

    #[test]
    fn a_changed_activity_earns_a_line_but_stays_bounded() {
        // An operator watching a cook must see the moment it stopped editing
        // and started compiling — bounded by the same total line budget.
        let mut state = CookProgressState::default();
        let mut messages = Vec::new();
        for sample in 0..100 {
            let activity = format!("no files written yet, {sample}s in `cargo test`");
            if let Some(message) = next_machine_progress_message(
                &mut state,
                "heartbeat",
                None,
                Some("run-123"),
                Some(&activity),
                None,
            ) {
                messages.push(message);
            }
        }

        assert_eq!(
            messages.len(),
            MAX_MACHINE_PROGRESS_LINES,
            "changing activity is still capped by the machine progress budget"
        );
    }

    #[test]
    fn terminal_machine_progress_is_emitted_after_the_heartbeat_budget() {
        let mut state = CookProgressState::default();
        for heartbeat in 0..MAX_MACHINE_PROGRESS_LINES {
            let activity = format!("sample {heartbeat}");
            assert!(next_machine_progress_message(
                &mut state,
                "heartbeat",
                None,
                Some("run-123"),
                Some(&activity),
                None,
            )
            .is_some());
        }

        let terminal = next_machine_progress_message(
            &mut state,
            "terminal",
            None,
            Some("run-123"),
            Some("failed"),
            Some("homeboy agent-task retry run-123 --run"),
        )
        .expect("terminal outcome remains visible");

        assert_eq!(
            terminal,
            "Cook terminal: durable run `run-123`. Phase: terminal. Outcome: failed. Next: `homeboy agent-task status run-123`. Diagnose: `homeboy agent-task diagnose run-123 --full`. Retry: `homeboy agent-task retry run-123 --run`."
        );
        assert_eq!(state.emitted_lines, MAX_MACHINE_PROGRESS_LINES + 1);
        assert!(next_machine_progress_message(
            &mut state,
            "terminal",
            None,
            Some("run-123"),
            Some("failed"),
            Some("homeboy agent-task retry run-123 --run"),
        )
        .is_none());
        assert_eq!(state.emitted_lines, MAX_MACHINE_PROGRESS_LINES + 1);
    }

    #[test]
    fn terminal_machine_progress_is_emitted_once_before_the_heartbeat_budget() {
        let mut state = CookProgressState::default();

        assert!(next_machine_progress_message(
            &mut state,
            "terminal",
            None,
            Some("run-123"),
            Some("failed"),
            Some("homeboy agent-task retry run-123 --run"),
        )
        .is_some());
        assert!(next_machine_progress_message(
            &mut state,
            "terminal",
            None,
            Some("run-123"),
            Some("failed"),
            Some("homeboy agent-task retry run-123 --run"),
        )
        .is_none());
        assert_eq!(state.emitted_lines, 1);
    }

    #[test]
    fn terminal_machine_progress_names_a_successful_outcome() {
        let mut state = CookProgressState::default();
        let terminal = next_machine_progress_message(
            &mut state,
            "terminal",
            None,
            Some("run-123"),
            Some("succeeded"),
            None,
        )
        .expect("terminal outcome remains visible");

        assert_eq!(
            terminal,
            "Cook terminal: durable run `run-123`. Phase: terminal. Outcome: succeeded. Next: `homeboy agent-task status run-123`."
        );
    }

    #[test]
    fn terminal_progress_only_advertises_durably_legal_retries_on_tty_and_non_tty() {
        // Both output modes render this same terminal sentence. The durable
        // recovery action, not the process exit code, is its retry authority.
        let cases = [
            (
                "pre-execution",
                Some("homeboy agent-task retry run-123 --run"),
                true,
            ),
            ("provider", None, false),
            ("gate", None, false),
            ("finalization", None, false),
            ("policy/nonretryable", None, false),
            ("success", None, false),
        ];

        for (path, retry_command, expects_retry) in cases {
            let outcome = (path != "success")
                .then_some("failed")
                .or(Some("succeeded"));
            let tty = cook_terminal_progress_message("run-123", outcome, retry_command);
            let non_tty = next_machine_progress_message(
                &mut CookProgressState::default(),
                "terminal",
                None,
                Some("run-123"),
                outcome,
                retry_command,
            )
            .expect("terminal progress is emitted once");

            assert_eq!(tty, non_tty, "{path} uses one terminal contract");
            assert_eq!(
                tty.contains("agent-task retry run-123 --run"),
                expects_retry,
                "{path} retry guidance follows durable legal_actions"
            );
        }
    }
}

use homeboy::agents::agent_tasks::service as agent_task_service;

pub(crate) fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod output_projection_tests {
    use super::status::project_operator_output;
    use serde_json::json;

    const REALISTIC_OPERATOR_RESPONSE_MAX_BYTES: usize = 24 * 1024;

    fn production_shaped_evidence() -> serde_json::Value {
        let event = json!({ "timestamp": "2026-08-12T00:00:00Z", "message": "x".repeat(512) });
        json!({
            "hydrated_evidence": [{
                "kind": "provider-transcript",
                "uri": "file:///tmp/provider-transcript.json",
                "status": "ok",
                "content": {
                    "current_diff": "d".repeat(256 * 1024),
                    "transcript": "t".repeat(256 * 1024),
                    "runtime_log": "r".repeat(256 * 1024),
                    "body": "b".repeat(256 * 1024),
                    "raw_events": vec![event.clone(); 80],
                    "resource_timeline": vec![event; 80],
                }
            }],
            "artifact_refs": [{ "kind": "patch", "uri": "file:///tmp/change.patch" }],
            "next_actions": [{ "command": "homeboy agent-task status run-1" }],
            "identity": { "run_id": "run-1" },
            "gates": [{ "name": "check", "status": "passed" }],
        })
    }

    #[test]
    fn default_operator_projection_bounds_expanding_evidence_families() {
        let mut value = production_shaped_evidence();
        let original = value.clone();

        project_operator_output(&mut value);

        assert!(
            serde_json::to_vec(&value)
                .expect("serialize projection")
                .len()
                < REALISTIC_OPERATOR_RESPONSE_MAX_BYTES
        );
        let content = &value["hydrated_evidence"][0]["content"];
        for field in ["current_diff", "transcript", "runtime_log", "body"] {
            assert!(
                content[field].is_string(),
                "{field} keeps its string schema"
            );
            assert!(content[field].as_str().unwrap().len() < 256);
        }
        assert_eq!(content["raw_events"].as_array().unwrap().len(), 12);
        assert_eq!(content["raw_events_projection"]["omitted_items"], 68);
        assert_eq!(content["resource_timeline"].as_array().unwrap().len(), 12);
        assert_eq!(content["resource_timeline_projection"]["omitted_items"], 68);
        assert_eq!(
            value["next_actions"][0]["command"],
            "homeboy agent-task status run-1"
        );
        assert_eq!(value["identity"]["run_id"], "run-1");
        assert_eq!(value["gates"][0]["status"], "passed");
        assert_eq!(value["artifact_refs"], original["artifact_refs"]);
    }

    #[test]
    fn full_projection_is_lossless() {
        let value = production_shaped_evidence();
        let expected = value.clone();

        assert_eq!(value, expected);
    }

    #[test]
    fn default_diagnosis_keeps_cause_and_refs_without_runtime_payloads() {
        let mut value = json!({
            "schema": "homeboy/agent-task-diagnose/v1",
            "run_id": "run-diagnosis",
            "state": "failed",
            "root_cause": {
                "class": "provider.process_stream",
                "message": "task worktree has no .git",
                "details": { "transcript": "t".repeat(256 * 1024) },
            },
            "hydrated_evidence": [{
                "kind": "provider-transcript",
                "uri": "file:///tmp/transcript.json",
                "status": "ok",
                "content": {
                    "body": "b".repeat(256 * 1024),
                    "runtime_log": "r".repeat(256 * 1024),
                    "raw_events": vec![json!({ "body": "event".repeat(512) }); 100],
                },
            }],
            "next_commands": ["homeboy agent-task evidence run-diagnosis --full"],
        });
        let full = value.clone();

        project_operator_output(&mut value);

        assert!(serde_json::to_vec(&value).unwrap().len() < REALISTIC_OPERATOR_RESPONSE_MAX_BYTES);
        assert_eq!(value["root_cause"]["class"], "provider.process_stream");
        assert_eq!(value["root_cause"]["message"], "task worktree has no .git");
        assert!(value["root_cause"]["details"]["transcript"]
            .as_str()
            .unwrap()
            .starts_with("[omitted"));
        assert_eq!(
            value["next_commands"][0],
            "homeboy agent-task evidence run-diagnosis --full"
        );
        assert_eq!(
            full["hydrated_evidence"][0]["content"]["body"]
                .as_str()
                .unwrap()
                .len(),
            256 * 1024
        );
    }
}
