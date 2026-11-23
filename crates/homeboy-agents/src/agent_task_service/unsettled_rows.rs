//! Settlement of agent-task observation rows left `running` after their typed
//! lifecycle record ended.
//!
//! A Cook's rows are owned by its driver: the lifecycle projection keeps them
//! `running` until the driver's terminal report settles them. A driver that
//! exits on an error path, dies, or never starts (a detached child that
//! persisted its recipe but never materialized its attempt) leaves those rows
//! `running` forever. Active discovery skips them because their lifecycle is
//! terminal, and the generic observation reconciler defers rows carrying a
//! lifecycle record to that lifecycle, so nothing else ever settles them.
//!
//! This module is the one owner of that gap. The explicit `agent-task
//! reconcile --all` sweep settles rows no controller has driven for longer
//! than the heartbeat window, and a Cook driver that exits without a
//! terminal report settles its own rows immediately.

use std::collections::{HashMap, HashSet};

use homeboy_core::observation::{
    ObservationStore, RunRecord, RunStatus, RUNNING_HEARTBEAT_STALE_MINUTES,
};
use homeboy_core::Result;
use homeboy_lifecycle_contract::RunExecutionState;
use serde_json::{json, Value};

use super::reconcile::RECONCILER_ACTOR;
use super::CookRecipeStore;
use crate::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunRecord};

/// The redirected Cook's attempt never wrote a lifecycle record, so the only
/// honest outcome is that the work is unknown.
pub const UNSETTLED_REDIRECTED_ATTEMPT_ABSENT: &str = "redirected_attempt_absent";
/// The redirected Cook's attempt settled, but its root never mirrored it.
pub const UNSETTLED_REDIRECTED_ROOT_UNMIRRORED: &str = "redirected_root_unmirrored";
/// A Cook attempt whose lifecycle ended without its driver settling the row.
pub const UNSETTLED_COOK_ATTEMPT: &str = "cook_attempt_unsettled";
/// Any other agent-task row whose lifecycle ended without a settled row.
pub const UNSETTLED_TERMINAL_LIFECYCLE: &str = "terminal_lifecycle_unsettled";

/// One row the sweep considered, with its class and proposed terminal status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskUnsettledRow {
    pub run_id: String,
    pub cook_id: String,
    pub class: &'static str,
    /// The typed lifecycle state recorded on the row.
    pub lifecycle_state: RunExecutionState,
    /// The terminal row status this row settles to.
    pub proposed_status: &'static str,
    /// `would-settle` (preview), `settled`, `no-op` (settled concurrently),
    /// or `failed`.
    pub action: &'static str,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct Candidate {
    row: RunRecord,
    record: AgentTaskRunRecord,
    cook_id: String,
}

struct Decision {
    class: &'static str,
    status: RunStatus,
    reason: String,
}

/// How a sweep decides and settles.
#[derive(Clone, Copy)]
struct SweepMode {
    /// Only settle rows whose lifecycle has been quiet for the heartbeat
    /// window, and allow the `stale` outcome for never-materialized attempts.
    require_quiet_window: bool,
    /// Historical outcomes were never announced when they happened; settling
    /// them now must not page anyone.
    suppress_completion_notification: bool,
    dry_run: bool,
}

/// Fleet sweep behind `agent-task reconcile --all`. Preview reports every row
/// it would settle; apply settles them.
pub(crate) fn sweep_unsettled_rows(
    lifecycle_store: &AgentTaskLifecycleStore,
    dry_run: bool,
) -> Result<Vec<AgentTaskUnsettledRow>> {
    sweep(
        lifecycle_store,
        None,
        SweepMode {
            require_quiet_window: true,
            suppress_completion_notification: true,
            dry_run,
        },
    )
}

/// Settle the rows of one Cook whose driver just exited without a terminal
/// report. Rows of a Cook that still has live lifecycle work, or that another
/// controller now drives, are left alone.
pub fn settle_unowned_cook_rows(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<Vec<AgentTaskUnsettledRow>> {
    sweep(
        lifecycle_store,
        Some(cook_id),
        SweepMode {
            require_quiet_window: false,
            suppress_completion_notification: false,
            dry_run: false,
        },
    )
}

fn sweep(
    lifecycle_store: &AgentTaskLifecycleStore,
    only_cook: Option<&str>,
    mode: SweepMode,
) -> Result<Vec<AgentTaskUnsettledRow>> {
    let observation = lifecycle_store.open_observation_initialized()?;
    let recipes = CookRecipeStore::new(lifecycle_store.roots().clone());
    let candidates: Vec<Candidate> = observation
        .list_active_runs()?
        .into_iter()
        .filter(|row| row.kind == "agent-task")
        .filter_map(|row| {
            let record = row
                .metadata_json
                .get("agent_task_run")
                .cloned()
                .and_then(|value| serde_json::from_value::<AgentTaskRunRecord>(value).ok())?;
            let cook_id = record_cook_id(&record);
            Some(Candidate {
                row,
                record,
                cook_id,
            })
        })
        .filter(|candidate| only_cook.is_none_or(|cook_id| candidate.cook_id == cook_id))
        .collect();

    // A Cook with any live lifecycle member is still in flight; its own
    // controller or the liveness reconciler owns it, not this sweep.
    let live_cooks: HashSet<String> = candidates
        .iter()
        .filter(|candidate| !candidate.record.state().is_terminal())
        .map(|candidate| candidate.cook_id.clone())
        .collect();
    let mut driven: HashMap<String, bool> = HashMap::new();
    let mut rows = Vec::new();

    for candidate in &candidates {
        let state = candidate.record.state();
        if !state.is_terminal() || live_cooks.contains(&candidate.cook_id) {
            continue;
        }
        if matches!(
            candidate.record.metadata["cook_continuation"]["state"].as_str(),
            Some("pending" | "running")
        ) {
            continue;
        }
        if mode.require_quiet_window && !outlived_heartbeat_window(&candidate.record) {
            continue;
        }
        let is_driven = match driven.get(&candidate.cook_id) {
            Some(held) => *held,
            None => {
                let held = recipes.cook_driver_held(&candidate.cook_id)?;
                driven.insert(candidate.cook_id.clone(), held);
                held
            }
        };
        if is_driven {
            continue;
        }
        let decision = match decide(&observation, candidate, mode) {
            Ok(Some(decision)) => decision,
            Ok(None) => continue,
            Err(error) => {
                rows.push(AgentTaskUnsettledRow {
                    run_id: candidate.row.id.clone(),
                    cook_id: candidate.cook_id.clone(),
                    class: UNSETTLED_TERMINAL_LIFECYCLE,
                    lifecycle_state: state,
                    proposed_status: RunStatus::Fail.as_str(),
                    action: "failed",
                    reason: "row classification failed".to_string(),
                    error: Some(error.message),
                });
                continue;
            }
        };
        let mut row = AgentTaskUnsettledRow {
            run_id: candidate.row.id.clone(),
            cook_id: candidate.cook_id.clone(),
            class: decision.class,
            lifecycle_state: state,
            proposed_status: decision.status.as_str(),
            action: "would-settle",
            reason: decision.reason.clone(),
            error: None,
        };
        if !mode.dry_run {
            let metadata = settled_metadata(&candidate.row, state, &decision, mode);
            match observation.finish_running_run(&candidate.row.id, decision.status, Some(metadata))
            {
                Ok(Some(_)) => row.action = "settled",
                Ok(None) => row.action = "no-op",
                Err(error) => {
                    row.action = "failed";
                    row.error = Some(error.message);
                }
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn record_cook_id(record: &AgentTaskRunRecord) -> String {
    record.metadata["cook_id"]
        .as_str()
        .filter(|cook_id| !cook_id.is_empty())
        .unwrap_or(&record.run_id)
        .to_string()
}

fn decide(
    observation: &ObservationStore,
    candidate: &Candidate,
    mode: SweepMode,
) -> Result<Option<Decision>> {
    let record = &candidate.record;
    let handoff = &record.metadata["detached_cook_handoff"];
    let redirected =
        handoff["cook_id"] == record.run_id.as_str() && handoff["state"] == "redirected";
    if !redirected {
        let (class, subject) = if candidate.cook_id != record.run_id {
            (UNSETTLED_COOK_ATTEMPT, "Cook attempt")
        } else {
            (UNSETTLED_TERMINAL_LIFECYCLE, "run")
        };
        return Ok(Some(Decision {
            class,
            status: terminal_row_status(record.state()),
            reason: format!(
                "the {subject}'s lifecycle ended `{}` but no live controller settled its row",
                state_label(record.state())
            ),
        }));
    }

    // A redirected root speaks for its attempt, never for its own handoff
    // bookkeeping: its `succeeded` only means the handoff completed.
    let Some(attempt_id) = handoff["attempt_run_id"]
        .as_str()
        .filter(|attempt_id| !attempt_id.is_empty())
    else {
        return Ok(mode.require_quiet_window.then(|| Decision {
            class: UNSETTLED_REDIRECTED_ATTEMPT_ABSENT,
            status: RunStatus::Stale,
            reason:
                "the detached Cook redirected without naming an attempt and no controller drives it"
                    .to_string(),
        }));
    };
    let Some(attempt) = observation.get_run(attempt_id)? else {
        return Ok(mode.require_quiet_window.then(|| Decision {
            class: UNSETTLED_REDIRECTED_ATTEMPT_ABSENT,
            status: RunStatus::Stale,
            reason: format!(
                "the detached Cook redirected to attempt `{attempt_id}`, which never materialized a lifecycle record, and no controller drives it"
            ),
        }));
    };
    if attempt.status != RunStatus::Running.as_str() {
        let status = RunStatus::from_label(&attempt.status).unwrap_or(RunStatus::Fail);
        return Ok(Some(Decision {
            class: UNSETTLED_REDIRECTED_ROOT_UNMIRRORED,
            status,
            reason: format!(
                "the Cook's attempt `{attempt_id}` settled `{}` but its redirected root never mirrored it",
                attempt.status
            ),
        }));
    }
    // The attempt row is itself unsettled; settle the root to the attempt's
    // lifecycle outcome, exactly as the attempt row will settle.
    let attempt_record = attempt
        .metadata_json
        .get("agent_task_run")
        .cloned()
        .and_then(|value| serde_json::from_value::<AgentTaskRunRecord>(value).ok());
    Ok(attempt_record
        .filter(|attempt| attempt.state().is_terminal())
        .map(|attempt| Decision {
            class: UNSETTLED_REDIRECTED_ROOT_UNMIRRORED,
            status: terminal_row_status(attempt.state()),
            reason: format!(
                "the Cook's attempt `{attempt_id}` ended `{}` but neither row was settled",
                state_label(attempt.state())
            ),
        }))
}

/// The row status a terminal lifecycle state settles to.
fn terminal_row_status(state: RunExecutionState) -> RunStatus {
    match state {
        RunExecutionState::Succeeded => RunStatus::Pass,
        RunExecutionState::Cancelled => RunStatus::Skipped,
        _ => RunStatus::Fail,
    }
}

fn state_label(state: RunExecutionState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{state:?}"))
}

/// Whether the record's last lifecycle write is older than the heartbeat
/// window, so a controller that is merely between writes is never mistaken
/// for a dead one. An unparseable timestamp is treated as fresh.
fn outlived_heartbeat_window(record: &AgentTaskRunRecord) -> bool {
    let stamp = record
        .updated_at
        .as_deref()
        .unwrap_or(record.submitted_at.as_str());
    chrono::DateTime::parse_from_rfc3339(stamp).is_ok_and(|at| {
        chrono::Utc::now()
            .signed_duration_since(at.with_timezone(&chrono::Utc))
            .num_minutes()
            >= RUNNING_HEARTBEAT_STALE_MINUTES
    })
}

fn settled_metadata(
    row: &RunRecord,
    state: RunExecutionState,
    decision: &Decision,
    mode: SweepMode,
) -> Value {
    let mut metadata = row.metadata_json.clone();
    if !metadata.is_object() {
        metadata = json!({});
    }
    let now = chrono::Utc::now().to_rfc3339();
    let object = metadata.as_object_mut().expect("object ensured above");
    object.insert(
        "homeboy_reconciled".to_string(),
        json!({
            "status": decision.status.as_str(),
            "reason": decision.class,
            "detail": decision.reason,
            "actor": RECONCILER_ACTOR,
            "lifecycle_state": state,
            "reconciled_at": now,
        }),
    );
    if mode.suppress_completion_notification && !object.contains_key("notification_delivered") {
        object.insert(
            "notification_delivered".to_string(),
            json!({
                "at": now,
                "by": "agent-task-reconcile",
                "suppressed": "historical outcome settled by reconciliation",
            }),
        );
    }
    metadata
}
