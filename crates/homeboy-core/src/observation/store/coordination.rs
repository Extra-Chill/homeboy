//! Generic coordination primitives in the observation store (#15947).
//!
//! Three shapes cover the file-backed coordination stores Homeboy used to keep
//! beside the database:
//!
//! - **records**: mutable keyed state guarded by a revision compare-and-set;
//! - **leases**: claims, exclusive leases and counting semaphores, with a
//!   monotonic fence per key and owner liveness proven by process start
//!   identity rather than PID alone;
//! - **audit**: an append-only trail per subject.
//!
//! Every row lives under a namespace. Namespaces are `&'static str` constants
//! owned by the domain that uses them, and domains expose their own typed
//! wrappers instead of handing these rows to callers.

use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{begin_control_plane_write, sqlite_error, ObservationStore};
use crate::process::{
    host_identity, process_identity_state_with_start_identity, process_start_identity,
    ProcessIdentityState, ProcessStartIdentity,
};
use crate::{Error, Result};

/// Upper bound for one audit read, whatever the caller asks for.
pub const MAX_COORDINATION_AUDIT_READ: usize = 1000;

/// One keyed coordination record.
#[derive(Debug, Clone, PartialEq)]
pub struct CoordinationRecord {
    pub namespace: String,
    pub key: String,
    pub revision: i64,
    pub state: String,
    pub payload: Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

/// The revision a record write or delete expects to replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedRevision {
    /// Create the record; there must be no live record under the key.
    Absent,
    /// Replace exactly this revision.
    Revision(i64),
}

/// The fields a record write stores.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordWrite<'a> {
    pub state: &'a str,
    pub payload: &'a Value,
    /// When set, the record counts as absent from this instant on.
    pub expires_at_ms: Option<i64>,
}

/// Outcome of a compare-and-set record write or delete.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordOutcome {
    /// The write committed. `None` after a delete.
    Applied(Option<CoordinationRecord>),
    /// The expected revision did not match; `current` is what is stored now.
    Conflict { current: Option<CoordinationRecord> },
}

/// The process that owns a lease holder row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseOwner {
    pub pid: Option<u32>,
    /// Hash of [`host_identity`]; process evidence is only valid on this host.
    pub host: Option<String>,
    pub start_identity: Option<ProcessStartIdentity>,
}

impl LeaseOwner {
    /// The current process on the current host.
    pub fn current_process() -> Self {
        let pid = std::process::id();
        Self {
            pid: Some(pid),
            host: local_host(),
            start_identity: process_start_identity(pid).ok().flatten(),
        }
    }
}

/// How an acquire decides whether another holder may join.
pub enum LeaseAdmission<'a> {
    /// Admit only when the key has no live holder.
    Exclusive,
    /// Admit when the predicate accepts the current live holders, e.g. a
    /// counting semaphore summing their weights against a capacity.
    Capacity(&'a dyn Fn(&[LeaseHolder]) -> bool),
}

/// A lease acquisition request.
pub struct LeaseRequest<'a> {
    pub namespace: &'static str,
    pub key: &'a str,
    pub ttl: Duration,
    pub owner: LeaseOwner,
    pub admission: LeaseAdmission<'a>,
    pub weight: &'a Value,
    pub payload: &'a Value,
}

/// One live holder of a lease key.
#[derive(Debug, Clone, PartialEq)]
pub struct LeaseHolder {
    pub namespace: String,
    pub key: String,
    pub holder_token: String,
    pub fence: i64,
    pub owner: LeaseOwner,
    pub acquired_at_ms: i64,
    pub renewed_at_ms: i64,
    pub expires_at_ms: i64,
    pub weight: Value,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LeaseAcquire {
    Acquired(LeaseHolder),
    /// Admission refused; these are the live holders it was judged against.
    Contended(Vec<LeaseHolder>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LeaseRenew {
    Renewed(LeaseHolder),
    /// The holder token and fence no longer name a live holder.
    Lost,
}

/// One appended audit entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CoordinationAuditEntry {
    pub sequence: i64,
    pub namespace: String,
    pub subject_key: String,
    pub at_ms: i64,
    pub event: String,
    pub payload: Value,
}

impl ObservationStore {
    /// The live record under `key`, if any. Expired records read as absent.
    pub fn coordination_record(
        &self,
        namespace: &'static str,
        key: &str,
    ) -> Result<Option<CoordinationRecord>> {
        read_record(&self.connection, namespace, key, now_ms())
    }

    /// Every live record in `namespace`, ordered by key. Expired records are
    /// omitted, matching [`ObservationStore::coordination_record`].
    pub fn coordination_records(&self, namespace: &'static str) -> Result<Vec<CoordinationRecord>> {
        super::validate_required("coordination.namespace", namespace)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT namespace, record_key, revision, state, payload_json,
                        created_at_ms, updated_at_ms, expires_at_ms
                 FROM coordination_records
                 WHERE namespace = ?1
                   AND (expires_at_ms IS NULL OR expires_at_ms > ?2)
                 ORDER BY record_key",
            )
            .map_err(sqlite_error("prepare coordination record list"))?;
        let rows = statement
            .query_map(params![namespace, now_ms()], record_from_row)
            .map_err(sqlite_error("list coordination records"))?;
        super::collect_rows(rows, "collect coordination records")
    }

    /// Write a record when `expected` matches what is stored. Creating needs
    /// [`ExpectedRevision::Absent`]; an expired record counts as absent and is
    /// replaced with the next revision so revisions never repeat.
    pub fn put_coordination_record(
        &self,
        namespace: &'static str,
        key: &str,
        expected: ExpectedRevision,
        write: RecordWrite<'_>,
    ) -> Result<RecordOutcome> {
        put_record_at(&self.connection, namespace, key, expected, write, now_ms())
    }

    /// Delete a record when it is still at `expected_revision`.
    pub fn delete_coordination_record(
        &self,
        namespace: &'static str,
        key: &str,
        expected_revision: i64,
    ) -> Result<RecordOutcome> {
        validate_key(namespace, key)?;
        let tx = begin_control_plane_write(&self.connection, "begin coordination record delete")?;
        let current = read_record(&tx, namespace, key, now_ms())?;
        if current.as_ref().map(|record| record.revision) != Some(expected_revision) {
            return Ok(RecordOutcome::Conflict { current });
        }
        tx.execute(
            "DELETE FROM coordination_records WHERE namespace = ?1 AND record_key = ?2",
            params![namespace, key],
        )
        .map_err(sqlite_error("delete coordination record"))?;
        tx.commit()
            .map_err(sqlite_error("commit coordination record delete"))?;
        Ok(RecordOutcome::Applied(None))
    }

    /// Acquire a holder row for `request.key`. Expired holders of the key are
    /// dropped first; admission is then judged against the remaining holders
    /// inside the same IMMEDIATE transaction, so two acquirers cannot both
    /// pass an exclusive or capacity check.
    pub fn acquire_coordination_lease(&self, request: LeaseRequest<'_>) -> Result<LeaseAcquire> {
        acquire_lease_at(&self.connection, request, now_ms())
    }

    /// Extend a live holder. The fence must match so a holder that lost and
    /// re-won the key cannot renew with stale authority.
    pub fn renew_coordination_lease(
        &self,
        namespace: &'static str,
        key: &str,
        holder_token: &str,
        fence: i64,
        ttl: Duration,
    ) -> Result<LeaseRenew> {
        renew_lease_at(
            &self.connection,
            namespace,
            key,
            holder_token,
            fence,
            ttl,
            now_ms(),
        )
    }

    /// Release a holder. Returns whether a row was removed.
    pub fn release_coordination_lease(
        &self,
        namespace: &'static str,
        key: &str,
        holder_token: &str,
    ) -> Result<bool> {
        validate_key(namespace, key)?;
        let removed = self
            .connection
            .execute(
                "DELETE FROM coordination_leases
                 WHERE namespace = ?1 AND lease_key = ?2 AND holder_token = ?3",
                params![namespace, key, holder_token],
            )
            .map_err(sqlite_error("release coordination lease"))?;
        Ok(removed > 0)
    }

    /// The live holders of `key`, oldest fence first.
    pub fn coordination_lease_holders(
        &self,
        namespace: &'static str,
        key: &str,
    ) -> Result<Vec<LeaseHolder>> {
        let mut holders = holders_on(&self.connection, namespace, Some(key))?;
        let now = now_ms();
        holders.retain(|holder| holder.expires_at_ms > now);
        Ok(holders)
    }

    /// Remove holders in `namespace` that are expired or whose owner is proven
    /// dead on this host. A PID alone is not proof: a recorded start identity
    /// that no longer matches counts as dead, while holders on other hosts or
    /// with unverifiable owners are kept until they expire. Returns the
    /// removed holders.
    pub fn reap_coordination_leases(&self, namespace: &'static str) -> Result<Vec<LeaseHolder>> {
        reap_leases_at(&self.connection, namespace, now_ms(), &local_host())
    }

    /// Append one audit entry and return its sequence.
    pub fn append_coordination_audit(
        &self,
        namespace: &'static str,
        subject_key: &str,
        event: &str,
        payload: &Value,
    ) -> Result<i64> {
        validate_key(namespace, subject_key)?;
        self.connection
            .execute(
                "INSERT INTO coordination_audit(namespace, subject_key, at_ms, event, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![namespace, subject_key, now_ms(), event, to_json(payload)?],
            )
            .map_err(sqlite_error("append coordination audit"))?;
        Ok(self.connection.last_insert_rowid())
    }

    /// The newest `limit` audit entries for a subject, newest first. `limit`
    /// is capped at [`MAX_COORDINATION_AUDIT_READ`].
    pub fn coordination_audit(
        &self,
        namespace: &'static str,
        subject_key: &str,
        limit: usize,
    ) -> Result<Vec<CoordinationAuditEntry>> {
        let limit = limit.min(MAX_COORDINATION_AUDIT_READ) as i64;
        let mut statement = self
            .connection
            .prepare(
                "SELECT sequence, namespace, subject_key, at_ms, event, payload_json
                 FROM coordination_audit
                 WHERE namespace = ?1 AND subject_key = ?2
                 ORDER BY sequence DESC LIMIT ?3",
            )
            .map_err(sqlite_error("prepare coordination audit read"))?;
        let rows = statement
            .query_map(params![namespace, subject_key, limit], |row| {
                Ok(CoordinationAuditEntry {
                    sequence: row.get(0)?,
                    namespace: row.get(1)?,
                    subject_key: row.get(2)?,
                    at_ms: row.get(3)?,
                    event: row.get(4)?,
                    payload: super::parse_metadata(row.get(5)?)?,
                })
            })
            .map_err(sqlite_error("read coordination audit"))?;
        super::collect_rows(rows, "collect coordination audit")
    }

    /// The newest `limit` audit entries across every subject in a namespace,
    /// newest first. `limit` is capped at [`MAX_COORDINATION_AUDIT_READ`].
    pub fn coordination_audit_in_namespace(
        &self,
        namespace: &'static str,
        limit: usize,
    ) -> Result<Vec<CoordinationAuditEntry>> {
        super::validate_required("coordination.namespace", namespace)?;
        let limit = limit.min(MAX_COORDINATION_AUDIT_READ) as i64;
        let mut statement = self
            .connection
            .prepare(
                "SELECT sequence, namespace, subject_key, at_ms, event, payload_json
                 FROM coordination_audit
                 WHERE namespace = ?1
                 ORDER BY sequence DESC LIMIT ?2",
            )
            .map_err(sqlite_error("prepare coordination namespace audit read"))?;
        let rows = statement
            .query_map(params![namespace, limit], |row| {
                Ok(CoordinationAuditEntry {
                    sequence: row.get(0)?,
                    namespace: row.get(1)?,
                    subject_key: row.get(2)?,
                    at_ms: row.get(3)?,
                    event: row.get(4)?,
                    payload: super::parse_metadata(row.get(5)?)?,
                })
            })
            .map_err(sqlite_error("read coordination namespace audit"))?;
        super::collect_rows(rows, "collect coordination namespace audit")
    }
}

fn put_record_at(
    connection: &Connection,
    namespace: &'static str,
    key: &str,
    expected: ExpectedRevision,
    write: RecordWrite<'_>,
    now: i64,
) -> Result<RecordOutcome> {
    validate_key(namespace, key)?;
    super::validate_required("coordination_record.state", write.state)?;
    let payload = to_json(write.payload)?;
    let tx = begin_control_plane_write(connection, "begin coordination record write")?;
    let current = read_record(&tx, namespace, key, now)?;
    let matches = match expected {
        ExpectedRevision::Absent => current.is_none(),
        ExpectedRevision::Revision(revision) => {
            current.as_ref().map(|record| record.revision) == Some(revision)
        }
    };
    if !matches {
        return Ok(RecordOutcome::Conflict { current });
    }
    // An expired row is still physically present; continue its revision so a
    // stale writer holding the old revision can never match again.
    let stored: Option<i64> = tx
        .query_row(
            "SELECT revision FROM coordination_records WHERE namespace = ?1 AND record_key = ?2",
            params![namespace, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error("read coordination record revision"))?;
    let revision = stored.unwrap_or(0) + 1;
    let created_at_ms = current.as_ref().map_or(now, |record| record.created_at_ms);
    tx.execute(
        "INSERT INTO coordination_records(
             namespace, record_key, revision, state, payload_json,
             created_at_ms, updated_at_ms, expires_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(namespace, record_key) DO UPDATE SET
             revision = excluded.revision,
             state = excluded.state,
             payload_json = excluded.payload_json,
             created_at_ms = excluded.created_at_ms,
             updated_at_ms = excluded.updated_at_ms,
             expires_at_ms = excluded.expires_at_ms",
        params![
            namespace,
            key,
            revision,
            write.state,
            payload,
            created_at_ms,
            now,
            write.expires_at_ms
        ],
    )
    .map_err(sqlite_error("write coordination record"))?;
    let written = read_record(&tx, namespace, key, now)?;
    tx.commit()
        .map_err(sqlite_error("commit coordination record write"))?;
    Ok(RecordOutcome::Applied(written))
}

fn read_record(
    connection: &Connection,
    namespace: &str,
    key: &str,
    now: i64,
) -> Result<Option<CoordinationRecord>> {
    connection
        .query_row(
            "SELECT namespace, record_key, revision, state, payload_json,
                    created_at_ms, updated_at_ms, expires_at_ms
             FROM coordination_records
             WHERE namespace = ?1 AND record_key = ?2
               AND (expires_at_ms IS NULL OR expires_at_ms > ?3)",
            params![namespace, key, now],
            record_from_row,
        )
        .optional()
        .map_err(sqlite_error("read coordination record"))
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CoordinationRecord> {
    Ok(CoordinationRecord {
        namespace: row.get(0)?,
        key: row.get(1)?,
        revision: row.get(2)?,
        state: row.get(3)?,
        payload: super::parse_metadata(row.get(4)?)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        expires_at_ms: row.get(7)?,
    })
}

fn acquire_lease_at(
    connection: &Connection,
    request: LeaseRequest<'_>,
    now: i64,
) -> Result<LeaseAcquire> {
    validate_key(request.namespace, request.key)?;
    let ttl_ms = ttl_ms(request.ttl)?;
    let weight = to_json(request.weight)?;
    let payload = to_json(request.payload)?;
    let start_identity = request
        .owner
        .start_identity
        .as_ref()
        .map(|identity| {
            serde_json::to_string(identity).map_err(|error| {
                Error::internal_json(error.to_string(), Some("serialize lease owner".into()))
            })
        })
        .transpose()?;

    let tx = begin_control_plane_write(connection, "begin coordination lease acquire")?;
    tx.execute(
        "DELETE FROM coordination_leases
         WHERE namespace = ?1 AND lease_key = ?2 AND expires_at_ms <= ?3",
        params![request.namespace, request.key, now],
    )
    .map_err(sqlite_error("drop expired coordination leases"))?;
    let holders = holders_on(&tx, request.namespace, Some(request.key))?;
    let admitted = match request.admission {
        LeaseAdmission::Exclusive => holders.is_empty(),
        LeaseAdmission::Capacity(admit) => admit(&holders),
    };
    if !admitted {
        return Ok(LeaseAcquire::Contended(holders));
    }

    let fence: i64 = tx
        .query_row(
            "INSERT INTO coordination_fences(namespace, lease_key, last_fence)
             VALUES (?1, ?2, 1)
             ON CONFLICT(namespace, lease_key) DO UPDATE SET last_fence = last_fence + 1
             RETURNING last_fence",
            params![request.namespace, request.key],
            |row| row.get(0),
        )
        .map_err(sqlite_error("advance coordination lease fence"))?;
    let holder_token = uuid::Uuid::new_v4().to_string();
    let expires_at_ms = now.saturating_add(ttl_ms);
    tx.execute(
        "INSERT INTO coordination_leases(
             namespace, lease_key, holder_token, fence, owner_pid, owner_host,
             owner_start_identity_json, acquired_at_ms, renewed_at_ms, expires_at_ms,
             weight_json, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11)",
        params![
            request.namespace,
            request.key,
            holder_token,
            fence,
            request.owner.pid,
            request.owner.host,
            start_identity,
            now,
            expires_at_ms,
            weight,
            payload
        ],
    )
    .map_err(sqlite_error("insert coordination lease holder"))?;
    tx.commit()
        .map_err(sqlite_error("commit coordination lease acquire"))?;
    Ok(LeaseAcquire::Acquired(LeaseHolder {
        namespace: request.namespace.to_string(),
        key: request.key.to_string(),
        holder_token,
        fence,
        owner: request.owner,
        acquired_at_ms: now,
        renewed_at_ms: now,
        expires_at_ms,
        weight: request.weight.clone(),
        payload: request.payload.clone(),
    }))
}

fn renew_lease_at(
    connection: &Connection,
    namespace: &'static str,
    key: &str,
    holder_token: &str,
    fence: i64,
    ttl: Duration,
    now: i64,
) -> Result<LeaseRenew> {
    validate_key(namespace, key)?;
    let expires_at_ms = now.saturating_add(ttl_ms(ttl)?);
    let tx = begin_control_plane_write(connection, "begin coordination lease renew")?;
    let updated = tx
        .execute(
            "UPDATE coordination_leases
             SET renewed_at_ms = ?5, expires_at_ms = ?6
             WHERE namespace = ?1 AND lease_key = ?2 AND holder_token = ?3 AND fence = ?4
               AND expires_at_ms > ?5",
            params![namespace, key, holder_token, fence, now, expires_at_ms],
        )
        .map_err(sqlite_error("renew coordination lease"))?;
    if updated == 0 {
        return Ok(LeaseRenew::Lost);
    }
    let holder = holders_on(&tx, namespace, Some(key))?
        .into_iter()
        .find(|holder| holder.holder_token == holder_token);
    tx.commit()
        .map_err(sqlite_error("commit coordination lease renew"))?;
    Ok(holder.map_or(LeaseRenew::Lost, LeaseRenew::Renewed))
}

fn reap_leases_at(
    connection: &Connection,
    namespace: &'static str,
    now: i64,
    this_host: &Option<String>,
) -> Result<Vec<LeaseHolder>> {
    let tx = begin_control_plane_write(connection, "begin coordination lease reap")?;
    let reaped: Vec<LeaseHolder> = holders_on(&tx, namespace, None)?
        .into_iter()
        .filter(|holder| holder.expires_at_ms <= now || owner_is_dead(&holder.owner, this_host))
        .collect();
    for holder in &reaped {
        tx.execute(
            "DELETE FROM coordination_leases
             WHERE namespace = ?1 AND lease_key = ?2 AND holder_token = ?3",
            params![holder.namespace, holder.key, holder.holder_token],
        )
        .map_err(sqlite_error("reap coordination lease"))?;
    }
    tx.commit()
        .map_err(sqlite_error("commit coordination lease reap"))?;
    Ok(reaped)
}

/// Whether the owner is conclusively gone. Only evidence from this host
/// counts, and a live PID whose start identity changed is a reused PID.
fn owner_is_dead(owner: &LeaseOwner, this_host: &Option<String>) -> bool {
    let (Some(pid), Some(host)) = (owner.pid, owner.host.as_ref()) else {
        return false;
    };
    if this_host.as_ref() != Some(host) {
        return false;
    }
    matches!(
        process_identity_state_with_start_identity(pid, None, owner.start_identity.as_ref()),
        ProcessIdentityState::Dead | ProcessIdentityState::IdentityMismatch
    )
}

fn holders_on(
    connection: &Connection,
    namespace: &str,
    key: Option<&str>,
) -> Result<Vec<LeaseHolder>> {
    let mut statement = connection
        .prepare(
            "SELECT namespace, lease_key, holder_token, fence, owner_pid, owner_host,
                    owner_start_identity_json, acquired_at_ms, renewed_at_ms, expires_at_ms,
                    weight_json, payload_json
             FROM coordination_leases
             WHERE namespace = ?1 AND (?2 IS NULL OR lease_key = ?2)
             ORDER BY lease_key, fence",
        )
        .map_err(sqlite_error("prepare coordination lease read"))?;
    let rows = statement
        .query_map(params![namespace, key], |row| {
            let start_identity: Option<String> = row.get(6)?;
            let start_identity = start_identity
                .map(|raw| {
                    serde_json::from_str::<ProcessStartIdentity>(&raw).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .transpose()?;
            Ok(LeaseHolder {
                namespace: row.get(0)?,
                key: row.get(1)?,
                holder_token: row.get(2)?,
                fence: row.get(3)?,
                owner: LeaseOwner {
                    pid: row.get(4)?,
                    host: row.get(5)?,
                    start_identity,
                },
                acquired_at_ms: row.get(7)?,
                renewed_at_ms: row.get(8)?,
                expires_at_ms: row.get(9)?,
                weight: super::parse_metadata(row.get(10)?)?,
                payload: super::parse_metadata(row.get(11)?)?,
            })
        })
        .map_err(sqlite_error("read coordination leases"))?;
    super::collect_rows(rows, "collect coordination leases")
}

fn local_host() -> Option<String> {
    host_identity().map(|host| format!("{:x}", Sha256::digest(host.as_bytes())))
}

fn validate_key(namespace: &str, key: &str) -> Result<()> {
    super::validate_required("coordination.namespace", namespace)?;
    super::validate_required("coordination.key", key)
}

fn ttl_ms(ttl: Duration) -> Result<i64> {
    let ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
    if ms <= 0 {
        return Err(Error::validation_invalid_argument(
            "coordination_lease.ttl",
            "lease ttl must be at least one millisecond",
            None,
            None,
        ));
    }
    Ok(ms)
}

fn to_json(value: &Value) -> Result<String> {
    serde_json::to_string(value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize coordination json".into()),
        )
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
#[path = "coordination_tests.rs"]
mod tests;
