//! Durable notification outbox.
//!
//! [`crate::notify::dispatch`] spawns a transport child and returns whether it
//! succeeded. Every producer discarded that answer (`let _ = notify::dispatch`),
//! because a notification is observability and must never become a cook failure
//! mode. The cost of that invariant was that a transport outage lasting sixty
//! seconds silently destroyed the terminal outcome of a three-hour cook: the
//! event existed only on the stack of the process that produced it.
//!
//! This module keeps the invariant and removes the cost. Producers hand the
//! event to [`dispatch_with_outbox`], which attempts delivery inline exactly as
//! before and — when the attempt fails for a reason that could succeed later —
//! writes the event to a durable queue. The daemon's existing five-second
//! completion tick drains that queue with exponential backoff, and gives up
//! into a dead-letter bucket rather than retrying forever.
//!
//! # Enqueue is infallible from the caller's perspective
//!
//! Nothing in this module returns `Err` to a producer. A full disk, an
//! unwritable home, a corrupt entry — every one of them degrades to "this
//! notification was not queued", which is exactly the behaviour producers
//! already had. `dispatch_with_outbox` reports what happened through
//! [`NotifyOutboxDisposition`] so a producer that owns an exactly-once marker
//! can decide whether to hold or release it, but it can never fail.
//!
//! # Claiming
//!
//! The daemon's run-completion notifier claims a delivery with a conditional
//! `UPDATE ... WHERE $.notification_delivered IS NULL` — a compare-and-set, so
//! two observers racing on the same run produce exactly one notification. This
//! module does not invent a second claim protocol; it uses the filesystem's
//! compare-and-set with the same shape. An entry is claimed by atomically
//! renaming it out of `pending/` into `inflight/`: `rename` either moves the
//! only copy or fails `ENOENT` because another drainer already moved it.
//!
//! The *subject*'s exactly-once marker stays where it already lives, and the
//! outbox never touches it. A producer that wins a claim and then queues the
//! event has **discharged** that claim: the event is durable, so it will be
//! delivered or dead-lettered by the drain, and no second observer may
//! re-announce it. The entry records the [`NotifyOnceMarker`] it consumed as
//! provenance — so a dead letter names the claim it holds — not as something
//! the outbox later settles.
//!
//! # Filesystem roots
//!
//! Every entry point comes in a pair: an ambient one that resolves the config
//! root from the process environment, and an `_in_root` sibling that takes an
//! already-resolved one (#7505). The ambient wrapper resolves *once* and then
//! threads that value, so a single drain pass cannot claim an entry in one
//! installation and reschedule or dead-letter it in another — which would
//! strand the entry in the first installation's `inflight/` until the reclaim
//! window elapsed, exactly the loss this module exists to prevent.
//!
//! The transport an attempt is delivered through is *not* rooted here: it comes
//! from the notification-transport registry that `notify::dispatch` resolves,
//! which is config/extension-store state rather than queue state.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::notify::{self, NotifyDelivery, NotifyEvent, NotifyOutcome, NotifyTerminalRejection};

/// Schema of a persisted outbox entry.
pub const NOTIFY_OUTBOX_ENTRY_SCHEMA: &str = "homeboy/notification-outbox-entry/v1";
/// Schema of a drain report.
pub const NOTIFY_OUTBOX_DRAIN_SCHEMA: &str = "homeboy/notification-outbox-drain/v1";

/// First retry delay. Matches the daemon's completion-notify cadence so the
/// very next tick after a failed inline attempt is the one that retries.
const BASE_BACKOFF_SECS: u64 = 5;
/// Ceiling on a single backoff step. A transport that has been down for a
/// quarter of an hour is an operator problem, not a polling problem.
const MAX_BACKOFF_SECS: u64 = 900;

/// Attempts (inline plus redeliveries) before an entry is dead-lettered.
///
/// With the 5s-doubling ladder this spends roughly twenty-one minutes of
/// wall-clock trying before giving up, which comfortably covers a transport
/// restart, a rate-limit window, or a brief network partition without leaving
/// an undeliverable event cycling forever.
pub const MAX_ATTEMPTS: u32 = 8;

/// Entries a single drain pass will attempt. A pass runs on the daemon's
/// five-second tick, and each attempt spawns a transport child; without a bound
/// a large backlog would turn one tick into an unbounded stall of the
/// completion notifier that shares the thread.
const MAX_DRAIN_PER_PASS: usize = 32;

/// How long a claimed entry may sit in `inflight/` before a later drain
/// reclaims it. Only a process that died mid-attempt leaves one behind, and
/// the reclaim mirrors how the job store reconciles expired reservations when
/// the daemon opens it.
const INFLIGHT_RECLAIM_SECS: i64 = 300;

/// The exactly-once claim a producer already won for this event's subject.
///
/// Provenance only. The outbox neither establishes nor releases one of these:
/// a producer that queues an event has already discharged its claim, because
/// the event is now durable and the drain owns delivery. Recording it means a
/// dead-lettered entry can name the claim it consumed, which is otherwise
/// unrecoverable from the entry alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyOnceMarker {
    /// Generic subject class the marker is keyed on, e.g. `agent_task_cook`.
    pub subject_kind: String,
    /// The claimed subject id.
    pub subject_id: String,
    /// Attribution recorded on the claim by its producer.
    pub claimed_by: String,
}

impl NotifyOnceMarker {
    pub fn new(
        subject_kind: impl Into<String>,
        subject_id: impl Into<String>,
        claimed_by: impl Into<String>,
    ) -> Self {
        Self {
            subject_kind: subject_kind.into(),
            subject_id: subject_id.into(),
            claimed_by: claimed_by.into(),
        }
    }
}

/// A durable, redeliverable notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyOutboxEntry {
    #[serde(default = "entry_schema")]
    pub schema: String,
    pub entry_id: String,
    pub enqueued_at: String,
    pub event: NotifyEvent,
    /// Delivery attempts already made, inline attempt included.
    #[serde(default)]
    pub attempts: u32,
    /// Earliest time a drain may attempt this entry again (RFC 3339).
    pub next_attempt_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Explicit terminal rejection retained as durable dead-letter evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_rejection: Option<NotifyTerminalRejection>,
    /// The producer-owned exactly-once claim this entry consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once_marker: Option<NotifyOnceMarker>,
}

fn entry_schema() -> String {
    NOTIFY_OUTBOX_ENTRY_SCHEMA.to_string()
}

/// What `dispatch_with_outbox` did beyond attempting delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyOutboxDisposition {
    /// A transport accepted the event on the inline attempt. Nothing queued.
    Delivered,
    /// The inline attempt failed and the event is durably queued. A producer
    /// holding an exactly-once claim should **consume** it: the outbox now owns
    /// redelivery, and leaving the claim releasable would let a second observer
    /// announce the same outcome alongside the retry.
    Queued { entry_id: String },
    /// The transport rejected the event before making its own delivery attempt
    /// and explicitly said retrying cannot succeed.
    Rejected { entry_id: String },
    /// The inline attempt failed and nothing was queued. Either the failure is
    /// not retryable (no transport configured, or the named transport is not
    /// installed) or the queue itself could not be written. A producer holding
    /// an exactly-once claim should release it, exactly as it did before the
    /// outbox existed.
    Dropped,
}

/// The result of an outbox-backed dispatch.
#[derive(Debug)]
pub struct NotifyOutboxDispatch {
    pub outcome: NotifyOutcome,
    pub disposition: NotifyOutboxDisposition,
}

/// One entry's fate in a drain pass.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyOutboxDrainEntry {
    pub entry_id: String,
    pub attempts: u32,
    /// `delivered`, `retrying`, `dead_lettered`, or `unreadable`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What one drain pass did.
#[derive(Debug, Clone, Serialize)]
pub struct NotifyOutboxDrainReport {
    pub schema: &'static str,
    /// Entries that were due and attempted in this pass.
    pub attempted: usize,
    pub delivered: usize,
    pub retrying: usize,
    pub dead_lettered: usize,
    /// Entries reclaimed from a drainer that died mid-attempt.
    pub reclaimed: usize,
    /// Entries whose file could not be parsed and were dead-lettered unread.
    pub unreadable: usize,
    /// `true` when the per-pass bound stopped the pass with work still due.
    pub truncated: bool,
    pub entries: Vec<NotifyOutboxDrainEntry>,
}

impl NotifyOutboxDrainReport {
    fn empty() -> Self {
        Self {
            schema: NOTIFY_OUTBOX_DRAIN_SCHEMA,
            attempted: 0,
            delivered: 0,
            retrying: 0,
            dead_lettered: 0,
            reclaimed: 0,
            unreadable: 0,
            truncated: false,
            entries: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The queue directory below an already-resolved *config* root.
fn outbox_root_in_root(config_root: &Path) -> PathBuf {
    config_root.join("notify-outbox")
}

fn pending_dir_in_root(config_root: &Path) -> PathBuf {
    outbox_root_in_root(config_root).join("pending")
}

fn inflight_dir_in_root(config_root: &Path) -> PathBuf {
    outbox_root_in_root(config_root).join("inflight")
}

fn dead_letter_dir_in_root(config_root: &Path) -> PathBuf {
    outbox_root_in_root(config_root).join("dead-letter")
}

/// The config root the ambient wrappers hang from.
///
/// `None` is "no queue is reachable", which every ambient entry point already
/// degrades to a dropped notification rather than an error (see the module
/// header). It is resolved once per public call and then threaded, so a single
/// drain pass cannot claim an entry in one installation and release it in
/// another.
fn ambient_config_root() -> Option<PathBuf> {
    crate::paths::homeboy().ok()
}

fn pending_dir() -> Option<PathBuf> {
    Some(pending_dir_in_root(&ambient_config_root()?))
}

/// Only the tests below still resolve this ambiently. Production reaches the
/// inflight directory through an injected root (#7505).
#[cfg(test)]
fn inflight_dir() -> Option<PathBuf> {
    Some(inflight_dir_in_root(&ambient_config_root()?))
}

fn dead_letter_dir() -> Option<PathBuf> {
    Some(dead_letter_dir_in_root(&ambient_config_root()?))
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

/// Delay before the next attempt, given how many attempts have already been
/// made. `attempts_made` of 1 (the inline attempt) yields the base delay.
///
/// Exposed for tests so the ladder is asserted rather than described.
pub fn backoff_after(attempts_made: u32) -> Duration {
    // Clamped before the shift: `1u64 << 64` is undefined behaviour in the
    // debug sense (a panic), and the ladder saturates at the cap long before 20
    // doublings anyway.
    let steps = attempts_made.saturating_sub(1).min(20);
    let secs = BASE_BACKOFF_SECS
        .saturating_mul(1u64 << steps)
        .min(MAX_BACKOFF_SECS);
    Duration::from_secs(secs)
}

/// Whether a failed delivery could plausibly succeed on a later attempt.
///
/// A transport that was spawned and rejected the event, or could not be
/// spawned at all, is an outage: retry. `NotConfigured` is a configuration
/// state, not an outage — no transport is selected, or the selected one is not
/// declared by any installed extension — and retrying it produces an entry
/// that can only ever dead-letter. Producers already record that case as
/// `not_configured` and leave the event eligible for a later observer, which
/// stays the right behaviour.
fn terminal_rejection(outcome: &NotifyOutcome) -> Option<NotifyTerminalRejection> {
    (!outcome.delivered && matches!(outcome.delivery, NotifyDelivery::Transport { .. }))
        .then(|| notify::terminal_rejection_result(outcome.result.as_ref()))
        .flatten()
}

fn is_retryable(outcome: &NotifyOutcome) -> bool {
    !outcome.delivered
        && matches!(outcome.delivery, NotifyDelivery::Transport { .. })
        && terminal_rejection(outcome).is_none()
}

// ---------------------------------------------------------------------------
// Producer API
// ---------------------------------------------------------------------------

/// Attempt delivery now, and durably queue the event when the attempt fails
/// for a retryable reason.
///
/// This is a drop-in replacement for `notify::dispatch` at any producer that
/// cannot afford to lose the event. It never returns an error and never
/// panics on a storage failure.
pub fn dispatch_with_outbox(
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
) -> NotifyOutboxDispatch {
    dispatch_with_outbox_inner(ambient_config_root().as_deref(), event, once_marker)
}

/// [`dispatch_with_outbox`] against an already-resolved config root.
pub fn dispatch_with_outbox_in_root(
    config_root: &Path,
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
) -> NotifyOutboxDispatch {
    dispatch_with_outbox_inner(Some(config_root), event, once_marker)
}

/// `config_root` is `Option` rather than `&Path` so the ambient wrapper keeps
/// its pre-injection laziness: an unresolvable home must still attempt inline
/// delivery and report `Delivered`, and only the *queueing* step degrades to
/// `Dropped`. Resolving eagerly and bailing would turn a deliverable event into
/// a dropped one on a machine with no `HOME`.
fn dispatch_with_outbox_inner(
    config_root: Option<&Path>,
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
) -> NotifyOutboxDispatch {
    let outcome = notify::dispatch(event);
    if outcome.delivered {
        return NotifyOutboxDispatch {
            outcome,
            disposition: NotifyOutboxDisposition::Delivered,
        };
    }
    if !is_retryable(&outcome) {
        let rejected = terminal_rejection(&outcome);
        return NotifyOutboxDispatch {
            outcome,
            disposition: if let Some(rejection) = rejected {
                reject_after_attempt(config_root, event, once_marker, rejection)
                    .map_or(NotifyOutboxDisposition::Dropped, |entry_id| {
                        NotifyOutboxDisposition::Rejected { entry_id }
                    })
            } else {
                NotifyOutboxDisposition::Dropped
            },
        };
    }
    let queued = config_root.and_then(|config_root| {
        enqueue_after_attempt(config_root, event, once_marker, outcome.error.clone())
    });
    let disposition = match queued {
        Some(entry_id) => NotifyOutboxDisposition::Queued { entry_id },
        None => NotifyOutboxDisposition::Dropped,
    };
    NotifyOutboxDispatch {
        outcome,
        disposition,
    }
}

/// Queue an event for the drain without attempting it inline.
///
/// Returns the entry id, or `None` when the queue could not be written —
/// which is a dropped notification, never a caller error.
pub fn enqueue(event: &NotifyEvent, once_marker: Option<NotifyOnceMarker>) -> Option<String> {
    enqueue_in_root(&ambient_config_root()?, event, once_marker)
}

/// [`enqueue`] against an already-resolved config root.
pub fn enqueue_in_root(
    config_root: &Path,
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
) -> Option<String> {
    write_new_entry(
        config_root,
        event,
        once_marker,
        0,
        Utc::now(),
        None,
        None,
        false,
    )
}

fn enqueue_after_attempt(
    config_root: &Path,
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
    error: Option<String>,
) -> Option<String> {
    let now = Utc::now();
    let due = now
        + chrono::Duration::from_std(backoff_after(1)).unwrap_or_else(|_| chrono::Duration::zero());
    write_new_entry(config_root, event, once_marker, 1, due, error, None, false)
}

fn reject_after_attempt(
    config_root: Option<&Path>,
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
    rejection: NotifyTerminalRejection,
) -> Option<String> {
    write_new_entry(
        config_root?,
        event,
        once_marker,
        0,
        Utc::now(),
        None,
        Some(rejection),
        true,
    )
}

fn write_new_entry(
    config_root: &Path,
    event: &NotifyEvent,
    once_marker: Option<NotifyOnceMarker>,
    attempts: u32,
    next_attempt_at: DateTime<Utc>,
    last_error: Option<String>,
    terminal_rejection: Option<NotifyTerminalRejection>,
    dead_letter: bool,
) -> Option<String> {
    let now = Utc::now();
    let entry_id = format!(
        "{}-{}",
        now.format("%Y%m%dT%H%M%S%3fZ"),
        uuid::Uuid::new_v4().simple()
    );
    let entry = NotifyOutboxEntry {
        schema: entry_schema(),
        entry_id: entry_id.clone(),
        enqueued_at: now.to_rfc3339(),
        event: event.clone(),
        attempts,
        next_attempt_at: next_attempt_at.to_rfc3339(),
        last_attempt_at: (attempts > 0).then(|| now.to_rfc3339()),
        last_error,
        terminal_rejection,
        once_marker,
    };
    let directory = if dead_letter {
        dead_letter_dir_in_root(config_root)
    } else {
        pending_dir_in_root(config_root)
    };
    let path = directory.join(format!("{entry_id}.json"));
    write_entry(&path, &entry).then_some(entry_id)
}

fn write_entry(path: &Path, entry: &NotifyOutboxEntry) -> bool {
    let Ok(bytes) = serde_json::to_vec_pretty(entry) else {
        return false;
    };
    crate::io::write_output_file_atomically(path, bytes, crate::io::OutputWriteOptions::artifact())
        .is_ok()
}

// ---------------------------------------------------------------------------
// Drain
// ---------------------------------------------------------------------------

/// Attempt every due entry once, rescheduling or dead-lettering each.
///
/// Called by the daemon on its completion tick. Returns a report rather than a
/// `Result`: a drain that cannot read its own directory has nothing to do, and
/// that is not an error the daemon can act on.
pub fn drain(now: DateTime<Utc>) -> NotifyOutboxDrainReport {
    let Some(config_root) = ambient_config_root() else {
        // No reachable queue: nothing to claim, nothing to release.
        return NotifyOutboxDrainReport::empty();
    };
    drain_in_root(&config_root, now)
}

/// [`drain`] against an already-resolved config root.
///
/// Every directory this pass touches — `pending/`, `inflight/`, and
/// `dead-letter/` — is derived from this one root and threaded through
/// `reclaim`, `claim`, and `apply_attempt`. A claim taken in one installation
/// can therefore never be released, rescheduled, or dead-lettered in another,
/// which would strand the entry in the first installation's `inflight/` until
/// its reclaim window elapsed.
///
/// Delivery itself (`notify::dispatch`) still resolves its transport from the
/// ambient config; that is the notification *transport* registry, not this
/// queue's state, and rooting it belongs to the config/extension-store layer.
pub fn drain_in_root(config_root: &Path, now: DateTime<Utc>) -> NotifyOutboxDrainReport {
    let mut report = NotifyOutboxDrainReport::empty();
    let pending = pending_dir_in_root(config_root);
    report.reclaimed = reclaim_stale_inflight(config_root, now);

    let mut due = due_entries(&pending, now);
    // Oldest deadline first, so a long-backed-off entry is not starved by a
    // steady arrival of fresh ones.
    due.sort_by(|left, right| left.0.cmp(&right.0));
    if due.len() > MAX_DRAIN_PER_PASS {
        report.truncated = true;
        due.truncate(MAX_DRAIN_PER_PASS);
    }

    for (_, path) in due {
        let Some(claimed) = claim(config_root, &path) else {
            // Another drainer won the rename, or the entry vanished.
            continue;
        };
        let Some(entry) = read_entry(&claimed) else {
            report.unreadable += 1;
            report.entries.push(NotifyOutboxDrainEntry {
                entry_id: file_stem(&claimed),
                attempts: 0,
                outcome: "unreadable",
                next_attempt_at: None,
                error: Some("outbox entry could not be parsed".to_string()),
            });
            // An unparseable entry can never be delivered, and leaving it in
            // `pending/` would make every future pass re-read it forever.
            move_to_dead_letter(config_root, &claimed);
            continue;
        };
        report.attempted += 1;
        apply_attempt(config_root, &claimed, entry, now, &mut report);
    }
    report
}

fn due_entries(pending: &Path, now: DateTime<Utc>) -> Vec<(DateTime<Utc>, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(pending) else {
        return Vec::new();
    };
    let mut due = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = read_entry(&path) else {
            // Unparseable: treat as immediately due so the pass can retire it.
            due.push((DateTime::<Utc>::MIN_UTC, path));
            continue;
        };
        let deadline = parse_time(&record.next_attempt_at).unwrap_or(now);
        if deadline <= now {
            due.push((deadline, path));
        }
    }
    due
}

/// Claim an entry by atomically moving it out of `pending/`.
///
/// This is the filesystem's compare-and-set. Two drainers that both saw the
/// entry both call `rename`; the loser's source no longer exists and it gets
/// `ENOENT`, exactly as the second writer of the run-completion marker gets
/// zero updated rows.
fn claim(config_root: &Path, path: &Path) -> Option<PathBuf> {
    let inflight = inflight_dir_in_root(config_root);
    if std::fs::create_dir_all(&inflight).is_err() {
        return None;
    }
    let target = inflight.join(path.file_name()?);
    std::fs::rename(path, &target).ok().map(|()| target)
}

fn apply_attempt(
    config_root: &Path,
    claimed: &Path,
    mut entry: NotifyOutboxEntry,
    now: DateTime<Utc>,
    report: &mut NotifyOutboxDrainReport,
) {
    let outcome = notify::dispatch(&entry.event);
    entry.attempts = entry.attempts.saturating_add(1);
    entry.last_attempt_at = Some(now.to_rfc3339());

    if outcome.delivered {
        report.delivered += 1;
        report.entries.push(NotifyOutboxDrainEntry {
            entry_id: entry.entry_id.clone(),
            attempts: entry.attempts,
            outcome: "delivered",
            next_attempt_at: None,
            error: None,
        });
        let _ = std::fs::remove_file(claimed);
        return;
    }

    entry.last_error = outcome.error.clone();
    if let Some(rejection) = terminal_rejection(&outcome) {
        entry.terminal_rejection = Some(rejection);
        report.dead_lettered += 1;
        report.entries.push(NotifyOutboxDrainEntry {
            entry_id: entry.entry_id.clone(),
            attempts: entry.attempts,
            outcome: "terminal_rejected",
            next_attempt_at: None,
            error: entry.last_error.clone(),
        });
        let path = dead_letter_dir_in_root(config_root).join(format!("{}.json", entry.entry_id));
        if write_entry(&path, &entry) {
            let _ = std::fs::remove_file(claimed);
        }
        return;
    }
    // A now-unconfigured transport is retried like any other failure until the
    // budget runs out. Unlike the inline path there is no producer left to
    // record `not_configured` against, and an operator repairing the transport
    // inside the retry window is the case worth covering.
    if entry.attempts >= MAX_ATTEMPTS {
        report.dead_lettered += 1;
        report.entries.push(NotifyOutboxDrainEntry {
            entry_id: entry.entry_id.clone(),
            attempts: entry.attempts,
            outcome: "dead_lettered",
            next_attempt_at: None,
            error: entry.last_error.clone(),
        });
        let dead = dead_letter_dir_in_root(config_root);
        let path = dead.join(format!("{}.json", entry.entry_id));
        if !write_entry(&path, &entry) {
            // Nowhere to record it; dropping is still better than leaving a
            // permanently-failing entry cycling in the queue.
            let _ = std::fs::remove_file(claimed);
            return;
        }
        let _ = std::fs::remove_file(claimed);
        return;
    }

    let delay = chrono::Duration::from_std(backoff_after(entry.attempts))
        .unwrap_or_else(|_| chrono::Duration::zero());
    let next = now + delay;
    entry.next_attempt_at = next.to_rfc3339();
    report.retrying += 1;
    report.entries.push(NotifyOutboxDrainEntry {
        entry_id: entry.entry_id.clone(),
        attempts: entry.attempts,
        outcome: "retrying",
        next_attempt_at: Some(entry.next_attempt_at.clone()),
        error: entry.last_error.clone(),
    });
    let path = pending_dir_in_root(config_root).join(format!("{}.json", entry.entry_id));
    if write_entry(&path, &entry) {
        let _ = std::fs::remove_file(claimed);
    }
    // If the rewrite failed the entry stays in `inflight/` and the stale
    // reclaim returns it to `pending/` later. Losing it here would be the one
    // outcome the outbox exists to prevent.
}

/// Return entries abandoned by a drainer that died mid-attempt.
fn reclaim_stale_inflight(config_root: &Path, now: DateTime<Utc>) -> usize {
    let inflight = inflight_dir_in_root(config_root);
    let pending = pending_dir_in_root(config_root);
    let Ok(entries) = std::fs::read_dir(&inflight) else {
        return 0;
    };
    if std::fs::create_dir_all(&pending).is_err() {
        return 0;
    }
    let mut reclaimed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let stale = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .map(|modified| {
                DateTime::<Utc>::from(modified) + chrono::Duration::seconds(INFLIGHT_RECLAIM_SECS)
                    <= now
            })
            // An entry whose age cannot be read is reclaimed rather than
            // stranded: a duplicate attempt is recoverable, an entry nothing
            // ever looks at again is not.
            .unwrap_or(true);
        if !stale {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        if std::fs::rename(&path, pending.join(name)).is_ok() {
            reclaimed += 1;
        }
    }
    reclaimed
}

fn move_to_dead_letter(config_root: &Path, claimed: &Path) {
    let dead = dead_letter_dir_in_root(config_root);
    if std::fs::create_dir_all(&dead).is_err() {
        let _ = std::fs::remove_file(claimed);
        return;
    }
    let Some(name) = claimed.file_name() else {
        return;
    };
    if std::fs::rename(claimed, dead.join(name)).is_err() {
        let _ = std::fs::remove_file(claimed);
    }
}

fn read_entry(path: &Path) -> Option<NotifyOutboxEntry> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// Every entry currently queued for redelivery, oldest deadline first.
pub fn pending_entries() -> Vec<NotifyOutboxEntry> {
    read_dir_entries_opt(pending_dir())
}

/// [`pending_entries`] against an already-resolved config root.
pub fn pending_entries_in_root(config_root: &Path) -> Vec<NotifyOutboxEntry> {
    read_dir_entries(&pending_dir_in_root(config_root))
}

/// Every entry that exhausted its attempt budget.
pub fn dead_letter_entries() -> Vec<NotifyOutboxEntry> {
    read_dir_entries_opt(dead_letter_dir())
}

/// [`dead_letter_entries`] against an already-resolved config root.
pub fn dead_letter_entries_in_root(config_root: &Path) -> Vec<NotifyOutboxEntry> {
    read_dir_entries(&dead_letter_dir_in_root(config_root))
}

/// [`inflight_entries`] against an already-resolved config root.
pub fn inflight_entries_in_root(config_root: &Path) -> Vec<NotifyOutboxEntry> {
    read_dir_entries(&inflight_dir_in_root(config_root))
}

fn read_dir_entries_opt(dir: Option<PathBuf>) -> Vec<NotifyOutboxEntry> {
    dir.map(|dir| read_dir_entries(&dir)).unwrap_or_default()
}

fn read_dir_entries(dir: &Path) -> Vec<NotifyOutboxEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut records: Vec<NotifyOutboxEntry> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|path| read_entry(&path))
        .collect();
    records.sort_by(|left, right| left.next_attempt_at.cmp(&right.next_attempt_at));
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notification_payload::NotifyEventKind;

    fn install_transport(id: &str, command: Vec<&str>) {
        let mut manifest: homeboy_extension_contract::ExtensionManifest =
            serde_json::from_value(serde_json::json!({
                "name": "Outbox transport",
                "version": "1.0.0",
                "notification_transports": [{
                    "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
                    "id": id,
                    "command": command,
                }]
            }))
            .unwrap();
        manifest.id = "outbox-test-transport".to_string();
        crate::extension_store::save_manifest(&manifest).unwrap();
    }

    fn set_default_transport(id: &str) {
        crate::defaults::save_config(&crate::defaults::HomeboyConfig {
            notifications: crate::defaults::NotificationConfig {
                default_transport: Some(id.to_string()),
            },
            ..Default::default()
        })
        .unwrap();
    }

    fn event(run_id: &str) -> NotifyEvent {
        NotifyEvent::run_completed(run_id, "fail")
    }

    #[test]
    fn backoff_doubles_from_the_tick_cadence_and_is_capped() {
        assert_eq!(backoff_after(1), Duration::from_secs(5));
        assert_eq!(backoff_after(2), Duration::from_secs(10));
        assert_eq!(backoff_after(3), Duration::from_secs(20));
        assert_eq!(backoff_after(8), Duration::from_secs(640));
        // Beyond the budget the ladder still saturates rather than overflowing.
        assert_eq!(backoff_after(64), Duration::from_secs(MAX_BACKOFF_SECS));
    }

    #[test]
    fn a_failed_terminal_notification_is_queued_rather_than_lost() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("outbox.down", vec!["false"]);
            set_default_transport("outbox.down");

            let dispatch = dispatch_with_outbox(&event("run-outage"), None);
            assert!(!dispatch.outcome.delivered);
            assert!(
                matches!(dispatch.disposition, NotifyOutboxDisposition::Queued { .. }),
                "{:?}",
                dispatch.disposition
            );
            let queued = pending_entries();
            assert_eq!(queued.len(), 1);
            assert_eq!(queued[0].attempts, 1);
            assert_eq!(queued[0].event.run_id, "run-outage");
        });
    }

    #[test]
    fn an_unconfigured_transport_is_not_queued() {
        // Nothing installed: this is configuration, not an outage. Queuing it
        // would guarantee a dead letter and would take the producer's
        // exactly-once claim hostage for twenty minutes first.
        crate::test_support::with_isolated_home(|_| {
            let dispatch = dispatch_with_outbox(&event("run-unconfigured"), None);
            assert_eq!(dispatch.disposition, NotifyOutboxDisposition::Dropped);
            assert!(pending_entries().is_empty());
        });
    }

    #[test]
    fn a_rejection_before_an_attempt_is_not_queued() {
        crate::test_support::with_isolated_home(|_| {
            install_transport(
                "outbox.rejected",
                vec!["sh", "-c", "printf '%s\\n' '{\"schema\":\"homeboy/notification-transport-result/v1\",\"status\":\"rejected\",\"attempts\":0,\"terminal_rejection\":{\"schema\":\"homeboy/notification-transport-rejection/v1\",\"reason_code\":\"invalid_route\",\"validation_field\":\"route\"}}'; exit 1"],
            );
            set_default_transport("outbox.rejected");

            let dispatch = dispatch_with_outbox(&event("run-rejected"), None);
            assert!(matches!(
                dispatch.disposition,
                NotifyOutboxDisposition::Rejected { .. }
            ));
            assert!(pending_entries().is_empty());
            let dead = dead_letter_entries();
            assert_eq!(dead.len(), 1);
            assert_eq!(dead[0].attempts, 0);
            assert_eq!(
                dead[0]
                    .terminal_rejection
                    .as_ref()
                    .map(|rejection| rejection.reason_code.as_str()),
                Some("invalid_route")
            );
        });
    }

    #[test]
    fn arbitrary_retryable_false_output_remains_retryable() {
        crate::test_support::with_isolated_home(|_| {
            install_transport(
                "outbox.unversioned",
                vec!["sh", "-c", "printf '%s\\n' '{\"retryable\":false}'; exit 1"],
            );
            set_default_transport("outbox.unversioned");

            let dispatch = dispatch_with_outbox(&event("run-unversioned"), None);
            assert!(matches!(
                dispatch.disposition,
                NotifyOutboxDisposition::Queued { .. }
            ));
            assert_eq!(pending_entries().len(), 1);
            assert!(dead_letter_entries().is_empty());
        });
    }

    #[test]
    fn a_redelivery_rejection_is_dead_lettered_with_attempt_history() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("outbox.reject-later", vec!["false"]);
            set_default_transport("outbox.reject-later");
            dispatch_with_outbox(&event("run-reject-later"), None);
            install_transport(
                "outbox.reject-later",
                vec!["sh", "-c", "printf '%s\\n' '{\"schema\":\"homeboy/notification-transport-result/v1\",\"status\":\"rejected\",\"attempts\":0,\"terminal_rejection\":{\"schema\":\"homeboy/notification-transport-rejection/v1\",\"reason_code\":\"invalid_route\"}}'; exit 1"],
            );

            let report = drain(Utc::now() + chrono::Duration::seconds(30));
            assert_eq!(report.dead_lettered, 1);
            assert_eq!(report.entries[0].outcome, "terminal_rejected");
            let dead = dead_letter_entries();
            assert_eq!(dead.len(), 1);
            assert_eq!(dead[0].attempts, 2);
            assert!(dead[0].terminal_rejection.is_some());
        });
    }

    #[test]
    fn the_outbox_redelivers_after_a_transient_transport_failure() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("outbox.flaky", vec!["false"]);
            set_default_transport("outbox.flaky");
            let dispatch = dispatch_with_outbox(&event("run-transient"), None);
            assert!(matches!(
                dispatch.disposition,
                NotifyOutboxDisposition::Queued { .. }
            ));

            // The transport comes back before the entry is due.
            install_transport("outbox.flaky", vec!["true"]);

            // Not yet due: the pass must leave it alone.
            let early = drain(Utc::now());
            assert_eq!(early.attempted, 0);
            assert_eq!(pending_entries().len(), 1);

            let report = drain(Utc::now() + chrono::Duration::seconds(30));
            assert_eq!(report.attempted, 1);
            assert_eq!(report.delivered, 1);
            assert!(pending_entries().is_empty());
            assert!(dead_letter_entries().is_empty());
        });
    }

    #[test]
    fn the_outbox_dead_letters_after_the_attempt_budget() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("outbox.dead", vec!["false"]);
            set_default_transport("outbox.dead");
            dispatch_with_outbox(&event("run-dead"), None);

            let mut now = Utc::now();
            // The inline attempt already consumed one of the budget.
            for _ in 1..MAX_ATTEMPTS {
                now += chrono::Duration::seconds(MAX_BACKOFF_SECS as i64 + 1);
                drain(now);
            }
            assert!(
                pending_entries().is_empty(),
                "expected the entry to be retired: {:?}",
                pending_entries()
            );
            let dead = dead_letter_entries();
            assert_eq!(dead.len(), 1);
            assert_eq!(dead[0].attempts, MAX_ATTEMPTS);
            assert!(dead[0].last_error.is_some());
        });
    }

    #[test]
    fn a_structured_payload_survives_the_round_trip() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("outbox.payload", vec!["false"]);
            set_default_transport("outbox.payload");
            let payload = crate::notification_payload::NotifyPayload::new(
                NotifyEventKind::NeedsAttention,
                crate::notification_payload::NotifySubject::new("agent_task_cook", "cook-abc"),
            )
            .with_fact("Stop reason", "gate_failed");
            dispatch_with_outbox(&event("run-payload").with_payload(payload), None);

            let queued = pending_entries();
            assert_eq!(queued.len(), 1);
            let restored = queued[0].event.payload.clone().expect("payload survives");
            assert_eq!(restored.kind, NotifyEventKind::NeedsAttention);
            assert_eq!(restored.subject.unwrap().id, "cook-abc");
            assert_eq!(queued[0].event.kind, NotifyEventKind::NeedsAttention);
        });
    }

    #[test]
    fn an_unparseable_entry_is_retired_instead_of_re_read_forever() {
        crate::test_support::with_isolated_home(|_| {
            let pending = pending_dir().unwrap();
            std::fs::create_dir_all(&pending).unwrap();
            std::fs::write(pending.join("corrupt.json"), b"{not json").unwrap();

            let report = drain(Utc::now());
            assert_eq!(report.unreadable, 1);
            assert!(!pending.join("corrupt.json").exists());
        });
    }

    #[test]
    fn an_abandoned_inflight_entry_is_reclaimed() {
        crate::test_support::with_isolated_home(|_| {
            install_transport("outbox.reclaim", vec!["true"]);
            set_default_transport("outbox.reclaim");
            let inflight = inflight_dir().unwrap();
            std::fs::create_dir_all(&inflight).unwrap();
            let entry = NotifyOutboxEntry {
                schema: entry_schema(),
                entry_id: "abandoned".to_string(),
                enqueued_at: Utc::now().to_rfc3339(),
                event: event("run-abandoned"),
                attempts: 1,
                next_attempt_at: Utc::now().to_rfc3339(),
                last_attempt_at: None,
                last_error: None,
                terminal_rejection: None,
                once_marker: None,
            };
            assert!(write_entry(&inflight.join("abandoned.json"), &entry));

            // Far enough in the future that the claim lease has expired.
            let later = Utc::now() + chrono::Duration::seconds(INFLIGHT_RECLAIM_SECS + 60);
            let report = drain(later);
            assert_eq!(report.reclaimed, 1);
            // Reclaimed into `pending/` and delivered by the same pass is not
            // required; the next pass may take it. Either way it is not lost.
            assert!(
                report.delivered == 1 || pending_entries().len() == 1,
                "{report:?}"
            );
        });
    }

    #[test]
    fn injected_roots_own_independent_queues() {
        // No `with_isolated_home`: an injected root must not consult the
        // process environment at all, which is the whole point of the
        // `_in_root` pair.
        let left = tempfile::tempdir().expect("left root");
        let right = tempfile::tempdir().expect("right root");

        let entry_id = enqueue_in_root(left.path(), &event("run-left"), None).expect("queued");

        let queued = pending_entries_in_root(left.path());
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].entry_id, entry_id);
        assert_eq!(queued[0].event.run_id, "run-left");
        assert!(pending_entries_in_root(right.path()).is_empty());
    }

    #[test]
    fn a_drain_claims_and_releases_inside_the_root_it_was_given() {
        // An unparseable entry exercises claim -> release without spawning a
        // transport, so this asserts the drain's *own* state machine rather
        // than delivery. A claim taken under one root and released under
        // another would strand the entry until its reclaim window elapsed.
        let root = tempfile::tempdir().expect("root");
        let other = tempfile::tempdir().expect("other root");
        let pending = pending_dir_in_root(root.path());
        std::fs::create_dir_all(&pending).expect("seed pending");
        std::fs::write(pending.join("corrupt.json"), b"{not json").expect("seed entry");

        let report = drain_in_root(root.path(), Utc::now());

        assert_eq!(report.unreadable, 1);
        assert!(!pending.join("corrupt.json").exists());
        assert!(
            !inflight_dir_in_root(root.path())
                .join("corrupt.json")
                .exists(),
            "the claim was released, not stranded in inflight"
        );
        assert!(
            dead_letter_dir_in_root(root.path())
                .join("corrupt.json")
                .exists(),
            "the release landed under the same root the claim was taken in"
        );
        assert!(!other.path().join("notify-outbox").exists());
    }

    #[test]
    fn a_drain_over_an_empty_or_missing_outbox_is_inert() {
        crate::test_support::with_isolated_home(|_| {
            let report = drain(Utc::now());
            assert_eq!(report.attempted, 0);
            assert_eq!(report.reclaimed, 0);
            assert!(report.entries.is_empty());
        });
    }
}
