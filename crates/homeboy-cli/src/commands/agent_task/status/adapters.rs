//! Watch polling and output, artifact/evidence reads, and cancellation adapters.

use super::*;
use homeboy::core::error::posix_quote_arg;

pub(super) struct StatusPoller {
    pub(super) args: StatusArgs,
    /// Waiting on a Cook admission: an attempt its replay is still
    /// materializing (recipe written, lifecycle record not yet) is not an end.
    pub(super) await_materialization: bool,
    pub(super) recovery_step: bool,
}

impl WatchPoller for StatusPoller {
    type Item = (Value, i32);

    fn poll(&self, _id: &str) -> homeboy::core::Result<Self::Item> {
        // A foreground `recover --wait` stops on SIGINT/SIGTERM; the step
        // stays owned by the daemon (#18042).
        if let Some(stop) = homeboy::core::foreground_wait::stop_requested() {
            return Err(stop.error(
                "agent-task recover --wait",
                None,
                &format!(
                    "the step stays owned by the daemon; follow it with `homeboy agent-task status {} --watch`",
                    self.args.run_id
                ),
            ));
        }
        status_once(self.args.clone())
    }

    fn is_terminal(&self, item: &Self::Item) -> bool {
        if self.recovery_step {
            return agent_task_service_direct::recovery_step_exit_code(&self.args.run_id)
                .ok()
                .flatten()
                .is_some();
        }
        if self.await_materialization && item.0["status"] == "recipe_only_recovery_required" {
            // The admission stays open until its replay materializes the
            // attempt or terminalizes the admission itself.
            return agent_task_lifecycle::exact_record(&self.args.run_id)
                .map_or(true, |admission| admission.state().is_terminal());
        }
        if agent_task_service_direct::recovery_step_in_flight(&self.args.run_id)
            .ok()
            .flatten()
            .is_some()
        {
            return false;
        }
        status_is_terminal(&item.0) && !cook_driver_still_owns(&item.0)
    }
}

/// A Cook advances an attempt through intermediate states that look terminal
/// on their own: the provider hands back a patch (`candidate_recoverable` /
/// `patch_available`) before the same driver applies it and runs the gates.
/// While a live driver holds the Cook, those states are not its verdict.
pub(super) fn cook_driver_still_owns(status: &Value) -> bool {
    status
        .get("mission")
        .and_then(Value::as_str)
        .is_some_and(|cook_id| {
            agent_task_service_direct::cook_driver_is_live(cook_id).unwrap_or(false)
        })
}

pub(super) fn watch_status(args: StatusArgs) -> CmdResult<Value> {
    let timeout = parse_duration("--timeout", &args.timeout)?;
    watch_status_with_timeout(args, Some(timeout))
}

/// Global `--wait` observes durable ownership until terminal completion rather
/// than treating acceptance or the ordinary bounded watch timeout as success.
pub(crate) fn wait_for_terminal_status(run_id: &str) -> CmdResult<Value> {
    watch_status_until(
        StatusArgs {
            run_id: run_id.to_string(),
            full: false,
            exact: false,
            // A successful --no-finalize run may still offer follow-up actions. Its
            // available follow-up actions do not make terminal execution fail.
            strict_subject_exit: false,
            watch: true,
            interval: "1s".to_string(),
            timeout: "30m".to_string(),
        },
        None,
        true,
    )
}

pub(crate) fn wait_for_recovery_status(
    run_id: &str,
    full: bool,
    timeout: Duration,
) -> CmdResult<Value> {
    watch_status_until(
        StatusArgs {
            run_id: run_id.into(),
            full,
            exact: false,
            strict_subject_exit: false,
            watch: true,
            interval: "1s".into(),
            timeout: "30m".into(),
        },
        Some(timeout),
        true,
    )
}

pub(super) fn watch_status_with_timeout(
    args: StatusArgs,
    timeout: Option<Duration>,
) -> CmdResult<Value> {
    watch_status_until(args, timeout, false)
}

pub(super) fn watch_status_until(
    mut args: StatusArgs,
    timeout: Option<Duration>,
    await_materialization: bool,
) -> CmdResult<Value> {
    let interval = parse_duration("--interval", &args.interval)?;
    args.watch = false;
    let poller = StatusPoller {
        args: args.clone(),
        await_materialization,
        recovery_step: agent_task_service_direct::recovery_step_in_flight(&args.run_id)
            .ok()
            .flatten()
            .is_some(),
    };
    let started = Instant::now();
    let mut progress = StatusWatchProgress::default();
    let mut backoff = StatusWaitInterval::default();
    let result = watch_loop_with_interval(
        &poller,
        &args.run_id,
        &WatchConfig { interval, timeout },
        sleep_unless_stopped,
        || started.elapsed(),
        |(snapshot, _), poll| progress.observe(snapshot, poll, |line| eprintln!("{line}")),
        |(snapshot, _)| {
            if await_materialization {
                backoff.observe(status_change_projection(snapshot))
            } else {
                interval
            }
        },
    )?;
    Ok(watch_status_output(&args, result, progress))
}

/// Sleep in short slices so a foreground stop (#18042) ends the wait promptly.
fn sleep_unless_stopped(duration: Duration) {
    let wake = Instant::now() + duration;
    while homeboy::core::foreground_wait::stop_requested().is_none() {
        let remaining = wake.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(200)));
    }
}

/// Unchanged durable status projections need progressively fewer reads.
#[derive(Default)]
pub(super) struct StatusWaitInterval {
    previous: Option<Value>,
    seconds: u64,
}

impl StatusWaitInterval {
    pub(super) fn observe(&mut self, projection: Value) -> Duration {
        self.seconds = if self.previous.as_ref() == Some(&projection) {
            (self.seconds * 2).min(15)
        } else {
            1
        };
        self.previous = Some(projection);
        Duration::from_secs(self.seconds)
    }
}

#[derive(Default)]
pub(super) struct StatusWatchProgress {
    pub(super) last_change: Option<Value>,
    pub(super) changes: Vec<Value>,
    pub(super) omitted: u64,
}

impl StatusWatchProgress {
    pub(super) fn observe(&mut self, snapshot: &Value, poll: u64, mut emit: impl FnMut(String)) {
        let change = status_change_projection(snapshot);
        if self.last_change.as_ref() == Some(&change) {
            return;
        }
        self.last_change = Some(change);
        emit(emit_status_change_event(
            snapshot,
            poll,
            self.changes.len() >= STATUS_WATCH_CHANGE_LIMIT,
        ));
        if self.changes.len() < STATUS_WATCH_CHANGE_LIMIT {
            self.changes.push(status_watch_change(snapshot, poll));
        } else {
            self.omitted += 1;
        }
    }
}

pub(super) fn emit_status_change_event(
    snapshot: &Value,
    poll: u64,
    retained_limit_reached: bool,
) -> String {
    let run_id = status_run_id(snapshot).or_else(|| {
        snapshot
            .pointer("/identity/resolved_run_id")
            .and_then(Value::as_str)
    });
    let mut event = json!({
        "schema": "homeboy/agent-task-status-watch-event/v2",
        "event": "status_changed",
        "run_id": run_id,
        "state": status_run_state(snapshot),
        "poll": poll,
        "change": status_watch_change(snapshot, poll),
        "retained_limit_reached": retained_limit_reached,
        "continuation_command": run_id.map(|run_id| format!(
            "homeboy agent-task status {} --watch", posix_quote_arg(run_id)
        )),
    });
    if serialized_len(&event) > STATUS_WATCH_EVENT_BYTE_LIMIT {
        event["change"] = json!({
            "run_id": run_id,
            "state": status_run_state(snapshot),
            "change_basis": status_change_projection(snapshot),
            "full_status_ref": snapshot.get("full_command"),
        });
    }
    let line = serde_json::to_string(&event).expect("status watch JSONL event serializes");
    if line.len() <= STATUS_WATCH_EVENT_BYTE_LIMIT {
        return line;
    }
    event["change"] = json!({
        "run_id": run_id,
        "state": status_run_state(snapshot),
        "full_status_ref": snapshot.get("full_command"),
    });
    serde_json::to_string(&event).expect("bounded status watch JSONL event serializes")
}

pub(super) fn status_watch_change(snapshot: &Value, poll: u64) -> Value {
    let change_basis = status_change_projection(snapshot);
    let mut change = change_basis.clone();
    change["poll"] = json!(poll);
    change["run_id"] = json!(status_run_id(snapshot));
    change["change_basis"] = change_basis;
    if serialized_len(&change) > STATUS_WATCH_CHANGE_PAYLOAD_BYTE_LIMIT {
        return json!({
            "poll": poll,
            "run_id": status_run_id(snapshot),
            "state": status_run_state(snapshot),
            "change_basis": status_change_projection(snapshot),
        });
    }
    change
}

pub(super) fn status_change_projection(status: &Value) -> Value {
    json!({
        "state": status_run_state(status),
        "phase": status.get("phase"),
        "blocker": status.get("blocker"),
        "heartbeat_at": status.get("heartbeat_at"),
        "candidate": status.get("candidate"),
        "gates": status.get("gates"),
        "publication": status.get("publication"),
        "action_eligibility": status.get("action_eligibility"),
    })
}

pub(super) fn status_is_terminal(status: &Value) -> bool {
    canonical_status_state(status).is_none_or(|state| state.is_terminal())
}

pub(super) fn status_is_failure(status: &Value) -> bool {
    canonical_status_state(status).is_some_and(|state| {
        state.is_terminal() && state != homeboy_control_plane_contract::ControlPlaneState::Succeeded
    })
}

pub(super) fn canonical_status_state(
    status: &Value,
) -> Option<homeboy_control_plane_contract::ControlPlaneState> {
    serde_json::from_value(status_run_state(status)?.clone()).ok()
}

pub(super) fn watch_status_output(
    args: &StatusArgs,
    result: WatchResult<(Value, i32)>,
    progress: StatusWatchProgress,
) -> (Value, i32) {
    let timed_out = result.conclusion == WatchConclusion::TimedOut;
    let terminal = result.conclusion == WatchConclusion::Terminal;
    let (observed_latest, status_exit) = result.item;
    let continuation = format!(
        "homeboy agent-task status {} --watch --interval {} --timeout {}",
        posix_quote_arg(&args.run_id),
        args.interval,
        args.timeout
    );
    let total_changes = progress
        .changes
        .len()
        .saturating_add(progress.omitted as usize);
    let (changes, budget) = budget_json_values(
        progress.changes,
        total_changes,
        OutputBudget {
            max_items: STATUS_WATCH_CHANGE_LIMIT,
            max_bytes: STATUS_WATCH_CHANGE_BYTE_LIMIT,
            max_events: None,
            max_seconds: None,
        },
        continuation.clone(),
        format!(
            "homeboy agent-task status {} --output <path>",
            posix_quote_arg(&args.run_id)
        ),
    );
    let latest = status_watch_latest(&observed_latest);
    let terminal_summary =
        terminal.then(|| status_watch_terminal_summary(&observed_latest, &args.run_id));
    let mut output = json!({
        "schema": "homeboy/agent-task-status-watch/v2",
        "command": "agent-task.status.watch",
        "run_id": args.run_id,
        "terminal": terminal,
        "timed_out": timed_out,
        "poll_count": result.poll_count,
        "waited_secs": result.waited.as_secs(),
        "changes": changes,
        "changes_omitted": budget.omitted_items,
        "latest": latest,
        "terminal_summary": terminal_summary,
        "continuation_command": continuation,
        "output_budget": budget,
    });
    enforce_watch_output_budget(&mut output, &continuation);
    let exit_code = if timed_out {
        TIMEOUT_EXIT_CODE
    } else if status_is_failure(&output["latest"]) {
        1
    } else {
        status_exit
    };
    (output, exit_code)
}

pub(super) fn status_watch_latest(snapshot: &Value) -> Value {
    snapshot.clone()
}

/// Keep the final stdout object bounded as a whole, not merely its changes.
pub(super) fn enforce_watch_output_budget(output: &mut Value, continuation: &str) {
    while serialized_len(output) > STATUS_WATCH_BYTE_LIMIT {
        let removed_change = output
            .get_mut("changes")
            .and_then(Value::as_array_mut)
            .and_then(Vec::pop)
            .is_some();
        if !removed_change {
            if output["terminal_summary"].is_object()
                && output["terminal_summary"]["schema"]
                    != "homeboy/agent-task-status-watch-terminal-ref/v2"
            {
                output["terminal_summary"] = json!({
                    "schema": "homeboy/agent-task-status-watch-terminal-ref/v2",
                    "run_id": output["run_id"],
                    "state": output["latest"]["state"],
                    "command": format!("homeboy agent-task status {}", posix_quote_arg(output["run_id"].as_str().unwrap_or_default())),
                });
                continue;
            }
            if output["latest"].is_object()
                && output["latest"]["schema"] != "homeboy/agent-task-status-watch-latest-ref/v2"
            {
                output["latest"] = json!({
                    "schema": "homeboy/agent-task-status-watch-latest-ref/v2",
                    "run_id": output["run_id"],
                    "state": output["latest"]["state"],
                    "command": format!("homeboy agent-task status {}", posix_quote_arg(output["run_id"].as_str().unwrap_or_default())),
                });
                continue;
            }
            break;
        }
        let omitted = output["changes_omitted"].as_u64().unwrap_or_default() + 1;
        output["changes_omitted"] = json!(omitted);
        output["output_budget"]["truncated"] = json!(true);
        output["output_budget"]["continuation_command"] = json!(continuation);
    }
}

pub(super) fn status_watch_terminal_summary(snapshot: &Value, run_id: &str) -> Value {
    json!({
        "schema": "homeboy/agent-task-status-watch-terminal/v1",
        "run_id": status_run_id(snapshot).unwrap_or(run_id),
        "state": status_run_state(snapshot),
        "phase": snapshot.get("phase"),
        "blocker": snapshot.get("blocker"),
        "candidate": snapshot.get("candidate"),
        "gates": snapshot.get("gates"),
        "publication": snapshot.get("publication"),
        "action_eligibility": snapshot.get("action_eligibility"),
    })
}

pub(super) fn status_run_id(status: &Value) -> Option<&str> {
    status
        .get("run")
        .or_else(|| status.get("run_id"))
        .and_then(Value::as_str)
}

pub(super) fn status_run_state(status: &Value) -> Option<&Value> {
    status.get("state")
}

pub(crate) fn artifacts(mut args: LifecycleReadArgs) -> CmdResult<Value> {
    let target = resolve_cook_reader_target(&args.run_id, false)?;
    args.run_id = target.run_id;
    if let Some(artifact_id) = args.show.as_deref() {
        return show_artifact(&args.run_id, artifact_id, args.tail);
    }
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let mut value = serde_json::to_value(artifacts).unwrap_or(Value::Null);
    if !args.full {
        attach_collection_budget(
            &mut value,
            "artifacts",
            &format!(
                "homeboy runs artifacts {} --full",
                posix_quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy runs artifacts {} --full --output <path>",
                posix_quote_arg(&args.run_id)
            ),
        );
    }
    value["source"] = json!("agent_task_lifecycle");
    if let Some(selection) = target.selection {
        value["candidate_selection"] = json!(selection);
    }
    preserve_controller_owner_placement(&mut value, &args.run_id);
    Ok((value, 0))
}

pub(super) fn show_artifact(
    run_id: &str,
    artifact_id: &str,
    tail: Option<usize>,
) -> CmdResult<Value> {
    use std::io::Read;

    let target = resolve_cook_reader_target(run_id, false)?;
    let run_id = target.run_id.as_str();
    let snapshot = agent_task_lifecycle::durable_local_read(run_id)?;
    let aggregate = snapshot.aggregate.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "artifact_id",
            format!("run `{run_id}` has no aggregate artifacts"),
            Some(artifact_id.to_string()),
            None,
        )
    })?;
    let selected = aggregate
        .outcomes
        .iter()
        .find_map(|outcome| {
            outcome
                .artifacts
                .iter()
                .find(|artifact| artifact.id == artifact_id)
                .map(|artifact| (outcome.task_id.as_str(), artifact))
        })
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact_id",
                format!("artifact `{artifact_id}` was not found in run `{run_id}`"),
                Some(artifact_id.to_string()),
                None,
            )
        })?;
    let artifact = selected.1;
    let path = artifact
        .path
        .as_deref()
        .filter(|path| std::path::Path::new(path).is_file());
    let temporary;
    let source_path = if let Some(path) = path {
        std::path::PathBuf::from(path)
    } else {
        let runner_id = snapshot.record.runner_id().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "artifact_id",
                "runner artifact has no authenticated runner binding",
                Some(artifact_id.to_string()),
                None,
            )
        })?;
        let reference = homeboy::agents::agent_task_lifecycle::canonical_runner_artifact_ref(
            runner_id,
            run_id,
            selected.0,
            artifact_id,
        );
        temporary = tempfile::NamedTempFile::new().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("create artifact download target".to_string()),
            )
        })?;
        let download =
            homeboy::core::observation::runs_service::with_runner_evidence(|provider| {
                provider.download_remote_artifact(&reference, Some(temporary.path().to_path_buf()))
            })?;
        download.output_path
    };
    let mut content = String::new();
    std::fs::File::open(&source_path)
        .and_then(|mut file| file.read_to_string(&mut content))
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(format!("read artifact `{artifact_id}`")),
            )
        })?;
    if let Some(lines) = tail {
        content = tail_lines(&content, lines);
    }
    Ok((
        json!({"schema":"homeboy/agent-task-artifact-content/v1", "source":"agent_task_lifecycle", "run_id":run_id, "artifact_id":artifact_id, "content":content}),
        0,
    ))
}

pub(super) fn tail_lines(content: &str, count: usize) -> String {
    content
        .lines()
        .rev()
        .take(count)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn evidence(args: EvidenceArgs) -> CmdResult<Value> {
    let target = resolve_cook_reader_target(&args.run_id, false)?;
    let run_id = &target.run_id;
    let durable_read = agent_task_lifecycle::durable_local_read(run_id)?;
    let artifacts = agent_task_service::artifacts(run_id)?;
    let aggregate = durable_read.aggregate;
    let failed_tasks = failed_task_statuses(aggregate.as_ref());
    let plan = agent_task_lifecycle::load_plan(run_id).ok();

    let gate_outputs =
        agent_task_service_direct::recorded_promotion_report(&durable_read.record.metadata)
            .ok()
            .flatten()
            .and_then(|report| report.get("deterministic_gates").cloned())
            .and_then(|gates| gates.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(homeboy::agents::agent_task_service::gate_failure_excerpt_from_value)
            .collect::<Vec<_>>();
    let mut hydrated = Vec::new();
    let mut total = 0;
    for (evidence_ref, task_id) in
        evidence_refs_with_tasks(&artifacts.evidence_refs, aggregate.as_ref())
    {
        if args
            .kind
            .as_deref()
            .is_some_and(|kind| evidence_ref.kind != kind)
        {
            continue;
        }
        if args
            .task
            .as_deref()
            .is_some_and(|task| task_id.as_deref() != Some(task))
        {
            continue;
        }
        if args.failure_only
            && !task_id
                .as_deref()
                .is_some_and(|task| failed_tasks.contains_key(task))
        {
            continue;
        }

        total += 1;
        // Count filtered refs without hydrating their payload once the shared
        // collection budget is full.
        if !args.full && hydrated.len() >= OutputBudget::COLLECTION.max_items {
            continue;
        }
        hydrated.push(agent_task_service::hydrate_evidence_ref(
            run_id,
            &evidence_ref,
            task_id.as_deref(),
            plan.as_ref(),
            aggregate.as_ref(),
        ));
    }

    let mut value = serde_json::to_value(AgentTaskEvidenceReport {
        schema: "homeboy/agent-task-evidence/v1",
        run_id: run_id.clone(),
        filters: AgentTaskEvidenceFilters {
            kind: args.kind,
            task: args.task,
            failure_only: args.failure_only,
        },
        count: total,
        evidence_total: total,
        evidence: hydrated,
    })
    .unwrap_or(Value::Null);
    if !gate_outputs.is_empty() {
        value["gate_failure_outputs"] = json!(gate_outputs);
    }
    if let Some(selection) = target.selection {
        value["candidate_selection"] = json!(selection);
    }
    value["source"] = json!("agent_task_lifecycle");
    attach_durable_read_availability(&mut value, &durable_read.unavailable_sources);
    if !args.full {
        attach_collection_budget(
            &mut value,
            "evidence",
            &format!(
                "homeboy runs evidence {} --full",
                posix_quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy runs evidence {} --full --output <path>",
                posix_quote_arg(&args.run_id)
            ),
        );
    }
    preserve_controller_owner_placement(&mut value, run_id);
    Ok((value, 0))
}

pub(in crate::commands::agent_task) fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let audit_store = homeboy::core::observation::ObservationStore::open_initialized()?;
    let resolved_id = agent_task_service::resolve_cook_continuation_run_id(&args.run_id)
        .unwrap_or_else(|_| args.run_id.clone());
    let audit_run = homeboy_control_plane_contract::RunId::new(resolved_id)
        .map_err(|e| homeboy::core::Error::internal_unexpected(e.to_string()))?;
    let already_cancelled = audit_store
        .durable_control_plane_event(&audit_run, "run.cancelled")?
        .is_some();
    let idempotency_key = args
        .idempotency_key
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let acknowledgement = homeboy::agents::orchestration::execute_action_from_current_environment(
        &args.run_id,
        &homeboy_control_plane_contract::ControlPlaneActionRequest {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: homeboy_control_plane_contract::ControlPlaneAction::Cancel,
            effect_id: homeboy_control_plane_contract::action_effect_id(
                "cli",
                &args.run_id,
                "cancel",
                &idempotency_key,
            ),
            idempotency_key,
            actor: "homeboy-cli".to_string(),
            expected_updated_at: None,
            parameters: homeboy_control_plane_contract::ControlPlaneActionPayload {
                schema: homeboy_control_plane_contract::CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA
                    .to_string(),
                data: json!({ "reason": args.reason }),
            },
            confirmed: true,
        },
    )?;
    let exit_code = cancel_exit_code(acknowledgement.outcome, &acknowledgement.result)?;
    if !already_cancelled
        && acknowledgement.outcome
            != homeboy_control_plane_contract::ControlPlaneActionOutcome::AlreadySatisfied
        && matches!(
            acknowledgement.result.data["disposition"].as_str(),
            Some("cancelled" | "requested")
        )
    {
        homeboy::core::observation::store::record_intervention(
            &audit_store,
            acknowledgement.run.as_str(),
            "agent-task cancel",
            args.reason.as_deref(),
        )?;
    }
    Ok((
        serde_json::to_value(acknowledgement).unwrap_or(Value::Null),
        exit_code,
    ))
}

pub(super) fn cancel_exit_code(
    outcome: homeboy_control_plane_contract::ControlPlaneActionOutcome,
    payload: &homeboy_control_plane_contract::ControlPlaneActionPayload,
) -> homeboy::core::Result<i32> {
    if outcome == homeboy_control_plane_contract::ControlPlaneActionOutcome::Failed {
        return Ok(1);
    }
    let result: homeboy_control_plane_contract::ControlPlaneCancelResult =
        serde_json::from_value(payload.data.clone()).map_err(|error| {
            homeboy::core::Error::internal_unexpected(format!(
                "canonical cancellation result: {error}"
            ))
        })?;
    Ok((result.disposition
        == homeboy_control_plane_contract::ControlPlaneCancelDisposition::Requested)
        .then_some(TIMEOUT_EXIT_CODE)
        .unwrap_or(0))
}

#[derive(Serialize)]
pub(super) struct AgentTaskEvidenceReport {
    schema: &'static str,
    run_id: String,
    filters: AgentTaskEvidenceFilters,
    count: usize,
    evidence_total: usize,
    evidence: Vec<agent_task_service::AgentTaskHydratedEvidence>,
}

#[derive(Serialize)]
pub(super) struct AgentTaskEvidenceFilters {
    kind: Option<String>,
    task: Option<String>,
    failure_only: bool,
}

pub(super) fn failed_task_statuses(
    aggregate: Option<&AgentTaskAggregate>,
) -> HashMap<String, AgentTaskOutcomeStatus> {
    aggregate
        .into_iter()
        .flat_map(|aggregate| aggregate.outcomes.iter())
        .filter(|outcome| {
            matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Failed
                    | AgentTaskOutcomeStatus::ProviderError
                    | AgentTaskOutcomeStatus::Timeout
                    | AgentTaskOutcomeStatus::UnableToRemediate
            )
        })
        .map(|outcome| (outcome.task_id.clone(), outcome.status))
        .collect()
}

pub(super) fn evidence_refs_with_tasks(
    refs: &[AgentTaskEvidenceRef],
    aggregate: Option<&AgentTaskAggregate>,
) -> Vec<(AgentTaskEvidenceRef, Option<String>)> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    if let Some(aggregate) = aggregate {
        for outcome in &aggregate.outcomes {
            for evidence_ref in &outcome.evidence_refs {
                if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
                    entries.push((evidence_ref.clone(), Some(outcome.task_id.clone())));
                }
            }
            if let Some(workflow) = &outcome.workflow {
                for step in &workflow.steps {
                    for evidence_ref in &step.artifact_refs {
                        if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
                            entries.push((evidence_ref.clone(), Some(outcome.task_id.clone())));
                        }
                    }
                }
            }
        }
    }
    for evidence_ref in refs {
        if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
            entries.push((
                evidence_ref.clone(),
                agent_task_service::evidence_ref_task_id(evidence_ref),
            ));
        }
    }
    entries
}
