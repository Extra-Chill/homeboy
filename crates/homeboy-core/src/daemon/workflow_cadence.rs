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
//!   records the slot as pending; after a crash the restarted daemon finds
//!   the instance in the job ledger and completes the slot instead of
//!   admitting it again.
//!   Every registration write is a compare-and-set on the due time.
//!
//! - **Watch.** A definition with `watch` instead records its refs' last
//!   observed values on the registration. Each tick re-observes them; a
//!   change starts the debounce, and once it elapses one instance named by
//!   the change is admitted, unless the previous one is still running.
//!
//! Registrations outlive any one daemon process, so they live in the
//! observation store beside the daemon's job ledger. Every instance is
//! admitted into and looked up in that one store, so overlap holds across
//! daemon restarts.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::workflow_job::WorkflowJobRequest;
use super::JobStore;
use crate::error::{Error, Result};
use crate::workflow::{WatchRef, WorkflowDefinition};

pub const CADENCE_SCHEMA: &str = "homeboy/workflow-cadence/v1";
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
    /// Set when a finished instance failed and cleared by the next success,
    /// so `status` can see a failing cadence in the local store (#17234).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<CadenceFailure>,
    /// Durable observations of a `watch` registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchState>,
}

/// What a `watch` registration last saw. While a change is waiting out its
/// debounce, `next_due_ms` is when it may start an instance; otherwise 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchState {
    pub debounce_ms: u64,
    /// Last observed value of each ref, by [`WatchRef::key`].
    pub observed: BTreeMap<String, String>,
    /// When the change not yet handled by an instance was last observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at_ms: Option<u64>,
    /// When the refs were last observed; they are polled at most every
    /// [`WATCH_POLL_MS`], not on every orchestration tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polled_at_ms: Option<u64>,
}

/// How often a watch re-observes its refs. Remote refs cost a network round
/// trip, so the orchestration tick must not pay one every pass.
pub const WATCH_POLL_MS: u64 = 60_000;
/// Bound on one ref observation, so an unreachable remote cannot stall the
/// orchestration tick.
const WATCH_OBSERVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The run of failed instances a cadence is currently in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CadenceFailure {
    /// Slot of the first failed instance in the run.
    pub since_slot_ms: u64,
    /// The most recent failed instance and its reason.
    pub instance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingSlot {
    pub slot_ms: u64,
    pub instance_name: String,
    pub instance: String,
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
    instance_name("every", slot_ms, chrono::SecondsFormat::Secs)
}

/// `watch-<change as UTC RFC 3339>`: the instance name of one observed change.
pub fn change_instance_name(changed_at_ms: u64) -> String {
    instance_name("watch", changed_at_ms, chrono::SecondsFormat::Millis)
}

fn instance_name(prefix: &str, at_ms: u64, format: chrono::SecondsFormat) -> String {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(format, true);
    format!("{prefix}-{at}")
}

/// The current value of every ref `template` watches, resolved from its
/// admitting directory. A ref that could not be observed is left out and its
/// error returned, so it is never mistaken for a change.
fn observe_refs(template: &WorkflowJobRequest) -> (BTreeMap<String, String>, Option<String>) {
    let mut observed = BTreeMap::new();
    let mut errors = Vec::new();
    let refs = template
        .definition
        .watch
        .iter()
        .flat_map(|watch| &watch.refs);
    for watched in refs {
        match observe_ref(watched, Path::new(&template.cwd)) {
            Ok(value) => {
                observed.insert(watched.key(), value);
            }
            Err(error) => errors.push(format!("{}: {error}", watched.key())),
        }
    }
    (observed, (!errors.is_empty()).then(|| errors.join("; ")))
}

/// A branch head SHA, the latest version-sorted tag, or a file's SHA-256.
/// An absent branch, tag, or file is the empty value.
fn observe_ref(watched: &WatchRef, cwd: &Path) -> std::result::Result<String, String> {
    let ls_remote = |args: &[&str]| -> std::result::Result<String, String> {
        let args = [&["ls-remote"], args].concat();
        let output = crate::git::run_git_with_env_timeout(
            cwd,
            &args,
            "git ls-remote for workflow watch",
            &[("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())],
            WATCH_OBSERVE_TIMEOUT,
        )
        .map_err(|error| error.message)?;
        Ok(output.lines().next().unwrap_or_default().trim().to_string())
    };
    match watched {
        WatchRef::Branch(target) => {
            let (repo, branch) = target.rsplit_once('@').unwrap_or((target, ""));
            let line = ls_remote(&[repo, &format!("refs/heads/{branch}")])?;
            Ok(line
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string())
        }
        WatchRef::Release(repo) => ls_remote(&["--tags", "--refs", "--sort=-v:refname", repo]),
        WatchRef::File(path) => match std::fs::read(cwd.join(path)) {
            Ok(bytes) => {
                use sha2::Digest;
                Ok(format!("{:x}", sha2::Sha256::digest(bytes)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error.to_string()),
        },
    }
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

/// Registrations are durable daemon state, so they are written like the job
/// ledger: through the observation store, fsynced.
fn open() -> Result<Connection> {
    crate::api_jobs::persistence::open_ledger(&crate::paths::observation_db()?)
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
    list_in(&open()?)
}

/// Active registrations whose last finished instance failed, read without
/// initializing or writing the ledger.
pub fn failing() -> Result<Vec<Cadence>> {
    let path = crate::paths::observation_db()?;
    let Some(connection) = crate::api_jobs::persistence::open_ledger_readonly(&path)? else {
        return Ok(Vec::new());
    };
    Ok(list_in(&connection)?
        .into_iter()
        .filter(|cadence| cadence.cancelled_at_ms.is_none() && cadence.failing.is_some())
        .collect())
}

fn list_in(connection: &Connection) -> Result<Vec<Cadence>> {
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
    let watch = template
        .definition
        .watch
        .as_ref()
        .map(|watch| -> Result<WatchState> {
            Ok(WatchState {
                debounce_ms: watch.debounce()?.as_millis() as u64,
                observed: observe_refs(&template).0,
                changed_at_ms: None,
                polled_at_ms: Some(now_ms),
            })
        })
        .transpose()?;
    let (anchor_ms, interval_ms) = match watch {
        Some(_) => (now_ms, 0),
        None => grid(&template.definition, now_ms)?,
    };
    // A watch is due only once a change has waited out its debounce.
    let initial_due = |builtin: bool| match first_due(anchor_ms, interval_ms, now_ms) {
        _ if watch.is_some() => 0,
        // An operator's cadence fires on its first slot. A built-in is
        // installed by a starting daemon and waits one interval, so daemon
        // start never doubles as a maintenance run.
        due if builtin && due <= now_ms => due + interval_ms,
        due => due,
    };
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
                && existing.request.definition.watch == template.definition.watch
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
                existing.next_due_ms = initial_due(false);
                existing.cancelled_at_ms = None;
                existing.watch = watch;
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
                next_due_ms: initial_due(builtin),
                cancelled_at_ms: None,
                last_instance: None,
                pending: None,
                last_fired_at_ms: None,
                fired: 0,
                skipped: 0,
                last_skipped_at_ms: None,
                last_error: None,
                failing: None,
                watch,
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
///
/// `update` runs while this connection holds the SQLite writer. It must be a
/// pure edit of the registration: no job-store access, child waits or other
/// I/O, because every job-store writer waits for the same SQLite writer.
fn advance(
    workflow: &str,
    expected_next_due_ms: u64,
    update: impl FnOnce(&mut Cadence),
) -> Result<bool> {
    let mut connection = open()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sql(error, "begin"))?;
    #[cfg(test)]
    tests::run_write_held_hook();
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
        .filter(|cadence| cadence.cancelled_at_ms.is_none())
        .inspect(|cadence| record_outcome(store, cadence))
        .filter_map(|cadence| match cadence.watch {
            Some(_) => admit_watched(store, cadence, now_ms).ok(),
            None if now_ms >= cadence.next_due_ms => admit_one(store, cadence, now_ms).ok(),
            None => None,
        })
        .filter(|admitted| !admitted.is_null())
        .collect()
}

/// Record the last instance's failure on the registration once it finishes,
/// or clear it on success. Best effort: a lost race retries next tick.
fn record_outcome(store: &JobStore, cadence: &Cadence) {
    let Some(instance) = cadence.last_instance.as_deref() else {
        return;
    };
    if cadence
        .failing
        .as_ref()
        .is_some_and(|failure| failure.instance == instance)
    {
        return;
    }
    let Some(job) = store
        .controller_job_for_key(instance)
        .filter(|job| job.status.is_terminal())
    else {
        return;
    };
    let Ok(view) = super::workflow_job::workflow_status(store, instance) else {
        return;
    };
    let failing = match view["status"].as_str() {
        Some("succeeded") if cadence.failing.is_some() => None,
        Some("failed" | "cancelled" | "budget_exhausted") => Some(CadenceFailure {
            since_slot_ms: cadence.failing.as_ref().map_or(
                cadence.last_fired_at_ms.unwrap_or(job.created_at_ms),
                |failure| failure.since_slot_ms,
            ),
            instance: instance.to_string(),
            reason: view["reason"].as_str().map(str::to_string),
        }),
        _ => return,
    };
    let _ = advance(&cadence.workflow, cadence.next_due_ms, |cadence| {
        cadence.failing = failing
    });
}

fn admit_one(store: &JobStore, cadence: Cadence, now_ms: u64) -> Result<Value> {
    let due = cadence.next_due_ms;
    let workflow = cadence.workflow.clone();
    if let Some(pending) = &cadence.pending {
        return complete_pending(store, &cadence, pending);
    }
    let previous_active = match cadence
        .last_instance
        .as_deref()
        .map(|instance| instance_activity(store, instance))
        .transpose()
    {
        Ok(activity) => activity.flatten() == Some(true),
        Err(error) => {
            advance(&workflow, due, |cadence| {
                cadence.last_error = Some(error.message.clone())
            })?;
            return Err(error);
        }
    };
    match decide(&cadence, now_ms, previous_active) {
        Decision::NotDue => Ok(Value::Null),
        Decision::Skip {
            slot_ms,
            next_due_ms,
        } => {
            // Read the job store before the registration's write transaction:
            // a job-store writer holds the store's lock while it waits for the
            // SQLite writer, so reading the store inside the transaction
            // deadlocks both until the busy budget runs out (#17645).
            let stalled = cadence
                .last_instance
                .as_deref()
                .and_then(|instance| store.controller_job_for_key(instance))
                .zip(cadence.last_skipped_at_ms)
                .is_some_and(|(job, last)| job.updated_at_ms <= last);
            advance(&workflow, due, |cadence| {
                if stalled {
                    cadence.last_error = Some(
                        "previous instance has made no progress since the last skipped slot".into(),
                    );
                }
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
        } => fire(store, &cadence, slot_ms, instance_name, next_due_ms),
    }
}

/// A crash left this slot pending: complete it where it was admitted.
fn complete_pending(store: &JobStore, cadence: &Cadence, pending: &PendingSlot) -> Result<Value> {
    let admitted = if instance_activity(store, &pending.instance)?.is_some() {
        Ok("existing")
    } else {
        admit_slot(
            store,
            &slot_request(&cadence.request, &pending.instance_name),
        )
    };
    finish(&cadence.workflow, cadence.next_due_ms, pending, &admitted)?;
    Ok(fired(&cadence.workflow, &pending.instance, &admitted))
}

/// Record the slot as pending, then admit its instance.
fn fire(
    store: &JobStore,
    cadence: &Cadence,
    slot_ms: u64,
    instance_name: String,
    next_due_ms: u64,
) -> Result<Value> {
    let request = slot_request(&cadence.request, &instance_name);
    let pending = PendingSlot {
        slot_ms,
        instance_name,
        instance: request.instance.clone(),
        next_due_ms,
    };
    let (workflow, due) = (&cadence.workflow, cadence.next_due_ms);
    if !advance(workflow, due, |cadence| {
        cadence.pending = Some(pending.clone())
    })? {
        return Ok(Value::Null);
    }
    let admitted = admit_slot(store, &request);
    finish(workflow, due, &pending, &admitted)?;
    Ok(fired(workflow, &pending.instance, &admitted))
}

/// Re-observe a watch registration's refs. A change is recorded and restarts
/// the debounce; once the debounce elapses with no instance of this watch
/// running, one instance named by the change is admitted.
fn admit_watched(store: &JobStore, cadence: Cadence, now_ms: u64) -> Result<Value> {
    if let Some(pending) = &cadence.pending {
        return complete_pending(store, &cadence, pending);
    }
    let Some(watch) = &cadence.watch else {
        return Ok(Value::Null);
    };
    let poll_due = watch
        .polled_at_ms
        .is_none_or(|polled| now_ms >= polled + WATCH_POLL_MS);
    if poll_due {
        let (observed, error) = observe_refs(&cadence.request);
        let changed = observed
            .iter()
            .filter(|(key, value)| watch.observed.get(*key) != Some(*value))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let debounce_ms = watch.debounce_ms;
        advance(&cadence.workflow, cadence.next_due_ms, |cadence| {
            if let Some(error) = error {
                cadence.last_error = Some(error);
            }
            if !changed.is_empty() {
                cadence.next_due_ms = now_ms + debounce_ms;
            }
            if let Some(watch) = cadence.watch.as_mut() {
                if !changed.is_empty() {
                    watch.changed_at_ms = Some(now_ms);
                }
                watch.polled_at_ms = Some(now_ms);
                watch.observed.extend(observed);
            }
        })?;
        if !changed.is_empty() {
            return Ok(json!({ "workflow": cadence.workflow, "changed": changed }));
        }
    }
    let Some(changed_at_ms) = watch.changed_at_ms else {
        return Ok(Value::Null);
    };
    if now_ms < cadence.next_due_ms {
        return Ok(Value::Null);
    }
    // One instance per watch at a time: the change waits for it to finish.
    if let Some(previous) = cadence.last_instance.as_deref() {
        if instance_activity(store, previous)? == Some(true) {
            return Ok(Value::Null);
        }
    }
    fire(
        store,
        &cadence,
        changed_at_ms,
        change_instance_name(changed_at_ms),
        0,
    )
}

type Admission = Result<&'static str>;

/// Record a pending slot's admission (or its error) and move to the next slot.
fn finish(workflow: &str, due: u64, pending: &PendingSlot, admitted: &Admission) -> Result<bool> {
    advance(workflow, due, |cadence| {
        cadence.pending = None;
        cadence.next_due_ms = pending.next_due_ms;
        if let Some(watch) = cadence.watch.as_mut() {
            watch.changed_at_ms = None;
        }
        match admitted {
            Ok(_) => {
                cadence.fired += 1;
                cadence.last_fired_at_ms = Some(pending.slot_ms);
                cadence.last_instance = Some(pending.instance.clone());
                cadence.last_error = None;
            }
            Err(error) => cadence.last_error = Some(error.message.clone()),
        }
    })
}

fn fired(workflow: &str, instance: &str, admitted: &Admission) -> Value {
    match admitted {
        Ok(how) => json!({ "workflow": workflow, "instance": instance, "admitted": how }),
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
    Ok(admission.disposition.as_str())
}

/// Whether `instance` is running (`Some(true)`), terminal (`Some(false)`), or
/// unknown (`None`) in the daemon's one job store.
fn instance_activity(store: &JobStore, instance: &str) -> Result<Option<bool>> {
    store.controller_instance_activity(instance)
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
        "watch": cadence.request.definition.watch,
        "observed": cadence.watch.as_ref().map(|watch| &watch.observed),
        "changed_at": at(cadence.watch.as_ref().and_then(|watch| watch.changed_at_ms)),
        "status": if cadence.cancelled_at_ms.is_some() { "cancelled" } else { "active" },
        "builtin": cadence.builtin,
        "definition_digest": cadence.request.definition_digest,
        "inputs": cadence.request.inputs,
        "registered_at": at(Some(cadence.registered_at_ms)),
        "next_due_at": at(Some(cadence.next_due_ms))
            .filter(|_| cadence.cancelled_at_ms.is_none() && cadence.next_due_ms > 0),
        "cancelled_at": at(cadence.cancelled_at_ms),
        "fired": cadence.fired,
        "last_fired_slot": at(cadence.last_fired_at_ms),
        "last_instance": cadence.last_instance,
        "skipped": cadence.skipped,
        "last_skipped_slot": at(cadence.last_skipped_at_ms),
        "last_error": cadence.last_error,
        "failing": cadence.failing.as_ref().map(|failure| json!({
            "since_slot": at(Some(failure.since_slot_ms)),
            "instance": failure.instance,
            "reason": failure.reason,
        })),
    })
}

/// The last instance's terminal status and reason. `last_error` covers only
/// admission, so a cadence whose every instance fails still looked healthy
/// without this (#17234).
fn last_outcome(cadence: &Cadence, store: &JobStore) -> Value {
    let Some(instance) = cadence.last_instance.as_deref() else {
        return Value::Null;
    };
    super::workflow_job::workflow_status(store, instance).map_or(Value::Null, |status| {
        json!({ "instance": instance, "status": status["status"], "reason": status["reason"] })
    })
}

/// `GET /workflows/cadences`.
pub(crate) fn http_list(store: &JobStore) -> Result<Value> {
    let entities = list()?
        .iter()
        .map(|cadence| {
            let mut view = view(cadence);
            view["last_outcome"] = last_outcome(cadence, store);
            view
        })
        .collect::<Vec<_>>();
    Ok(json!({ "entities": entities }))
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

pub use homeboy_engine_primitives::now_ms;

fn to_value<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some("serialize workflow cadence".into()))
    })
}

fn sql(error: rusqlite::Error, operation: &str) -> Error {
    if crate::observation::store::is_transient_lock_error(&error) {
        return Error::observation_store_busy(
            crate::paths::observation_db()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            format!("{operation} workflow cadence store"),
            crate::observation::store::control_plane_write_wait().as_millis() as u64,
        );
    }
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
