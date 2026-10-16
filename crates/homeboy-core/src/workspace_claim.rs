//! Durable, transport-neutral workspace authority.
//!
//! A workspace has either renewable owners or one short reconciliation claim.
//! The durable revision and opaque tokens, rather than clocks, establish authority.
//! The authority lives in the observation store as one coordination record per
//! workspace (#15947); every mutation is a revision compare-and-set.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::observation::coordination::{
    CoordinationRecord, ExpectedRevision, LeaseOwner, RecordOutcome, RecordWrite,
};
use crate::observation::ObservationStore;

pub use homeboy_runner_contract::{
    WorkspaceClaim, WorkspaceClaimBinding, WorkspaceClaimProtocol, WorkspaceIdentity,
    WorkspaceOwnerLease, WorkspaceOwnerLeaseProtocol, WORKSPACE_CLAIM_CAPABILITY,
    WORKSPACE_CLAIM_PROTOCOL_VERSION, WORKSPACE_CLAIM_SCHEMA, WORKSPACE_IDENTITY_SCHEMA,
    WORKSPACE_OWNER_LEASE_CAPABILITY, WORKSPACE_OWNER_LEASE_SCHEMA,
};
pub const WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA: &str =
    "homeboy/workspace-owner-release-recovery/v1";
/// Coordination namespace holding one authority record per workspace digest.
/// The controller and the daemon share it: a workspace has exactly one authority.
pub const WORKSPACE_AUTHORITY_NAMESPACE: &str = "workspace-authority";
const WORKSPACE_AUTHORITY_RECORD_STATE: &str = "active";
const WORKSPACE_AUTHORITY_SCHEMA: &str = "homeboy/workspace-authority/v2";
pub const MAX_WORKSPACE_CLAIM_TTL_MS: u64 = 300_000;

/// Token-free owner identity returned by authority inventory. The stable digest
/// lets callers distinguish live owners without disclosing owner identifiers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedactedWorkspaceOwnerIdentity {
    pub kind: String,
    pub redacted_id: String,
    pub lifecycle_revision: u64,
}

/// Read-only authority inventory for one exact workspace identity. This is safe
/// for transport responses: it deliberately contains neither claim nor lease
/// tokens (or expiry timestamps that could be used as authority receipts).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceAuthorityStatus {
    pub schema: String,
    pub protocol: WorkspaceClaimProtocol,
    pub workspace: WorkspaceIdentity,
    pub lifecycle_revision: u64,
    pub live_reconciliation_claim: bool,
    pub live_owner_count: usize,
    pub live_owners: Vec<RedactedWorkspaceOwnerIdentity>,
    pub clear: bool,
}

/// Token-free local owner details for operator diagnostics and write admission.
/// Unlike transport-safe authority status, this intentionally names owner IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceOwnerStatus {
    pub owner_id: String,
    pub lifecycle_revision: u64,
    pub expires_at_ms: u64,
}

pub const WORKSPACE_AUTHORITY_STATUS_SCHEMA: &str = "homeboy/workspace-authority-status/v2";

impl WorkspaceAuthorityStatus {
    pub fn verify(&self, workspace: &WorkspaceIdentity) -> Result<()> {
        self.protocol.verify()?;
        self.workspace.verify()?;
        (self.schema == WORKSPACE_AUTHORITY_STATUS_SCHEMA
            && &self.workspace == workspace
            && self.live_owner_count == self.live_owners.len()
            && self.clear == (!self.live_reconciliation_claim && self.live_owners.is_empty())
            && self.live_owners.iter().all(|owner| {
                owner.kind == "workspace-owner-sha256/v1"
                    && owner.lifecycle_revision > 0
                    && owner.redacted_id.len() == 64
                    && owner
                        .redacted_id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            }))
        .then_some(())
        .ok_or_else(|| {
            invalid(
                "workspace_authority_status",
                "workspace authority status is malformed",
            )
        })
    }
}

/// Durable evidence that a direct-owner rollback could not release its exact
/// lease. Daemon startup retries these records before accepting new work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceOwnerReleaseRecovery {
    pub schema: String,
    pub lease: WorkspaceOwnerLease,
    pub error_code: String,
    pub error_message: String,
}

/// The one durable authority record for a workspace, stored as the payload of
/// a `workspace-authority` coordination record keyed by the workspace digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct WorkspaceAuthority {
    schema: String,
    workspace: WorkspaceIdentity,
    lifecycle_revision: u64,
    owners: Vec<WorkspaceOwnerLease>,
    reconciliation: Option<Reconciliation>,
    #[serde(default)]
    owner_release_recoveries: Vec<WorkspaceOwnerReleaseRecovery>,
}

/// The single reconciliation claim and, while a destructive mutation runs, the
/// process fencing it. A set `fencing_owner` keeps the claim live past its
/// expiry; only clearing the fence or proving its owner dead releases it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Reconciliation {
    claim: WorkspaceClaim,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fencing_owner: Option<LeaseOwner>,
}

impl Reconciliation {
    fn is_live(&self, now_ms: u64) -> bool {
        match &self.fencing_owner {
            Some(owner) => !owner.is_proven_dead(),
            None => self.claim.expires_at_ms > now_ms,
        }
    }
}

impl WorkspaceAuthority {
    fn empty(workspace: WorkspaceIdentity) -> Self {
        Self {
            schema: WORKSPACE_AUTHORITY_SCHEMA.into(),
            workspace,
            lifecycle_revision: 0,
            owners: Vec::new(),
            reconciliation: None,
            owner_release_recoveries: Vec::new(),
        }
    }
    fn verify(&self) -> Result<()> {
        if self.schema != WORKSPACE_AUTHORITY_SCHEMA {
            return Err(invalid(
                "workspace_authority",
                "workspace authority state has an unsupported schema",
            ));
        }
        self.workspace.verify()?;
        for owner in &self.owners {
            owner.verify_shape(0)?;
            if owner.workspace != self.workspace {
                return Err(invalid(
                    "workspace_authority",
                    "owner workspace does not match authority workspace",
                ));
            }
        }
        if let Some(reconciliation) = &self.reconciliation {
            reconciliation.claim.verify_shape(0)?;
            if reconciliation.claim.workspace != self.workspace {
                return Err(invalid(
                    "workspace_authority",
                    "claim workspace does not match authority workspace",
                ));
            }
        }
        for recovery in &self.owner_release_recoveries {
            if recovery.schema != WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA
                || recovery.lease.workspace != self.workspace
            {
                return Err(invalid(
                    "workspace_owner_release_recovery",
                    "owner release recovery evidence is malformed or belongs to another workspace",
                ));
            }
            recovery.lease.verify_shape(0)?;
        }
        Ok(())
    }
    fn claim(&self) -> Option<&WorkspaceClaim> {
        self.reconciliation
            .as_ref()
            .map(|reconciliation| &reconciliation.claim)
    }
    /// Drop expired owners and a reconciliation claim that is no longer live.
    /// A fenced claim survives expiry until its fencing process is proven dead.
    fn prune(&mut self, now_ms: u64) {
        self.owners.retain(|owner| owner.expires_at_ms > now_ms);
        if self
            .reconciliation
            .as_ref()
            .is_some_and(|reconciliation| !reconciliation.is_live(now_ms))
        {
            self.reconciliation = None;
        }
    }
}

/// Workspace authority handle over the observation store.
///
/// Every workspace has exactly one authority record in the
/// [`WORKSPACE_AUTHORITY_NAMESPACE`] coordination namespace, shared by the
/// controller and the daemon. Each mutation is a revision compare-and-set, so
/// two writers can never both pass an admission check.
///
/// Cloning is a handle copy: the store owns no in-memory coordination state, so
/// every clone addresses the exact same database. That is what lets one
/// resolved authority be threaded into a renewal thread instead of each
/// heartbeat re-resolving an ambient root and silently renewing in a different
/// installation than the one that registered the lease.
#[derive(Clone)]
pub struct WorkspaceClaimStore {
    database: PathBuf,
}
impl WorkspaceClaimStore {
    /// Address the authority in the observation database at `database`.
    pub fn at(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
        }
    }

    /// Address the authority in the observation database of `data_root`.
    pub fn in_data_root(data_root: &Path) -> Self {
        Self::at(crate::paths::observation_db_in_root(data_root))
    }

    /// Address the authority in the ambient installation's observation store.
    pub fn from_environment() -> Result<Self> {
        Ok(Self::at(crate::paths::observation_db()?))
    }

    /// Atomically registers an active owner unless reconciliation is fenced.
    pub fn register_owner(
        &self,
        workspace: WorkspaceIdentity,
        owner_id: impl Into<String>,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<WorkspaceOwnerLease> {
        workspace.verify()?;
        let owner_id = owner_id.into();
        valid_ttl(ttl_ms)?;
        if owner_id.trim().is_empty() {
            return Err(invalid(
                "workspace_owner_id",
                "workspace owner identity must not be empty",
            ));
        }
        self.mutate(&workspace, |state| {
            state.prune(now_ms);
            if state.reconciliation.is_some() {
                return Err(invalid(
                    "workspace_reconciliation_fence",
                    "workspace is fenced by a live reconciliation claim",
                ));
            }
            if let Some(owner) = state.owners.iter().find(|owner| owner.owner_id == owner_id) {
                return Ok(owner.clone());
            }
            let lease = WorkspaceOwnerLease {
                schema: WORKSPACE_OWNER_LEASE_SCHEMA.into(),
                protocol: WorkspaceOwnerLeaseProtocol::current(),
                workspace: workspace.clone(),
                owner_id: owner_id.clone(),
                lifecycle_revision: next_revision(state)?,
                token: uuid::Uuid::new_v4().to_string(),
                expires_at_ms: now_ms.saturating_add(ttl_ms),
            };
            state.owners.push(lease.clone());
            Ok(lease)
        })
    }

    pub fn renew_owner(
        &self,
        lease: &WorkspaceOwnerLease,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<WorkspaceOwnerLease> {
        lease.verify_shape(0)?;
        valid_ttl(ttl_ms)?;
        self.mutate(&lease.workspace, |state| {
            state.prune(now_ms);
            if state.reconciliation.is_some() {
                return Err(invalid(
                    "workspace_reconciliation_fence",
                    "a live reconciliation fence cannot renew an owner",
                ));
            }
            let revision = next_revision(state)?;
            let owner = state
                .owners
                .iter_mut()
                .find(|owner| owner.owner_id == lease.owner_id)
                .ok_or_else(|| invalid("workspace_owner_lease", "owner lease is no longer live"))?;
            if owner.token != lease.token || owner.lifecycle_revision != lease.lifecycle_revision {
                return Err(invalid(
                    "workspace_owner_lease",
                    "owner lease token or epoch does not match durable authority",
                ));
            }
            owner.expires_at_ms = now_ms.saturating_add(ttl_ms);
            owner.lifecycle_revision = revision;
            Ok(owner.clone())
        })
    }

    /// Check the exact live owner token and epoch without extending its lease.
    pub fn validate_owner(&self, lease: &WorkspaceOwnerLease, now_ms: u64) -> Result<bool> {
        lease.verify_shape(now_ms)?;
        self.mutate(&lease.workspace, |state| {
            state.prune(now_ms);
            Ok(state.reconciliation.is_none() && state.owners.iter().any(|owner| owner == lease))
        })
    }

    pub fn release_owner(&self, lease: &WorkspaceOwnerLease, now_ms: u64) -> Result<()> {
        lease.workspace.verify()?;
        #[cfg(any(test, feature = "test-support"))]
        inject_owner_release_failure(&lease.workspace)?;
        self.mutate(&lease.workspace, |state| {
            state.prune(now_ms);
            match state
                .owners
                .iter()
                .position(|owner| owner.owner_id == lease.owner_id)
            {
                // Recovery can replay after a prior release succeeded but its
                // caller could not durably record the terminal state. The
                // absent owner proves the exact cleanup is complete.
                None => {
                    state
                        .owner_release_recoveries
                        .retain(|recovery| recovery.lease != *lease);
                    Ok(())
                }
                Some(index)
                    if state.owners[index].token == lease.token
                        && state.owners[index].lifecycle_revision == lease.lifecycle_revision =>
                {
                    state.owners.remove(index);
                    state
                        .owner_release_recoveries
                        .retain(|recovery| recovery.lease != *lease);
                    Ok(())
                }
                Some(_) => Err(invalid(
                    "workspace_owner_lease",
                    "owner lease token or epoch does not match durable authority",
                )),
            }
        })
    }

    /// Persist the exact lease and release failure so a later daemon startup can
    /// retry rollback even though no job or admission was committed.
    pub fn record_owner_release_failure(
        &self,
        lease: &WorkspaceOwnerLease,
        error: &Error,
    ) -> Result<WorkspaceOwnerReleaseRecovery> {
        lease.verify_shape(0)?;
        let recovery = WorkspaceOwnerReleaseRecovery {
            schema: WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA.into(),
            lease: lease.clone(),
            error_code: error.code.as_str().to_string(),
            error_message: error.to_string(),
        };
        self.mutate(&lease.workspace, |state| {
            state
                .owner_release_recoveries
                .retain(|existing| existing.lease != *lease);
            state.owner_release_recoveries.push(recovery.clone());
            Ok(())
        })?;
        Ok(recovery)
    }

    /// Return durable rollback work that was left pending by a failed admission
    /// or queue commit. A successful `release_owner` removes the matching record.
    pub fn pending_owner_release_recoveries(&self) -> Result<Vec<WorkspaceOwnerReleaseRecovery>> {
        let mut recoveries = Vec::new();
        for record in self
            .open()?
            .coordination_records(WORKSPACE_AUTHORITY_NAMESPACE)?
        {
            recoveries.extend(parse_authority(&record)?.owner_release_recoveries);
        }
        Ok(recoveries)
    }

    /// Make the next `count` owner releases of `workspace` on this thread fail.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_next_owner_releases(
        &self,
        workspace: &WorkspaceIdentity,
        count: u64,
    ) -> Result<()> {
        workspace.verify()?;
        OWNER_RELEASE_FAILURES.with(|failures| {
            failures
                .borrow_mut()
                .insert(workspace_key(workspace), count)
        });
        Ok(())
    }

    /// Acquire reconciliation only after atomically pruning expired owners and
    /// proving no active owner remains.
    pub fn acquire(
        &self,
        workspace: WorkspaceIdentity,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<WorkspaceClaim> {
        workspace.verify()?;
        valid_ttl(ttl_ms)?;
        self.mutate(&workspace, |state| {
            state.prune(now_ms);
            if !state.owners.is_empty() {
                return Err(live_owners(&state.owners));
            }
            if state.reconciliation.is_some() {
                return Err(invalid(
                    "workspace_claim",
                    "workspace already has a live reconciliation claim",
                ));
            }
            let claim = WorkspaceClaim {
                schema: WORKSPACE_CLAIM_SCHEMA.into(),
                protocol: WorkspaceClaimProtocol::current(),
                workspace: workspace.clone(),
                lifecycle_revision: next_revision(state)?,
                token: uuid::Uuid::new_v4().to_string(),
                expires_at_ms: now_ms.saturating_add(ttl_ms),
            };
            state.reconciliation = Some(Reconciliation {
                claim: claim.clone(),
                fencing_owner: None,
            });
            Ok(claim)
        })
    }

    pub fn validate(&self, claim: &WorkspaceClaim, now_ms: u64) -> Result<bool> {
        claim.verify_shape(now_ms)?;
        Ok(self.read(&claim.workspace)?.0.claim() == Some(claim))
    }

    /// Run a destructive reconciliation mutation inside the durable fence.
    ///
    /// Entering records this process as `fencing_owner` on the claim; leaving
    /// clears the fence together with the claim. Both are revision
    /// compare-and-set writes on the same record every owner registration
    /// goes through, so no owner can be admitted while `operation` runs, even
    /// after the claim's expiry. A fence whose process died is released by the
    /// next authority write that proves the death through process identity.
    pub fn with_reconciliation_fence<T>(
        &self,
        claim: &WorkspaceClaim,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        claim.workspace.verify()?;
        let owner = LeaseOwner::current_process();
        self.mutate(&claim.workspace, |state| match &mut state.reconciliation {
            Some(reconciliation)
                if reconciliation.claim == *claim && reconciliation.fencing_owner.is_none() =>
            {
                reconciliation.fencing_owner = Some(owner.clone());
                Ok(())
            }
            Some(reconciliation) if reconciliation.claim == *claim => Err(invalid(
                "workspace_claim",
                "reconciliation claim is already fenced",
            )),
            _ => Err(invalid(
                "workspace_claim",
                "reconciliation claim no longer matches durable authority",
            )),
        })?;
        let exit = FenceExit { store: self, claim };
        let result = operation();
        let exited = exit.exit();
        let value = result?;
        exited?;
        Ok(value)
    }

    /// Return whether this authority still has a live reconciliation claim or
    /// owner lease. Recovery uses this token-free inventory only after the
    /// maximum claim lifetime has elapsed.
    pub fn has_live_authority(&self, workspace: &WorkspaceIdentity, now_ms: u64) -> Result<bool> {
        workspace.verify()?;
        self.mutate(workspace, |state| {
            state.prune(now_ms);
            Ok(state.reconciliation.is_some() || !state.owners.is_empty())
        })
    }

    /// Return a token-free, exact-identity inventory after pruning expiry.
    /// Pruning is the only mutation this probe may do.
    pub fn authority_status(
        &self,
        workspace: &WorkspaceIdentity,
        now_ms: u64,
    ) -> Result<WorkspaceAuthorityStatus> {
        workspace.verify()?;
        let ((live_owners, live_reconciliation_claim), state) =
            self.mutate_with_state(workspace, |state| {
                state.prune(now_ms);
                let live_owners = state
                    .owners
                    .iter()
                    .map(|owner| RedactedWorkspaceOwnerIdentity {
                        kind: "workspace-owner-sha256/v1".into(),
                        redacted_id: sha256_hex(&owner.owner_id),
                        lifecycle_revision: owner.lifecycle_revision,
                    })
                    .collect::<Vec<_>>();
                Ok((live_owners, state.reconciliation.is_some()))
            })?;
        Ok(WorkspaceAuthorityStatus {
            schema: WORKSPACE_AUTHORITY_STATUS_SCHEMA.into(),
            protocol: WorkspaceClaimProtocol::current(),
            workspace: workspace.clone(),
            lifecycle_revision: state.lifecycle_revision,
            live_reconciliation_claim,
            live_owner_count: live_owners.len(),
            clear: !live_reconciliation_claim && live_owners.is_empty(),
            live_owners,
        })
    }

    /// Durably release every expired owner lease from one authority and report
    /// exactly which owner ids were dropped, so reclamation can account for
    /// stale registrations it released instead of leaving them to a probe.
    /// Live owners and a live reconciliation claim are never touched.
    pub fn prune_expired_owner_leases(
        &self,
        workspace: &WorkspaceIdentity,
        now_ms: u64,
    ) -> Result<Option<Vec<String>>> {
        workspace.verify()?;
        self.mutate(workspace, |state| {
            let before = state.clone();
            let expired = state
                .owners
                .iter()
                .filter(|owner| owner.expires_at_ms <= now_ms)
                .map(|owner| owner.owner_id.clone())
                .collect::<Vec<_>>();
            state.prune(now_ms);
            if *state == before {
                return Ok(None);
            }
            Ok(Some(expired).filter(|expired| !expired.is_empty()))
        })
    }

    /// Return exact live owner identities after pruning expiry. This
    /// local-only probe never exposes authority tokens.
    pub fn owner_status(
        &self,
        workspace: &WorkspaceIdentity,
        now_ms: u64,
    ) -> Result<Vec<WorkspaceOwnerStatus>> {
        workspace.verify()?;
        self.mutate(workspace, |state| {
            state.prune(now_ms);
            Ok(state
                .owners
                .iter()
                .map(|owner| WorkspaceOwnerStatus {
                    owner_id: owner.owner_id.clone(),
                    lifecycle_revision: owner.lifecycle_revision,
                    expires_at_ms: owner.expires_at_ms,
                })
                .collect())
        })
    }

    /// Exact reconciliation release is idempotent; a different live token fails.
    pub fn release(&self, claim: &WorkspaceClaim, now_ms: u64) -> Result<()> {
        claim.workspace.verify()?;
        self.mutate(&claim.workspace, |state| {
            state.prune(now_ms);
            match state.claim() {
                None => Ok(()),
                Some(current) if current == claim => {
                    state.reconciliation = None;
                    Ok(())
                }
                Some(_) => Err(invalid(
                    "workspace_claim",
                    "reconciliation claim token or epoch does not match durable authority",
                )),
            }
        })
    }

    /// Atomically authorizes only the exact live reconciliation operation.
    pub fn authorize_binding(&self, binding: &WorkspaceClaimBinding, now_ms: u64) -> Result<()> {
        binding.verify()?;
        match self.read(&binding.workspace)?.0.reconciliation {
            None => Ok(()),
            Some(reconciliation) if !reconciliation.is_live(now_ms) => Ok(()),
            Some(reconciliation) if binding.claim.as_ref() == Some(&reconciliation.claim) => Ok(()),
            Some(_) => Err(invalid(
                "workspace_claim",
                "workspace is fenced by a live reconciliation claim",
            )),
        }
    }

    fn open(&self) -> Result<ObservationStore> {
        ObservationStore::open_initialized_for_lifecycle_at(&self.database)
    }

    /// The stored authority and the revision a compare-and-set must expect.
    fn read(
        &self,
        workspace: &WorkspaceIdentity,
    ) -> Result<(WorkspaceAuthority, ExpectedRevision)> {
        read_in(&self.open()?, workspace)
    }

    /// Run `change` against the current authority and, when it changed the
    /// state, commit it with a revision compare-and-set. A lost race rereads
    /// and reruns `change`, so every decision is made against committed state.
    /// An error from `change` writes nothing.
    fn mutate<T>(
        &self,
        workspace: &WorkspaceIdentity,
        change: impl FnMut(&mut WorkspaceAuthority) -> Result<T>,
    ) -> Result<T> {
        Ok(self.mutate_with_state(workspace, change)?.0)
    }

    /// [`Self::mutate`], also returning the committed authority state.
    fn mutate_with_state<T>(
        &self,
        workspace: &WorkspaceIdentity,
        mut change: impl FnMut(&mut WorkspaceAuthority) -> Result<T>,
    ) -> Result<(T, WorkspaceAuthority)> {
        let store = self.open()?;
        let key = workspace_key(workspace);
        loop {
            let (current, expected) = read_in(&store, workspace)?;
            let mut state = current.clone();
            let value = change(&mut state)?;
            if state == current {
                return Ok((value, state));
            }
            state.lifecycle_revision = next_revision(&current)?;
            let payload = serde_json::to_value(&state)
                .map_err(|error| Error::internal_json(error.to_string(), None))?;
            match store.put_coordination_record(
                WORKSPACE_AUTHORITY_NAMESPACE,
                &key,
                expected,
                RecordWrite {
                    state: WORKSPACE_AUTHORITY_RECORD_STATE,
                    payload: &payload,
                    expires_at_ms: None,
                },
            )? {
                RecordOutcome::Applied(_) => return Ok((value, state)),
                RecordOutcome::Conflict { .. } => continue,
            }
        }
    }
}

/// Leaves the reconciliation fence exactly once, including when the fenced
/// operation panics, by clearing the fence together with its claim.
struct FenceExit<'a> {
    store: &'a WorkspaceClaimStore,
    claim: &'a WorkspaceClaim,
}

impl FenceExit<'_> {
    fn exit(self) -> Result<()> {
        let result = self.clear();
        std::mem::forget(self);
        result
    }

    fn clear(&self) -> Result<()> {
        self.store.mutate(&self.claim.workspace, |state| {
            if state.claim() == Some(self.claim) {
                state.reconciliation = None;
            }
            Ok(())
        })
    }
}

impl Drop for FenceExit<'_> {
    fn drop(&mut self) {
        let _ = self.clear();
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static OWNER_RELEASE_FAILURES: std::cell::RefCell<std::collections::HashMap<String, u64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(any(test, feature = "test-support"))]
fn inject_owner_release_failure(workspace: &WorkspaceIdentity) -> Result<()> {
    let key = workspace_key(workspace);
    let injected = OWNER_RELEASE_FAILURES.with(|failures| {
        let mut failures = failures.borrow_mut();
        match failures.get_mut(&key) {
            Some(remaining) if *remaining > 0 => {
                *remaining -= 1;
                if *remaining == 0 {
                    failures.remove(&key);
                }
                true
            }
            _ => false,
        }
    });
    if injected {
        return Err(Error::internal_io(
            "injected workspace owner release failure",
            None,
        ));
    }
    Ok(())
}

fn read_in(
    store: &ObservationStore,
    workspace: &WorkspaceIdentity,
) -> Result<(WorkspaceAuthority, ExpectedRevision)> {
    let Some(record) =
        store.coordination_record(WORKSPACE_AUTHORITY_NAMESPACE, &workspace_key(workspace))?
    else {
        return Ok((
            WorkspaceAuthority::empty(workspace.clone()),
            ExpectedRevision::Absent,
        ));
    };
    let state = parse_authority(&record)?;
    if state.workspace != *workspace {
        return Err(invalid(
            "workspace_authority",
            "authority state does not match requested workspace",
        ));
    }
    Ok((state, ExpectedRevision::Revision(record.revision)))
}

fn parse_authority(record: &CoordinationRecord) -> Result<WorkspaceAuthority> {
    let state: WorkspaceAuthority =
        serde_json::from_value(record.payload.clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("{}/{}", record.namespace, record.key)),
            )
        })?;
    state.verify()?;
    Ok(state)
}

/// The lifecycle revision the next committed change of `state` will carry.
fn next_revision(state: &WorkspaceAuthority) -> Result<u64> {
    state
        .lifecycle_revision
        .checked_add(1)
        .ok_or_else(|| invalid("workspace_authority", "authority epoch overflow"))
}

fn valid_ttl(ttl_ms: u64) -> Result<()> {
    (ttl_ms > 0 && ttl_ms <= MAX_WORKSPACE_CLAIM_TTL_MS)
        .then_some(())
        .ok_or_else(|| {
            invalid(
                "workspace_claim_ttl_ms",
                "workspace authority TTL must be positive and bounded",
            )
        })
}
fn live_owners(owners: &[WorkspaceOwnerLease]) -> Error {
    Error::validation_invalid_argument(
        "workspace_live_owners",
        "workspace has live owner leases",
        None,
        Some(
            owners
                .iter()
                .map(|owner| format!("{}@{}", owner.owner_id, owner.lifecycle_revision))
                .collect(),
        ),
    )
}
fn workspace_key(workspace: &WorkspaceIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace.schema.as_bytes());
    hasher.update([0]);
    hasher.update(workspace.kind.as_bytes());
    hasher.update([0]);
    hasher.update(workspace.locator.as_bytes());
    format!("{:x}", hasher.finalize())
}
fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}
fn invalid(field: &str, message: &str) -> Error {
    Error::validation_invalid_argument(field, message, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> WorkspaceIdentity {
        WorkspaceIdentity::new("managed-workspace", "repo@task").unwrap()
    }
    fn store() -> (tempfile::TempDir, WorkspaceClaimStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceClaimStore::in_data_root(dir.path());
        (dir, store)
    }

    /// The current PID with a shifted start identity: a reused PID, which
    /// process-identity liveness proves dead on this host.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn dead_owner() -> LeaseOwner {
        use crate::process::ProcessStartIdentity;
        let live = LeaseOwner::current_process();
        let start_identity = match live.start_identity.clone().expect("start identity") {
            ProcessStartIdentity::Linux { starttime_ticks } => ProcessStartIdentity::Linux {
                starttime_ticks: starttime_ticks.wrapping_add(1),
            },
            ProcessStartIdentity::Macos {
                start_seconds,
                start_microseconds,
            } => ProcessStartIdentity::Macos {
                start_seconds: start_seconds.wrapping_add(1),
                start_microseconds,
            },
        };
        let dead = LeaseOwner {
            start_identity: Some(start_identity),
            ..live
        };
        assert!(dead.is_proven_dead());
        dead
    }

    #[test]
    fn owners_block_reconciliation_and_can_coexist() {
        let (_dir, store) = store();
        let one = store
            .register_owner(identity(), "local:one", 100, 10)
            .unwrap();
        let two = store
            .register_owner(identity(), "remote:two", 100, 10)
            .unwrap();
        assert_ne!(one.token, two.token);
        assert!(store
            .acquire(identity(), 10, 11)
            .unwrap_err()
            .message
            .contains("live owner leases"));
    }
    #[test]
    fn claim_blocks_owner_registration_and_release_is_idempotent() {
        let (_dir, store) = store();
        let claim = store.acquire(identity(), 100, 10).unwrap();
        assert!(store.register_owner(identity(), "owner", 10, 11).is_err());
        store.release(&claim, 11).unwrap();
        store.release(&claim, 11).unwrap();
        assert!(store.register_owner(identity(), "owner", 10, 11).is_ok());
    }
    #[test]
    fn renewal_keeps_owner_live_beyond_five_minutes() {
        let (_dir, store) = store();
        let mut owner = store
            .register_owner(identity(), "owner", MAX_WORKSPACE_CLAIM_TTL_MS, 0)
            .unwrap();
        for now in [299_999, 599_998, 899_997] {
            owner = store
                .renew_owner(&owner, MAX_WORKSPACE_CLAIM_TTL_MS, now)
                .unwrap();
        }
        assert!(store.acquire(identity(), 10, 900_000).is_err());
    }
    #[test]
    fn expired_owners_are_pruned_before_reconciliation() {
        let (_dir, store) = store();
        store.register_owner(identity(), "owner", 10, 0).unwrap();
        assert!(store.acquire(identity(), 10, 11).is_ok());
    }
    #[test]
    fn owner_validation_requires_the_exact_token_and_epoch() {
        let (_dir, store) = store();
        let lease = store.register_owner(identity(), "owner", 100, 0).unwrap();
        assert!(store.validate_owner(&lease, 1).unwrap());
        let mut substituted = lease.clone();
        substituted.token = "other".into();
        assert!(!store.validate_owner(&substituted, 1).unwrap());
    }
    #[test]
    fn epochs_are_monotonic_across_expiry_reacquire_and_restart() {
        let (dir, store) = store();
        let first = store.acquire(identity(), 10, 0).unwrap();
        let second = store.acquire(identity(), 10, 11).unwrap();
        let restarted = WorkspaceClaimStore::in_data_root(dir.path());
        restarted.release(&second, 12).unwrap();
        let third = restarted.acquire(identity(), 10, 13).unwrap();
        assert!(
            first.lifecycle_revision < second.lifecycle_revision
                && second.lifecycle_revision < third.lifecycle_revision
        );
    }
    #[test]
    fn stale_epoch_and_token_substitution_are_rejected() {
        let (_dir, store) = store();
        let stale = store.acquire(identity(), 100, 0).unwrap();
        store.release(&stale, 1).unwrap();
        let claim = store.acquire(identity(), 100, 2).unwrap();
        assert!(!store.validate(&stale, 1).unwrap());
        assert!(store.release(&stale, 1).is_err());
        let mut forged = claim.clone();
        forged.token = "other".into();
        assert!(!store.validate(&forged, 1).unwrap());
    }
    #[test]
    fn registration_and_reconciliation_race_have_one_winner() {
        use std::sync::{Arc, Barrier};
        let (_dir, store) = store();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(2));
        let owner_store = store.clone();
        let owner_barrier = barrier.clone();
        let owner = std::thread::spawn(move || {
            owner_barrier.wait();
            owner_store
                .register_owner(identity(), "owner", 100, 1)
                .is_ok()
        });
        barrier.wait();
        let claim = store.acquire(identity(), 100, 1).is_ok();
        assert_ne!(owner.join().unwrap(), claim);
    }
    #[test]
    fn durable_write_leaves_restartable_state() {
        let (dir, store) = store();
        let claim = store.acquire(identity(), 100, 1).unwrap();
        drop(store);
        let restarted = WorkspaceClaimStore::in_data_root(dir.path());
        assert!(restarted.validate(&claim, 2).unwrap());
    }
    #[test]
    fn authority_status_is_token_free_and_prunes_expiry_under_lock() {
        let (_dir, store) = store();
        let owner = store.register_owner(identity(), "owner", 10, 0).unwrap();
        let status = store.authority_status(&identity(), 1).unwrap();
        assert!(!status.clear);
        assert_eq!(status.live_owner_count, 1);
        assert_ne!(status.live_owners[0].redacted_id, owner.owner_id);
        assert!(!serde_json::to_string(&status)
            .unwrap()
            .contains(&owner.token));
        let expired = store.authority_status(&identity(), 11).unwrap();
        assert!(expired.clear);
        assert_eq!(expired.live_owner_count, 0);
    }
    #[test]
    fn authority_status_reports_a_live_reconciliation_claim() {
        let (_dir, store) = store();
        let claim = store.acquire(identity(), 10, 0).unwrap();
        let status = store.authority_status(&identity(), 1).unwrap();
        assert!(status.live_reconciliation_claim);
        assert!(!status.clear);
        assert!(!serde_json::to_string(&status)
            .unwrap()
            .contains(&claim.token));
    }

    #[test]
    fn concurrent_acquires_have_exactly_one_winner() {
        use std::sync::{Arc, Barrier};
        const CLAIMANTS: usize = 6;
        let (dir, _store) = store();
        let barrier = Arc::new(Barrier::new(CLAIMANTS));
        let threads: Vec<_> = (0..CLAIMANTS)
            .map(|_| {
                let store = WorkspaceClaimStore::in_data_root(dir.path());
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.acquire(identity(), 100, 1).is_ok()
                })
            })
            .collect();
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
    }

    #[test]
    fn the_fence_holds_an_expired_claim_until_it_exits() {
        let (_dir, store) = store();
        let claim = store.acquire(identity(), 10, 0).unwrap();
        let (admitted_inside, clear_inside) = store
            .with_reconciliation_fence(&claim, || {
                // Far past the claim's expiry: the fence, not the clock, decides.
                Ok((
                    store.register_owner(identity(), "owner", 10, 1_000).is_ok(),
                    store.authority_status(&identity(), 1_000)?.clear,
                ))
            })
            .unwrap();
        assert!(
            !admitted_inside,
            "an expired fenced claim admitted an owner"
        );
        assert!(!clear_inside);
        // Leaving the fence clears it with the claim; the caller's release is
        // an idempotent no-op afterwards.
        store.release(&claim, 1_000).unwrap();
        assert!(!store.validate(&claim, 0).unwrap());
        assert!(store.register_owner(identity(), "owner", 10, 1_000).is_ok());
    }

    #[test]
    fn a_fence_cannot_be_entered_twice_or_with_a_stale_claim() {
        let (_dir, store) = store();
        let claim = store.acquire(identity(), 100, 0).unwrap();
        let mut forged = claim.clone();
        forged.token = "other".into();
        assert!(store.with_reconciliation_fence(&forged, || Ok(())).is_err());
        let nested = store
            .with_reconciliation_fence(&claim, || {
                Ok(store.with_reconciliation_fence(&claim, || Ok(())).is_err())
            })
            .unwrap();
        assert!(nested);
    }

    #[test]
    fn a_failed_or_panicking_fenced_operation_still_exits_the_fence() {
        let (_dir, store) = store();
        let claim = store.acquire(identity(), 100, 0).unwrap();
        assert!(store
            .with_reconciliation_fence(&claim, || -> Result<()> {
                Err(invalid("test", "operation failed"))
            })
            .is_err());
        assert!(store.register_owner(identity(), "owner", 10, 1).is_ok());
        store
            .release_owner(
                &store.register_owner(identity(), "owner", 10, 1).unwrap(),
                1,
            )
            .unwrap();

        let claim = store.acquire(identity(), 100, 2).unwrap();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = store.with_reconciliation_fence(&claim, || -> Result<()> {
                panic!("fenced operation panicked")
            });
        }));
        assert!(panicked.is_err());
        assert!(store.register_owner(identity(), "owner", 10, 3).is_ok());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_fence_held_by_a_dead_process_is_reaped_and_admits_a_new_owner() {
        let (_dir, store) = store();
        let claim = store
            .acquire(identity(), MAX_WORKSPACE_CLAIM_TTL_MS, 0)
            .unwrap();
        // Simulate a process that entered the fence and then crashed.
        store
            .mutate(&identity(), |state| {
                state.reconciliation.as_mut().unwrap().fencing_owner = Some(dead_owner());
                Ok(())
            })
            .unwrap();
        // The claim itself is far from expiry; the dead fencing owner alone
        // releases it.
        assert!(store
            .authorize_binding(
                &WorkspaceClaimBinding {
                    workspace: identity(),
                    lifecycle_revision: claim.lifecycle_revision,
                    claim: None,
                },
                1,
            )
            .is_ok());
        let owner = store.register_owner(identity(), "owner", 10, 1).unwrap();
        assert!(owner.lifecycle_revision > claim.lifecycle_revision);
        assert!(!store.validate(&claim, 0).unwrap());
    }

    #[test]
    fn a_live_fence_refuses_owners_and_acquires_from_every_handle() {
        let (dir, store) = store();
        let claim = store.acquire(identity(), 10, 0).unwrap();
        store
            .with_reconciliation_fence(&claim, || {
                // A second handle on the same database, as the daemon holds.
                let other = WorkspaceClaimStore::in_data_root(dir.path());
                assert!(other
                    .register_owner(identity(), "owner", 10, 1_000)
                    .is_err());
                assert!(other.acquire(identity(), 10, 1_000).is_err());
                assert!(other.with_reconciliation_fence(&claim, || Ok(())).is_err());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn controller_and_daemon_handles_share_one_authority() {
        let (dir, controller) = store();
        let daemon = WorkspaceClaimStore::at(crate::paths::observation_db_in_root(dir.path()));
        let owner = daemon
            .register_owner(identity(), "daemon-owner", 100, 0)
            .unwrap();
        assert!(controller.validate_owner(&owner, 1).unwrap());
        assert!(controller.acquire(identity(), 10, 1).is_err());
        // A separate installation is a separate authority.
        let (_other_dir, other) = store();
        assert!(other.acquire(identity(), 10, 1).is_ok());
    }

    #[test]
    fn owner_release_recoveries_survive_reopen_and_clear_on_release() {
        let (dir, store) = store();
        let lease = store.register_owner(identity(), "owner", 100, 0).unwrap();
        store.fail_next_owner_releases(&identity(), 1).unwrap();
        let error = store.release_owner(&lease, 1).unwrap_err();
        store.record_owner_release_failure(&lease, &error).unwrap();
        let reopened = WorkspaceClaimStore::in_data_root(dir.path());
        let recoveries = reopened.pending_owner_release_recoveries().unwrap();
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].lease, lease);
        reopened.release_owner(&lease, 1).unwrap();
        assert!(reopened
            .pending_owner_release_recoveries()
            .unwrap()
            .is_empty());
    }
}
