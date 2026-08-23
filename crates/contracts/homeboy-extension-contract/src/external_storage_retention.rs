//! Versioned provider contract for storage an installed runtime owns outside a
//! Homeboy checkout. Providers inventory and reclaim their own resources; core
//! supplies policy and never removes an external path itself.

use serde::{Deserialize, Serialize};

pub const EXTERNAL_STORAGE_RETENTION_SCHEMA: &str = "homeboy/external-storage-retention/v1";
pub const DEFAULT_EXTERNAL_STORAGE_PROVIDER_TIMEOUT_SECONDS: u64 = 30;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageInventory {
    #[serde(default = "default_external_storage_schema")]
    pub schema: String,
    pub provider_id: String,
    #[serde(default)]
    pub items: Vec<ExternalStorageItem>,
    /// Bytes the provider can account for but cannot safely classify. They are
    /// visible separately and never candidates.
    #[serde(default)]
    pub unknown_bytes: u64,
}

fn default_external_storage_schema() -> String {
    EXTERNAL_STORAGE_RETENTION_SCHEMA.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStorageRequest {
    pub schema: String,
    pub operation: ExternalStorageOperation,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_ids: Vec<String>,
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
            "unknown_bytes": 9,
            "items": [{
                "id": "scratch-1", "class": "scratch", "bytes": 4,
                "locator": "tmp/scratch-1", "reconstructable": true,
                "active": false, "referenced": false, "ownership_known": true, "age_days": 7
            }]
        }))
        .expect("inventory parses");
        assert_eq!(inventory.schema, EXTERNAL_STORAGE_RETENTION_SCHEMA);
        assert_eq!(inventory.unknown_bytes, 9);
    }
}
