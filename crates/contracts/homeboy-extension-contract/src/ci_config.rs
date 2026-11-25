use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// CI reproduction profiles declared by an extension.
///
/// Core treats these as explicit local-reproduction contracts. Best-effort
/// workflow discovery can inventory CI files, but runnable equivalence comes
/// from these declarations.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiCapability {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, CiProfileSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub jobs: BTreeMap<String, CiJobSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CiCacheSpec>,
}

/// Reconstructable CI state owned by an extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiCacheSpec {
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<CiCachePath>,
    /// The cached paths are content-addressed download caches (package-manager
    /// stores) that stay valid across key-file changes. CI restores the newest
    /// entry and saves a new one whenever the cached contents change, so the
    /// cache keeps rolling forward even when the installs that fill it run for
    /// components whose key files CI never sees (for example, deploys that
    /// hydrate other components of a monorepo). `key_files` become optional.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rolling: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CiCachePath {
    pub root: CiCachePathRoot,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CiCachePathRoot {
    Component,
    Home,
    HomeboyData,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiProfileSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum CiJobFidelity {
    LocalEquivalent,
    LocalPartial,
    RemoteOnly,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiJobSpec {
    #[serde(flatten)]
    pub mapping: CiJobMapping,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiJobMapping {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CiLocalContext {
    #[serde(default)]
    pub fidelity: CiJobFidelity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_cache_is_opt_in_omitted_when_off_and_drops_key_file_requirement() {
        let keyed: CiCacheSpec = serde_json::from_str(
            r#"{"namespace":"cargo","key_files":["Cargo.lock"],"paths":[{"root":"home","path":".cargo/registry"}]}"#,
        )
        .expect("parse lockfile-keyed cache");
        assert!(!keyed.rolling);
        assert!(serde_json::to_value(&keyed)
            .expect("serialize")
            .get("rolling")
            .is_none());

        let rolling: CiCacheSpec = serde_json::from_str(
            r#"{"namespace":"downloads","rolling":true,"paths":[{"root":"home","path":".cache/store","env":"STORE_DIR"}]}"#,
        )
        .expect("parse rolling cache without key files");
        assert!(rolling.key_files.is_empty());
        assert_eq!(
            serde_json::to_value(&rolling).expect("serialize")["rolling"],
            true
        );
    }
}
