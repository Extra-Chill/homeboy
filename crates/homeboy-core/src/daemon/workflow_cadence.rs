//! Workflow cadence: `every` on a `homeboy/workflow/v1` definition.
//!
//! `workflow run` on a definition with `every` registers it here instead of
//! admitting one instance. A registration is the admitted request template
//! (definition, inputs, environment, programs) plus its slot grid. The
//! admitting daemon's orchestration tick admits one instance per due slot.
//!
//! - **Slots.** Slots are `anchor + k * interval`. The anchor is the `at`
//!   time of day (UTC), or the registration time when there is none. Missed
//!   slots coalesce into the latest one.
//! - **Overlap skip.** A slot that comes due while the previous instance of the
//!   same registration is non-terminal is skipped and counted.
//! - **Exactly once per slot.** A slot's instance name is derived from the
//!   slot, so its instance id is deterministic. Before admitting it the tick
//!   records the slot as pending, with the job store it is admitted into;
//!   after a crash the restarted daemon finds it in that same store and
//!   completes the slot instead of admitting it again.
//!   Every registration write is a compare-and-set on the due time.
//!
//! Registrations outlive any one daemon process, so they live in one SQLite
//! file beside the daemon's single job store. Every instance is admitted into
//! and looked up in that one store, so overlap holds across daemon restarts.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::workflow_job::WorkflowJobRequest;
use super::JobStore;
use crate::error::{Error, Result};
use crate::workflow::WorkflowDefinition;

pub const CADENCE_SCHEMA: &str = "homeboy/workflow-cadence/v1";
const STORE_FILE: &str = "workflow-cadences.sqlite";
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// One registered cadence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cadence {
    pub schema: String,
    pub workflow: String,
    /// The admitted request every slot's instance is built from.
    pub request: WorkflowJobRequest,
    /// Registered by the daemon from a built-in definition.
    pub builtin: bool,
    pub registered_at_ms: u64,
    pub anchor_ms: u64,
    pub interval_ms: u64,
    pub next_due_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_instance: Option<String>,
    /// The job store `last_instance` was admitted into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_instance_store: Option<PathBuf>,
    /// A slot whose instance is being admitted. Set before admission and
    /// cleared with the registration's advance, so a crash in between is
    /// completed, not re-admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at_ms: Option<u64>,
    pub fired: u64,
    pub skipped: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_skipped_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingSlot {
    pub slot_ms: u64,
    pub instance_name: String,
    pub instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<PathBuf>,
    pub next_due_ms: u64,
}

/// What the tick does with one registration at `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    NotDue,
    /// The previous instance is still running: count the slot and move on.
    Skip {
        slot_ms: u64,
        next_due_ms: u64,
    },
    /// Admit the slot's instance.
    Fire {
        slot_ms: u64,
        instance_name: String,
        next_due_ms: u64,
    },
}

/// The slot grid of `definition` registered at `now_ms`: (anchor, interval).
pub fn grid(definition: &WorkflowDefinition, now_ms: u64) -> Result<(u64, u64)> {
    let every = definition.every.as_ref().ok_or_else(|| {
        invalid(format!(
            "workflow `{}` has no `every` cadence",
            definition.id
        ))
    })?;
    let interval_ms = every.interval()?.as_millis() as u64;
    let anchor_ms = match every.at_offset()? {
        Some(offset) => offset.as_millis() as u64 % DAY_MS,
        None => now_ms,
    };
    Ok((anchor_ms, interval_ms))
}

/// The first slot at or after `now_ms`.
pub fn first_due(anchor_ms: u64, interval_ms: u64, now_ms: u64) -> u64 {
    if now_ms <= anchor_ms {
        return anchor_ms;
    }
    let elapsed = now_ms - anchor_ms;
    anchor_ms + elapsed.div_ceil(interval_ms) * interval_ms
}

/// The latest slot at or before `now_ms` (`now_ms >= anchor_ms`).
fn latest_slot(anchor_ms: u64, interval_ms: u64, now_ms: u64) -> u64 {
    anchor_ms + (now_ms.saturating_sub(anchor_ms) / interval_ms) * interval_ms
}

/// The pure tick decision. `previous_active` says whether the registration's
/// last instance is still non-terminal.
pub fn decide(cadence: &Cadence, now_ms: u64, previous_active: bool) -> Decision {
    if cadence.cancelled_at_ms.is_some() || now_ms < cadence.next_due_ms {
        return Decision::NotDue;
    }
    let slot_ms = latest_slot(cadence.anchor_ms, cadence.interval_ms, now_ms);
    let next_due_ms = slot_ms + cadence.interval_ms;
    if previous_active {
        return Decision::Skip {
            slot_ms,
            next_due_ms,
        };
    }
    Decision::Fire {
        slot_ms,
        instance_name: slot_instance_name(slot_ms),
        next_due_ms,
    }
}

/// `every-<slot as UTC RFC 3339>`: the instance name of one slot.
pub fn slot_instance_name(slot_ms: u64) -> String {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(slot_ms as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!("every-{at}")
}

/// The request for one slot: the template, named by the slot.
pub fn slot_request(template: &WorkflowJobRequest, instance_name: &str) -> WorkflowJobRequest {
    let mut request = template.clone();
    request.instance_name = Some(instance_name.to_string());
    request.instance = request
        .definition
        .instance_id(&request.inputs, Some(instance_name));
    request
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

fn store_path() -> Result<PathBuf> {
    Ok(crate::paths::daemon_state_file()?.with_file_name(STORE_FILE))
}

fn open() -> Result<Connection> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let connection = Connection::open(&path).map_err(|error| sql(error, "open"))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| sql(error, "configure"))?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS workflow_cadences (
                 workflow TEXT PRIMARY KEY NOT NULL,
                 next_due_ms INTEGER NOT NULL,
                 body TEXT NOT NULL
             );",
        )
        .map_err(|error| sql(error, "initialize"))?;
    Ok(connection)
}

fn read(connection: &Connection, workflow: &str) -> Result<Option<Cadence>> {
    connection
        .query_row(
            "SELECT body FROM workflow_cadences WHERE workflow = ?1",
            params![workflow],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| sql(error, "read"))?
        .map(|body| decode(&body))
        .transpose()
}

fn decode(body: &str) -> Result<Cadence> {
    serde_json::from_str(body).map_err(|error| {
        Error::internal_json(error.to_string(), Some("parse workflow cadence".into()))
    })
}

fn write(connection: &Connection, cadence: &Cadence) -> Result<()> {
    let body = serde_json::to_string(cadence).map_err(|error| {
        Error::internal_json(error.to_string(), Some("serialize workflow cadence".into()))
    })?;
    connection
        .execute(
            "INSERT INTO workflow_cadences (workflow, next_due_ms, body) VALUES (?1, ?2, ?3)
             ON CONFLICT (workflow) DO UPDATE SET next_due_ms = ?2, body = ?3",
            params![cadence.workflow, cadence.next_due_ms as i64, body],
        )
        .map_err(|error| sql(error, "write"))?;
    Ok(())
}

/// Every registration, ordered by workflow id.
pub fn list() -> Result<Vec<Cadence>> {
    let connection = open()?;
    let mut statement = connection
        .prepare("SELECT body FROM workflow_cadences ORDER BY workflow")
        .map_err(|error| sql(error, "list"))?;
    let bodies = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| sql(error, "list"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| sql(error, "list"))?;
    bodies.iter().map(|body| decode(body)).collect()
}

/// Register (or re-register) `template`'s workflow. Re-registering an
/// unchanged cadence keeps its slot grid and counters; a built-in never
/// replaces an operator's registration or opt-out.
pub fn register(
    template: WorkflowJobRequest,
    builtin: bool,
    now_ms: u64,
) -> Result<(Cadence, &'static str)> {
    let template = WorkflowJobRequest::parse(&to_value(&template)?)?;
    if template.instance_name.is_some() {
        return Err(invalid(
            "a cadence registers its workflow without `--instance`",
        ));
    }
    let (anchor_ms, interval_ms) = grid(&template.definition, now_ms)?;
    let mut connection = open()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sql(error, "begin"))?;
    let existing = read(&transaction, &template.definition.id)?;
    let (cadence, disposition) = match existing {
        Some(existing) if builtin && (!existing.builtin || existing.cancelled_at_ms.is_some()) => {
            return Ok((existing, "kept"));
        }
        Some(mut existing) => {
            let same_cadence = existing.request.definition.every == template.definition.every
                && existing.cancelled_at_ms.is_none();
            let disposition = if same_cadence && existing.request == template {
                "unchanged"
            } else {
                "updated"
            };
            if !same_cadence {
                existing.registered_at_ms = now_ms;
                existing.anchor_ms = anchor_ms;
                existing.interval_ms = interval_ms;
                existing.next_due_ms = first_due(anchor_ms, interval_ms, now_ms);
                existing.cancelled_at_ms = None;
            }
            existing.request = template;
            existing.builtin = builtin;
            (existing, disposition)
        }
        None => (
            Cadence {
                schema: CADENCE_SCHEMA.to_string(),
                workflow: template.definition.id.clone(),
                request: template,
                builtin,
                registered_at_ms: now_ms,
                anchor_ms,
                interval_ms,
                // An operator's cadence fires on its first slot. A built-in is
                // installed by a starting daemon and waits one interval, so
                // daemon start never doubles as a maintenance run.
                next_due_ms: match first_due(anchor_ms, interval_ms, now_ms) {
                    due if builtin && due <= now_ms => due + interval_ms,
                    due => due,
                },
                cancelled_at_ms: None,
                last_instance: None,
                last_instance_store: None,
                pending: None,
                last_fired_at_ms: None,
                fired: 0,
                skipped: 0,
                last_skipped_at_ms: None,
                last_error: None,
            },
            "registered",
        ),
    };
    if disposition != "unchanged" {
        write(&transaction, &cadence)?;
    }
    transaction.commit().map_err(|error| sql(error, "commit"))?;
    Ok((cadence, disposition))
}

/// Stop a registration from firing. The row is kept, so the opt-out of a
/// built-in survives daemon restarts; `workflow run` re-registers it.
pub fn cancel(workflow: &str, now_ms: u64) -> Result<Cadence> {
    let mut connection = open()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sql(error, "begin"))?;
    let mut cadence = read(&transaction, workflow)?.ok_or_else(|| not_found(workflow))?;
    if cadence.cancelled_at_ms.is_none() {
        cadence.cancelled_at_ms = Some(now_ms);
        write(&transaction, &cadence)?;
    }
    transaction.commit().map_err(|error| sql(error, "commit"))?;
    Ok(cadence)
}

/// Advance `workflow` only if it is still due at `expected_next_due_ms`, so
/// a slot is consumed once even if two ticks race.
fn advance(
    workflow: &str,
    expected_next_due_ms: u64,
    update: impl FnOnce(&mut Cadence),
) -> Result<bool> {
    let mut connection = open()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sql(error, "begin"))?;
    let Some(mut cadence) = read(&transaction, workflow)? else {
        return Ok(false);
    };
    if cadence.next_due_ms != expected_next_due_ms || cadence.cancelled_at_ms.is_some() {
        return Ok(false);
    }
    update(&mut cadence);
    write(&transaction, &cadence)?;
    transaction.commit().map_err(|error| sql(error, "commit"))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Tick
// ---------------------------------------------------------------------------

/// Admit every due registration's slot instance, or skip the slot when the
/// previous instance is still running. Called by the admission owner's
/// orchestration tick.
pub fn admit_due(store: &JobStore, now_ms: u64) -> Vec<Value> {
    let Ok(cadences) = list() else {
        return Vec::new();
    };
    cadences
        .into_iter()
        .filter(|cadence| cadence.cancelled_at_ms.is_none() && now_ms >= cadence.next_due_ms)
        .filter_map(|cadence| admit_one(store, cadence, now_ms).ok())
        .collect()
}

fn admit_one(store: &JobStore, cadence: Cadence, now_ms: u64) -> Result<Value> {
    let due = cadence.next_due_ms;
    let workflow = cadence.workflow.clone();
    // A crash left this slot pending: complete it where it was admitted.
    if let Some(pending) = cadence.pending.clone() {
        let admitted = if instance_activity(store, &pending.instance).is_some() {
            Ok((pending.store.clone(), "existing"))
        } else {
            admit_slot(
                store,
                &slot_request(&cadence.request, &pending.instance_name),
            )
        };
        finish(&workflow, due, &pending, &admitted)?;
        return Ok(fired(&workflow, &pending.instance, &admitted));
    }
    let previous_active = cadence
        .last_instance
        .as_deref()
        .is_some_and(|instance| instance_activity(store, instance) == Some(true));
    match decide(&cadence, now_ms, previous_active) {
        Decision::NotDue => Ok(Value::Null),
        Decision::Skip {
            slot_ms,
            next_due_ms,
        } => {
            advance(&workflow, due, |cadence| {
                cadence.skipped += 1;
                cadence.last_skipped_at_ms = Some(slot_ms);
                cadence.next_due_ms = next_due_ms;
            })?;
            Ok(json!({ "workflow": workflow, "skipped": slot_ms }))
        }
        Decision::Fire {
            slot_ms,
            instance_name,
            next_due_ms,
        } => {
            let request = slot_request(&cadence.request, &instance_name);
            let pending = PendingSlot {
                slot_ms,
                instance_name,
                instance: request.instance.clone(),
                store: store.persistence_path(),
                next_due_ms,
            };
            if !advance(&workflow, due, |cadence| {
                cadence.pending = Some(pending.clone())
            })? {
                return Ok(Value::Null);
            }
            let admitted = admit_slot(store, &request);
            finish(&workflow, due, &pending, &admitted)?;
            Ok(fired(&workflow, &pending.instance, &admitted))
        }
    }
}

type Admission = Result<(Option<PathBuf>, &'static str)>;

/// Record a pending slot's admission (or its error) and move to the next slot.
fn finish(workflow: &str, due: u64, pending: &PendingSlot, admitted: &Admission) -> Result<bool> {
    advance(workflow, due, |cadence| {
        cadence.pending = None;
        cadence.next_due_ms = pending.next_due_ms;
        match admitted {
            Ok((store, _)) => {
                cadence.fired += 1;
                cadence.last_fired_at_ms = Some(pending.slot_ms);
                cadence.last_instance = Some(pending.instance.clone());
                cadence.last_instance_store = store.clone();
                cadence.last_error = None;
            }
            Err(error) => cadence.last_error = Some(error.message.clone()),
        }
    })
}

fn fired(workflow: &str, instance: &str, admitted: &Admission) -> Value {
    match admitted {
        Ok((_, how)) => json!({ "workflow": workflow, "instance": instance, "admitted": how }),
        Err(error) => json!({ "workflow": workflow, "instance": instance, "error": error.message }),
    }
}

/// Admit and start one slot's instance in this daemon's store.
fn admit_slot(store: &JobStore, request: &WorkflowJobRequest) -> Admission {
    let envelope: super::ControllerJobRequest = serde_json::from_value(request.submission()?)
        .map_err(|error| {
            Error::internal_json(error.to_string(), Some("workflow submission".into()))
        })?;
    let admission =
        super::with_daemon_job_admission(|| super::admit_controller_job(envelope, store))?;
    super::start_controller_job(admission.job.id, store)?;
    Ok((store.persistence_path(), admission.disposition.as_str()))
}

/// Whether `instance` is running (`Some(true)`), terminal (`Some(false)`), or
/// unknown (`None`) in the daemon's one job store.
fn instance_activity(store: &JobStore, instance: &str) -> Option<bool> {
    store
        .controller_job_for_key(instance)
        .map(|job| !job.status.is_terminal())
}

// ---------------------------------------------------------------------------
// Built-ins
// ---------------------------------------------------------------------------

/// Builds the admitted request for a built-in definition in the daemon's own
/// environment. Registered by the CLI, which owns admission.
pub type BuiltinRequestFactory =
    Arc<dyn Fn(&WorkflowDefinition) -> Result<WorkflowJobRequest> + Send + Sync>;

fn builtin_factory() -> &'static RwLock<Option<BuiltinRequestFactory>> {
    static FACTORY: OnceLock<RwLock<Option<BuiltinRequestFactory>>> = OnceLock::new();
    FACTORY.get_or_init(|| RwLock::new(None))
}

pub fn register_builtin_request_factory(factory: BuiltinRequestFactory) {
    *builtin_factory()
        .write()
        .expect("workflow cadence factory lock") = Some(factory);
}

/// Register every built-in cadence the operator has not opted out of. Called
/// once when the daemon starts.
pub fn ensure_builtin_cadences(now_ms: u64) -> Result<Vec<Value>> {
    let Some(factory) = builtin_factory()
        .read()
        .expect("workflow cadence factory lock")
        .clone()
    else {
        return Ok(Vec::new());
    };
    let mut registered = Vec::new();
    for definition in crate::workflow::builtins() {
        if definition.every.is_none() {
            continue;
        }
        let (cadence, disposition) = register(factory(&definition)?, true, now_ms)?;
        registered.push(json!({ "workflow": cadence.workflow, "disposition": disposition }));
    }
    Ok(registered)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// The public view of a registration: no environment values.
pub fn view(cadence: &Cadence) -> Value {
    let at = |ms: Option<u64>| {
        ms.and_then(|ms| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64))
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    };
    json!({
        "workflow": cadence.workflow,
        "every": cadence.request.definition.every,
        "status": if cadence.cancelled_at_ms.is_some() { "cancelled" } else { "active" },
        "builtin": cadence.builtin,
        "definition_digest": cadence.request.definition_digest,
        "inputs": cadence.request.inputs,
        "registered_at": at(Some(cadence.registered_at_ms)),
        "next_due_at": at(Some(cadence.next_due_ms)).filter(|_| cadence.cancelled_at_ms.is_none()),
        "cancelled_at": at(cadence.cancelled_at_ms),
        "fired": cadence.fired,
        "last_fired_slot": at(cadence.last_fired_at_ms),
        "last_instance": cadence.last_instance,
        "skipped": cadence.skipped,
        "last_skipped_slot": at(cadence.last_skipped_at_ms),
        "last_error": cadence.last_error,
    })
}

/// `GET /workflows/cadences`.
pub(crate) fn http_list() -> Result<Value> {
    Ok(json!({ "entities": list()?.iter().map(view).collect::<Vec<_>>() }))
}

/// `POST /workflows/cadences`: the body is the admitted request template.
pub(crate) fn http_register(body: Option<Value>) -> Result<Value> {
    let template = WorkflowJobRequest::parse(&body.unwrap_or(Value::Null))?;
    let (cadence, disposition) = register(template, false, now_ms())?;
    let mut view = view(&cadence);
    view["disposition"] = json!(disposition);
    Ok(view)
}

/// `POST /workflows/cadences/<workflow>/cancel`.
pub(crate) fn http_cancel(workflow: &str) -> Result<Value> {
    Ok(view(&cancel(workflow, now_ms())?))
}

pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn to_value<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some("serialize workflow cadence".into()))
    })
}

fn sql(error: rusqlite::Error, operation: &str) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("{operation} workflow cadence store")),
    )
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("every", message, None, None)
}

fn not_found(workflow: &str) -> Error {
    Error::validation_invalid_argument(
        "workflow",
        format!("no workflow instance or cadence `{workflow}`"),
        Some(workflow.to_string()),
        None,
    )
}

#[cfg(all(test, unix))]
#[path = "workflow_cadence_tests.rs"]
mod tests;
