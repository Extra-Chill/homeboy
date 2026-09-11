use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const WORKSPACE_CLAIM_CAPABILITY: &str = "workspace-claim";
pub const WORKSPACE_OWNER_LEASE_CAPABILITY: &str = "workspace-owner-lease";
pub const WORKSPACE_CLAIM_PROTOCOL_VERSION: u32 = 2;
pub const WORKSPACE_IDENTITY_SCHEMA: &str = "homeboy/workspace-identity/v1";
pub const WORKSPACE_CLAIM_SCHEMA: &str = "homeboy/workspace-claim/v2";
pub const WORKSPACE_OWNER_LEASE_SCHEMA: &str = "homeboy/workspace-owner-lease/v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceClaimProtocol {
    pub capability: String,
    pub version: u32,
}

impl WorkspaceClaimProtocol {
    pub fn current() -> Self {
        Self {
            capability: WORKSPACE_CLAIM_CAPABILITY.into(),
            version: WORKSPACE_CLAIM_PROTOCOL_VERSION,
        }
    }

    pub fn verify(&self) -> Result<()> {
        (self.capability == WORKSPACE_CLAIM_CAPABILITY
            && self.version == WORKSPACE_CLAIM_PROTOCOL_VERSION)
            .then_some(())
            .ok_or_else(|| {
                invalid(
                    "workspace_claim_protocol",
                    "workspace authority does not advertise the required claim protocol",
                )
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceOwnerLeaseProtocol {
    pub capability: String,
    pub version: u32,
}

impl WorkspaceOwnerLeaseProtocol {
    pub fn current() -> Self {
        Self {
            capability: WORKSPACE_OWNER_LEASE_CAPABILITY.into(),
            version: WORKSPACE_CLAIM_PROTOCOL_VERSION,
        }
    }

    pub fn verify(&self) -> Result<()> {
        (self.capability == WORKSPACE_OWNER_LEASE_CAPABILITY
            && self.version == WORKSPACE_CLAIM_PROTOCOL_VERSION)
            .then_some(())
            .ok_or_else(|| {
                invalid(
                    "workspace_owner_lease_protocol",
                    "workspace authority does not advertise the required owner lease protocol",
                )
            })
    }
}

/// A portable logical locator. Host paths are deliberately excluded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkspaceIdentity {
    pub schema: String,
    pub kind: String,
    pub locator: String,
}

impl WorkspaceIdentity {
    pub fn new(kind: impl Into<String>, locator: impl Into<String>) -> Result<Self> {
        let identity = Self {
            schema: WORKSPACE_IDENTITY_SCHEMA.into(),
            kind: kind.into(),
            locator: locator.into(),
        };
        identity.verify()?;
        Ok(identity)
    }

    pub fn verify(&self) -> Result<()> {
        (self.schema == WORKSPACE_IDENTITY_SCHEMA
            && !self.kind.trim().is_empty()
            && !self.locator.trim().is_empty()
            && !self.kind.contains('\n')
            && !self.locator.contains('\n'))
        .then_some(())
        .ok_or_else(|| {
            invalid(
                "workspace_identity",
                "workspace identity is malformed or unsupported",
            )
        })
    }
}

/// An exclusive short reconciliation fence. `lifecycle_revision` is an
/// authority-issued monotonically increasing epoch; callers never supply it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceClaim {
    pub schema: String,
    pub protocol: WorkspaceClaimProtocol,
    pub workspace: WorkspaceIdentity,
    pub lifecycle_revision: u64,
    pub token: String,
    pub expires_at_ms: u64,
}

/// A renewable active-task authority. Several distinct owner identities may be live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceOwnerLease {
    pub schema: String,
    pub protocol: WorkspaceOwnerLeaseProtocol,
    pub workspace: WorkspaceIdentity,
    pub owner_id: String,
    pub lifecycle_revision: u64,
    pub token: String,
    pub expires_at_ms: u64,
}

/// Optional transport binding for a reconciliation operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceClaimBinding {
    pub workspace: WorkspaceIdentity,
    pub lifecycle_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<WorkspaceClaim>,
}

impl WorkspaceClaimBinding {
    pub fn verify(&self) -> Result<()> {
        self.workspace.verify()?;
        if let Some(claim) = &self.claim {
            claim.verify_shape(0)?;
            if claim.workspace != self.workspace
                || claim.lifecycle_revision != self.lifecycle_revision
            {
                return Err(invalid(
                    "workspace_claim_binding",
                    "workspace identity and authority epoch do not agree",
                ));
            }
        }
        Ok(())
    }
}

impl WorkspaceClaim {
    pub fn verify_shape(&self, now_ms: u64) -> Result<()> {
        self.protocol.verify()?;
        self.workspace.verify()?;
        (self.schema == WORKSPACE_CLAIM_SCHEMA
            && !self.token.trim().is_empty()
            && self.lifecycle_revision > 0
            && self.expires_at_ms > now_ms)
            .then_some(())
            .ok_or_else(|| {
                invalid(
                    "workspace_claim",
                    "workspace reconciliation claim is malformed or expired",
                )
            })
    }
}

impl WorkspaceOwnerLease {
    pub fn verify_shape(&self, now_ms: u64) -> Result<()> {
        self.protocol.verify()?;
        self.workspace.verify()?;
        (self.schema == WORKSPACE_OWNER_LEASE_SCHEMA
            && !self.owner_id.trim().is_empty()
            && !self.token.trim().is_empty()
            && self.lifecycle_revision > 0
            && self.expires_at_ms > now_ms)
            .then_some(())
            .ok_or_else(|| {
                invalid(
                    "workspace_owner_lease",
                    "workspace owner lease is malformed or expired",
                )
            })
    }
}

fn invalid(field: &str, message: &str) -> Error {
    Error::validation_invalid_argument(field, message, None, None)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn authority_values_preserve_exact_wire_shapes() {
        let workspace = WorkspaceIdentity::new("managed-workspace", "repo@task").unwrap();
        let claim = WorkspaceClaim {
            schema: WORKSPACE_CLAIM_SCHEMA.into(),
            protocol: WorkspaceClaimProtocol::current(),
            workspace: workspace.clone(),
            lifecycle_revision: 7,
            token: "claim-token".into(),
            expires_at_ms: 100,
        };
        let lease = WorkspaceOwnerLease {
            schema: WORKSPACE_OWNER_LEASE_SCHEMA.into(),
            protocol: WorkspaceOwnerLeaseProtocol::current(),
            workspace: workspace.clone(),
            owner_id: "runner:one".into(),
            lifecycle_revision: 8,
            token: "lease-token".into(),
            expires_at_ms: 101,
        };
        let binding = WorkspaceClaimBinding {
            workspace,
            lifecycle_revision: 7,
            claim: Some(claim.clone()),
        };

        assert_eq!(
            serde_json::to_value(&claim).unwrap(),
            json!({
                "schema": "homeboy/workspace-claim/v2",
                "protocol": { "capability": "workspace-claim", "version": 2 },
                "workspace": { "schema": "homeboy/workspace-identity/v1", "kind": "managed-workspace", "locator": "repo@task" },
                "lifecycle_revision": 7,
                "token": "claim-token",
                "expires_at_ms": 100,
            })
        );
        assert_eq!(
            serde_json::to_value(&lease).unwrap(),
            json!({
                "schema": "homeboy/workspace-owner-lease/v2",
                "protocol": { "capability": "workspace-owner-lease", "version": 2 },
                "workspace": { "schema": "homeboy/workspace-identity/v1", "kind": "managed-workspace", "locator": "repo@task" },
                "owner_id": "runner:one",
                "lifecycle_revision": 8,
                "token": "lease-token",
                "expires_at_ms": 101,
            })
        );
        assert_eq!(
            serde_json::to_value(&binding).unwrap(),
            json!({
                "workspace": { "schema": "homeboy/workspace-identity/v1", "kind": "managed-workspace", "locator": "repo@task" },
                "lifecycle_revision": 7,
                "claim": claim,
            })
        );
    }

    #[test]
    fn authority_values_round_trip_and_binding_omits_absent_claim() {
        let binding: WorkspaceClaimBinding = serde_json::from_value(json!({
            "workspace": { "schema": WORKSPACE_IDENTITY_SCHEMA, "kind": "managed-workspace", "locator": "repo@task" },
            "lifecycle_revision": 9,
        }))
        .unwrap();

        assert_eq!(binding.claim, None);
        assert_eq!(
            serde_json::to_value(&binding).unwrap(),
            json!({
                "workspace": { "schema": WORKSPACE_IDENTITY_SCHEMA, "kind": "managed-workspace", "locator": "repo@task" },
                "lifecycle_revision": 9,
            })
        );
    }
}
