//! Versioned provider contract for storage an installed runtime owns outside a
//! Homeboy checkout. Providers inventory and reclaim their own resources; core
//! supplies policy and never removes an external path itself.

use serde::{Deserialize, Serialize};

pub const EXTERNAL_STORAGE_RETENTION_SCHEMA: &str = "homeboy/external-storage-retention/v1";
pub const DEFAULT_EXTERNAL_STORAGE_PROVIDER_TIMEOUT_SECONDS: u64 = 30;
/// Hard protocol ceilings protect stdin delivery before provider-specific policy
/// is available. The aggregate may choose stricter limits.
pub const MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS: usize = 1_000;
pub const MAX_EXTERNAL_STORAGE_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageRetentionProviderConfig {
    pub id: String,
    /// Executable plus fixed arguments. The provider receives one JSON request
    /// on stdin and returns one JSON response on stdout.
    pub command: Vec<String>,
    /// Per-invocation ceiling. A hung runtime helper must not block the bounded
    /// aggregate retention pass indefinitely.
    #[serde(default = "default_external_storage_provider_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_external_storage_provider_timeout_seconds() -> u64 {
    DEFAULT_EXTERNAL_STORAGE_PROVIDER_TIMEOUT_SECONDS
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageRetentionConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ExternalStorageRetentionProviderConfig>,
}

impl ExternalStorageRetentionConfig {
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalStorageResourceClass {
    Scratch,
    DurableArtifact,
    SessionStore,
    Credential,
    PinnedExport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageItem {
    pub id: String,
    pub root_id: String,
    pub class: ExternalStorageResourceClass,
    pub bytes: u64,
    /// A provider-local, non-secret locator for bounded operator evidence.
    pub locator: String,
    /// Reclaimable items must be reproducible by the provider or be reclaimed
    /// through a provider-native compaction action.
    pub reconstructable: bool,
    /// An active lease/session/owner is an unconditional veto.
    pub active: bool,
    /// A retained session, snapshot, or export still points at this item.
    pub referenced: bool,
    /// Unknown ownership fails closed. This lets mixed-version installs report
    /// bytes without promoting old unlabelled paths to deletion candidates.
    pub ownership_known: bool,
    /// Whole days since the provider's terminal lifecycle transition.
    pub age_days: u64,
    /// Provider-issued conditional capability valid only for this inventory
    /// generation. A provider rejects it after liveness or references change.
    pub reclaim_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageRoot {
    pub id: String,
    /// Core reads capacity from this path but never deletes it directly.
    pub path: String,
}

/// Bounded inventory evidence. Omission means the provider completed its
/// inventory, preserving compatibility with existing v1 providers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageInventoryCompleteness {
    #[serde(default = "default_inventory_complete")]
    pub complete: bool,
    /// Per-root lower-bound evidence for every traversal that hit a bound.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete_roots: Vec<ExternalStorageIncompleteRoot>,
}

fn default_inventory_complete() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageIncompleteRoot {
    pub root_id: String,
    pub reason: String,
    pub observed_entries: u64,
    pub observed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageInventory {
    #[serde(default = "default_external_storage_schema")]
    pub schema: String,
    pub provider_id: String,
    /// Opaque snapshot/lease identity that binds a reclaim request to this
    /// inventory. Providers must reject stale generations.
    pub generation: String,
    #[serde(default)]
    pub roots: Vec<ExternalStorageRoot>,
    #[serde(default)]
    pub items: Vec<ExternalStorageItem>,
    /// Bytes the provider can account for but cannot safely classify. They are
    /// visible separately and never candidates.
    #[serde(default)]
    pub unknown_bytes: u64,
    /// When incomplete, byte values are lower bounds and unknown content stays
    /// non-reclaimable. This additive field is optional for v1 providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<ExternalStorageInventoryCompleteness>,
    /// Provider-native capabilities this inventory pass probed, such as a
    /// runtime-specific compaction command. Omission means the provider
    /// declares none, preserving compatibility with existing v1 providers.
    ///
    /// A native contract the provider cannot execute is a distinct state from
    /// an empty `items` list: the former means reclaimable bytes may exist
    /// and stay invisible until the contract works again, while the latter
    /// means the provider looked and found nothing. Flattening the two into
    /// a zero candidate count hid a 99 GB unreclaimed SQLite event table
    /// behind a `completed` outcome (#14955).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_contracts: Vec<ExternalStorageNativeContract>,
}

fn default_external_storage_schema() -> String {
    EXTERNAL_STORAGE_RETENTION_SCHEMA.to_string()
}

/// One provider-declared native capability and whether this inventory pass
/// could execute it.
///
/// A provider declares a contract for a capability it — not core — knows how
/// to run natively (e.g. compacting its own event log). Core never invokes
/// the underlying capability itself; it only aggregates and surfaces whether
/// the provider's own probe of that capability succeeded.
///
/// `deny_unknown_fields` is deliberately omitted: serde does not support it on
/// a struct with a `#[serde(flatten)]` field, since the flattened field must
/// stay free to absorb the status-specific keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalStorageNativeContract {
    /// Stable, versioned capability identity, e.g.
    /// `opencode.db.compact-events.v1`. Never a path or free-text description.
    pub contract_id: String,
    #[serde(flatten)]
    pub status: ExternalStorageNativeContractStatus,
}

/// Whether a declared native contract's probe succeeded.
///
/// `Satisfied` and `Unsatisfied` are distinguished by the tag itself, so a
/// provider cannot report "unsatisfied" while omitting the invocation and
/// failure an operator needs to act on it, and core cannot mistake a working
/// probe that found nothing for a broken one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalStorageNativeContractStatus {
    /// The provider successfully probed the contract. It may still find
    /// nothing reclaimable through it right now; that is a genuine clean
    /// result, not a blocker.
    Satisfied,
    /// The provider could not execute the invocation its native contract
    /// depends on. Reclaimable bytes may exist behind this capability and are
    /// invisible until the contract is fixed; this is an actionable blocker,
    /// never a clean root.
    Unsatisfied {
        /// The concrete invocation the provider attempted (a command line or
        /// equivalent), for bounded operator evidence.
        invocation: String,
        /// The provider's observed failure, bounded for operator evidence.
        failure: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageRequest {
    pub schema: String,
    pub operation: ExternalStorageOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reclaim_targets: Vec<ExternalStorageReclaimTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageReclaimTarget {
    pub id: String,
    pub reclaim_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalStorageOperation {
    Inventory,
    Reclaim,
}

/// Provider-native reclaim receipt. A session-store provider can remove expired
/// reference rows and compact a database here without exposing database paths
/// to Homeboy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageReclaimResult {
    pub schema: String,
    pub provider_id: String,
    pub generation: String,
    #[serde(default)]
    pub reclaimed_item_ids: Vec<String>,
    #[serde(default)]
    pub reclaimed_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_contract_round_trips_liveness_and_unknown_bytes() {
        let inventory: ExternalStorageInventory = serde_json::from_value(serde_json::json!({
            "provider_id": "fixture-runtime",
            "generation": "generation-1",
            "unknown_bytes": 9,
            "items": [{
                "id": "scratch-1", "root_id": "temp", "class": "scratch", "bytes": 4,
                "locator": "tmp/scratch-1", "reconstructable": true,
                "active": false, "referenced": false, "ownership_known": true, "age_days": 7,
                "reclaim_token": "opaque"
            }]
        }))
        .expect("inventory parses");
        assert_eq!(inventory.schema, EXTERNAL_STORAGE_RETENTION_SCHEMA);
        assert_eq!(inventory.unknown_bytes, 9);
        assert!(inventory.completeness.is_none());
    }

    #[test]
    fn native_contracts_omission_defaults_empty_for_existing_v1_providers() {
        let inventory: ExternalStorageInventory = serde_json::from_value(serde_json::json!({
            "provider_id": "fixture-runtime",
            "generation": "generation-1",
        }))
        .expect("inventory without native_contracts parses");
        assert!(inventory.native_contracts.is_empty());
    }

    #[test]
    fn unsatisfied_native_contract_round_trips_invocation_and_failure() {
        let inventory: ExternalStorageInventory = serde_json::from_value(serde_json::json!({
            "provider_id": "opencode.external-storage-retention",
            "generation": "generation-1",
            "native_contracts": [{
                "contract_id": "opencode.db.compact-events.v1",
                "status": "unsatisfied",
                "invocation": "opencode db event-log-status",
                "failure": "SQL syntax error near 'event-log-status'",
            }]
        }))
        .expect("inventory with an unsatisfied native contract parses");
        assert_eq!(inventory.native_contracts.len(), 1);
        let contract = &inventory.native_contracts[0];
        assert_eq!(contract.contract_id, "opencode.db.compact-events.v1");
        match &contract.status {
            ExternalStorageNativeContractStatus::Unsatisfied {
                invocation,
                failure,
            } => {
                assert_eq!(invocation, "opencode db event-log-status");
                assert_eq!(failure, "SQL syntax error near 'event-log-status'");
            }
            ExternalStorageNativeContractStatus::Satisfied => {
                panic!("expected an unsatisfied native contract")
            }
        }
        let round_tripped = serde_json::to_value(&inventory).expect("re-serializes");
        let reparsed: ExternalStorageInventory =
            serde_json::from_value(round_tripped).expect("round trip parses");
        assert_eq!(reparsed, inventory);
    }

    #[test]
    fn satisfied_native_contract_carries_no_failure_fields() {
        let inventory: ExternalStorageInventory = serde_json::from_value(serde_json::json!({
            "provider_id": "opencode.external-storage-retention",
            "generation": "generation-1",
            "native_contracts": [{
                "contract_id": "opencode.db.compact-events.v1",
                "status": "satisfied",
            }]
        }))
        .expect("inventory with a satisfied native contract parses");
        assert_eq!(
            inventory.native_contracts[0].status,
            ExternalStorageNativeContractStatus::Satisfied
        );
    }
}
