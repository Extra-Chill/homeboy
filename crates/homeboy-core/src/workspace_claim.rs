//! Durable, transport-neutral workspace authority.
//!
//! A workspace has either renewable owners or one short reconciliation fence.
//! The durable revision and opaque tokens, rather than clocks, establish authority.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

pub use homeboy_runner_contract::{
    WorkspaceClaim, WorkspaceClaimBinding, WorkspaceClaimProtocol, WorkspaceIdentity,
    WorkspaceOwnerLease, WorkspaceOwnerLeaseProtocol, WORKSPACE_CLAIM_CAPABILITY,
    WORKSPACE_CLAIM_PROTOCOL_VERSION, WORKSPACE_CLAIM_SCHEMA, WORKSPACE_IDENTITY_SCHEMA,
    WORKSPACE_OWNER_LEASE_CAPABILITY, WORKSPACE_OWNER_LEASE_SCHEMA,
};
pub const WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA: &str =
    "homeboy/workspace-owner-release-recovery/v1";
pub const LOCAL_WORKSPACE_CLAIMS_DIR: &str = "agent-task-workspace-claims";
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceAuthority {
    schema: String,
    workspace: WorkspaceIdentity,
    lifecycle_revision: u64,
    owners: Vec<WorkspaceOwnerLease>,
    reconciliation: Option<WorkspaceClaim>,
    #[serde(default)]
    owner_release_recoveries: Vec<WorkspaceOwnerReleaseRecovery>,
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
        if let Some(claim) = &self.reconciliation {
            claim.verify_shape(0)?;
            if claim.workspace != self.workspace {
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
    fn prune(&mut self, now_ms: u64) -> bool {
        let before = self.owners.len();
        self.owners.retain(|owner| owner.expires_at_ms > now_ms);
        if self
            .reconciliation
            .as_ref()
            .is_some_and(|claim| claim.expires_at_ms <= now_ms)
        {
            self.reconciliation = None;
            return true;
        }
        before != self.owners.len()
    }
}

/// One durable state file and lock per normalized portable workspace identity.
///
/// Cloning is a handle copy: the store owns no in-memory coordination state, so
/// every clone addresses the exact same durable root. That is what lets one
/// resolved authority be threaded into a renewal thread instead of each
/// heartbeat re-resolving an ambient root and silently renewing in a different
/// installation than the one that registered the lease.
#[derive(Clone)]
pub struct WorkspaceClaimStore {
    root: PathBuf,
}
impl WorkspaceClaimStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
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
        self.with_lock(&workspace, || {
            let mut state = self.read_or_empty(&workspace)?;
            let mut changed = state.prune(now_ms);
            if state.reconciliation.is_some() {
                if changed {
                    self.commit(&mut state)?;
                }
                return Err(invalid(
                    "workspace_reconciliation_fence",
                    "workspace is fenced by a live reconciliation claim",
                ));
            }
            if let Some(owner) = state
                .owners
                .iter()
                .find(|owner| owner.owner_id == owner_id)
                .cloned()
            {
                if changed {
                    self.commit(&mut state)?;
                }
                return Ok(owner);
            }
            let lease = WorkspaceOwnerLease {
                schema: WORKSPACE_OWNER_LEASE_SCHEMA.into(),
                protocol: WorkspaceOwnerLeaseProtocol::current(),
                workspace: workspace.clone(),
                owner_id,
                lifecycle_revision: state.lifecycle_revision + 1,
                token: uuid::Uuid::new_v4().to_string(),
                expires_at_ms: now_ms.saturating_add(ttl_ms),
            };
            state.owners.push(lease.clone());
            changed = true;
            if changed {
                self.commit(&mut state)?;
            }
            let mut issued = lease;
            issued.lifecycle_revision = state.lifecycle_revision;
            Ok(issued)
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
        self.with_lock(&lease.workspace, || {
            let mut state = self.read_or_empty(&lease.workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
            if state.reconciliation.is_some() {
                return Err(invalid(
                    "workspace_reconciliation_fence",
                    "a live reconciliation fence cannot renew an owner",
                ));
            }
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
            owner.lifecycle_revision = state.lifecycle_revision + 1;
            self.commit(&mut state)?;
            Ok(state
                .owners
                .iter()
                .find(|owner| owner.owner_id == lease.owner_id)
                .unwrap()
                .clone())
        })
    }

    /// Check the exact live owner token and epoch without extending its lease.
    pub fn validate_owner(&self, lease: &WorkspaceOwnerLease, now_ms: u64) -> Result<bool> {
        lease.verify_shape(now_ms)?;
        self.with_lock(&lease.workspace, || {
            let mut state = self.read_or_empty(&lease.workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
            Ok(state.reconciliation.is_none() && state.owners.iter().any(|owner| owner == lease))
        })
    }

    pub fn release_owner(&self, lease: &WorkspaceOwnerLease, now_ms: u64) -> Result<()> {
        lease.workspace.verify()?;
        self.with_lock(&lease.workspace, || {
            #[cfg(any(test, feature = "test-support"))]
            self.inject_owner_release_failure(&lease.workspace)?;
            let mut state = self.read_or_empty(&lease.workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
            match state
                .owners
                .iter()
                .position(|owner| owner.owner_id == lease.owner_id)
            {
                None => {
                    // Recovery can replay after a prior release succeeded but
                    // its caller could not durably record the terminal state.
                    // The absent owner proves the exact cleanup is complete.
                    let recoveries = state.owner_release_recoveries.len();
                    state
                        .owner_release_recoveries
                        .retain(|recovery| recovery.lease != *lease);
                    if state.owner_release_recoveries.len() != recoveries {
                        self.commit(&mut state)?;
                    }
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
                    self.commit(&mut state)
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
        self.with_lock(&lease.workspace, || {
            let mut state = self.read_or_empty(&lease.workspace)?;
            let recovery = WorkspaceOwnerReleaseRecovery {
                schema: WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA.into(),
                lease: lease.clone(),
                error_code: error.code.as_str().to_string(),
                error_message: error.to_string(),
            };
            state
                .owner_release_recoveries
                .retain(|existing| existing.lease != *lease);
            state.owner_release_recoveries.push(recovery.clone());
            self.commit(&mut state)?;
            Ok(recovery)
        })
    }

    /// Return durable rollback work that was left pending by a failed admission
    /// or queue commit. A successful `release_owner` removes the matching record.
    pub fn pending_owner_release_recoveries(&self) -> Result<Vec<WorkspaceOwnerReleaseRecovery>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut recoveries = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let state: WorkspaceAuthority =
                serde_json::from_slice(&fs::read(&path).map_err(io_error)?).map_err(|error| {
                    Error::internal_json(error.to_string(), Some(path.display().to_string()))
                })?;
            state.verify()?;
            recoveries.extend(state.owner_release_recoveries);
        }
        Ok(recoveries)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_next_owner_releases(
        &self,
        workspace: &WorkspaceIdentity,
        count: u64,
    ) -> Result<()> {
        workspace.verify()?;
        crate::engine::local_files::create_dir_all_durably(&self.root)?;
        fs::write(
            self.owner_release_failure_path(workspace),
            count.to_string(),
        )
        .map_err(io_error)
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
        self.with_lock(&workspace, || {
            let mut state = self.read_or_empty(&workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
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
                lifecycle_revision: state.lifecycle_revision + 1,
                token: uuid::Uuid::new_v4().to_string(),
                expires_at_ms: now_ms.saturating_add(ttl_ms),
            };
            state.reconciliation = Some(claim);
            self.commit(&mut state)?;
            Ok(state.reconciliation.unwrap())
        })
    }

    pub fn validate(&self, claim: &WorkspaceClaim, now_ms: u64) -> Result<bool> {
        claim.verify_shape(now_ms)?;
        self.with_lock(&claim.workspace, || {
            Ok(self
                .read_or_empty(&claim.workspace)?
                .reconciliation
                .as_ref()
                == Some(claim))
        })
    }

    /// Hold the workspace authority lock while a destructive reconciliation
    /// mutation runs. Expiry cannot admit a new owner until this closure exits.
    pub fn with_reconciliation_fence<T>(
        &self,
        claim: &WorkspaceClaim,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        claim.workspace.verify()?;
        self.with_lock(&claim.workspace, || {
            let state = self.read_or_empty(&claim.workspace)?;
            if state.reconciliation.as_ref() != Some(claim) {
                return Err(invalid(
                    "workspace_claim",
                    "reconciliation claim no longer matches durable authority",
                ));
            }
            operation()
        })
    }
    /// Return whether this authority still has a live reconciliation claim or
    /// owner lease. Recovery uses this token-free inventory only after the
    /// maximum claim lifetime has elapsed.
    pub fn has_live_authority(&self, workspace: &WorkspaceIdentity, now_ms: u64) -> Result<bool> {
        workspace.verify()?;
        self.with_lock(workspace, || {
            let mut state = self.read_or_empty(workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
            Ok(state.reconciliation.is_some() || !state.owners.is_empty())
        })
    }
    /// Return a token-free, exact-identity inventory after pruning expiry while
    /// holding the workspace lock. Pruning is the only mutation this probe may do.
    pub fn authority_status(
        &self,
        workspace: &WorkspaceIdentity,
        now_ms: u64,
    ) -> Result<WorkspaceAuthorityStatus> {
        workspace.verify()?;
        self.with_lock(workspace, || {
            let mut state = self.read_or_empty(workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
            let live_owners = state
                .owners
                .iter()
                .map(|owner| RedactedWorkspaceOwnerIdentity {
                    kind: "workspace-owner-sha256/v1".into(),
                    redacted_id: sha256_hex(&owner.owner_id),
                    lifecycle_revision: owner.lifecycle_revision,
                })
                .collect::<Vec<_>>();
            Ok(WorkspaceAuthorityStatus {
                schema: WORKSPACE_AUTHORITY_STATUS_SCHEMA.into(),
                protocol: WorkspaceClaimProtocol::current(),
                workspace: workspace.clone(),
                lifecycle_revision: state.lifecycle_revision,
                live_reconciliation_claim: state.reconciliation.is_some(),
                live_owner_count: live_owners.len(),
                clear: state.reconciliation.is_none() && live_owners.is_empty(),
                live_owners,
            })
        })
    }
    /// Return exact live owner identities after pruning expiry under the
    /// workspace lock. This local-only probe never exposes authority tokens.
    pub fn owner_status(
        &self,
        workspace: &WorkspaceIdentity,
        now_ms: u64,
    ) -> Result<Vec<WorkspaceOwnerStatus>> {
        workspace.verify()?;
        self.with_lock(workspace, || {
            let mut state = self.read_or_empty(workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
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
        self.with_lock(&claim.workspace, || {
            let mut state = self.read_or_empty(&claim.workspace)?;
            if state.prune(now_ms) {
                self.commit(&mut state)?;
            }
            match &state.reconciliation {
                None => Ok(()),
                Some(current) if current == claim => {
                    state.reconciliation = None;
                    self.commit(&mut state)
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
        self.with_lock(&binding.workspace, || {
            let state = self.read_or_empty(&binding.workspace)?;
            match state.reconciliation {
                None => Ok(()),
                Some(claim) if claim.expires_at_ms <= now_ms => Ok(()),
                Some(claim) if binding.claim.as_ref() == Some(&claim) => Ok(()),
                Some(_) => Err(invalid(
                    "workspace_claim",
                    "workspace is fenced by a live reconciliation claim",
                )),
            }
        })
    }

    fn with_lock<T>(
        &self,
        workspace: &WorkspaceIdentity,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        crate::engine::local_files::create_dir_all_durably(&self.root)?;
        sync_dir(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.lock_path(workspace))
            .map_err(io_error)?;
        lock.lock_exclusive().map_err(io_error)?;
        let result = operation();
        let _ = FileExt::unlock(&lock);
        result
    }
    #[cfg(any(test, feature = "test-support"))]
    fn inject_owner_release_failure(&self, workspace: &WorkspaceIdentity) -> Result<()> {
        // This runs under the target workspace lock, so one test's injected
        // release failure cannot be consumed by another workspace.
        let path = self.owner_release_failure_path(workspace);
        let remaining = match fs::read_to_string(&path) {
            Ok(value) => value.trim().parse::<u64>().unwrap_or_default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(io_error(error)),
        };
        if remaining == 0 {
            return Ok(());
        }
        if remaining == 1 {
            fs::remove_file(path).map_err(io_error)?;
        } else {
            fs::write(path, (remaining - 1).to_string()).map_err(io_error)?;
        }
        Err(Error::internal_io(
            "injected workspace owner release failure",
            None,
        ))
    }
    fn read_or_empty(&self, workspace: &WorkspaceIdentity) -> Result<WorkspaceAuthority> {
        let path = self.state_path(workspace);
        if !path.exists() {
            return Ok(WorkspaceAuthority::empty(workspace.clone()));
        }
        let state: WorkspaceAuthority = serde_json::from_slice(&fs::read(&path).map_err(io_error)?)
            .map_err(|error| {
                Error::internal_json(error.to_string(), Some(path.display().to_string()))
            })?;
        state.verify()?;
        if state.workspace != *workspace {
            return Err(invalid(
                "workspace_authority",
                "authority state does not match requested workspace",
            ));
        }
        Ok(state)
    }
    fn commit(&self, state: &mut WorkspaceAuthority) -> Result<()> {
        state.lifecycle_revision = state
            .lifecycle_revision
            .checked_add(1)
            .ok_or_else(|| invalid("workspace_authority", "authority epoch overflow"))?;
        self.write_state(state)
    }
    fn write_state(&self, state: &WorkspaceAuthority) -> Result<()> {
        let path = self.state_path(&state.workspace);
        let bytes = serde_json::to_vec(state)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut file = File::create(&temporary).map_err(io_error)?;
        file.write_all(&bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        fs::rename(&temporary, &path).map_err(io_error)?;
        sync_dir(&self.root)
    }
    fn state_path(&self, workspace: &WorkspaceIdentity) -> PathBuf {
        self.root.join(format!("{}.json", workspace_key(workspace)))
    }
    fn lock_path(&self, workspace: &WorkspaceIdentity) -> PathBuf {
        self.root.join(format!("{}.lock", workspace_key(workspace)))
    }
    #[cfg(any(test, feature = "test-support"))]
    fn owner_release_failure_path(&self, workspace: &WorkspaceIdentity) -> PathBuf {
        self.root.join(format!(
            "{}.owner-release-failures.test",
            workspace_key(workspace)
        ))
    }
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
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(io_error)
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
fn io_error(error: std::io::Error) -> Error {
    Error::internal_io(error.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> WorkspaceIdentity {
        WorkspaceIdentity::new("managed-workspace", "repo@task").unwrap()
    }
    fn store() -> (tempfile::TempDir, WorkspaceClaimStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceClaimStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn owners_block_reconciliation_and_can_coexist() {
        let (_dir, store) = store();
        let one = store
            .register_owner(identity(), "local:one", 100, 10)
            .unwrap();
        let two = store
            .register_owner(identity(), "reverse:two", 100, 10)
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
        let restarted = WorkspaceClaimStore::new(dir.path());
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
        let restarted = WorkspaceClaimStore::new(dir.path());
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
}
