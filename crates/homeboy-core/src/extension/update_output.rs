use serde::Serialize;

use super::SourceMetadataRepair;

/// Result of updating all extensions.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateAllResult {
    pub updated: Vec<UpdateEntry>,
    pub skipped: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_details: Vec<UpdateSkippedEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repaired_source_metadata: Vec<SourceMetadataRepairEntry>,
}

/// A single extension update entry with before/after versions.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateEntry {
    pub extension_id: String,
    pub old_version: String,
    pub new_version: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<String>,
    #[serde(flatten)]
    pub source_update: ExtensionSourceUpdate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repaired_source_metadata: Option<SourceMetadataRepair>,
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct ExtensionSourceUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceMetadataRepairEntry {
    pub extension_id: String,
    #[serde(flatten)]
    pub repair: SourceMetadataRepair,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateSkippedEntry {
    pub extension_id: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_source_update_flattens_legacy_json_fields() {
        let entry = UpdateEntry {
            extension_id: "wordpress".to_string(),
            old_version: "1.0.0".to_string(),
            new_version: "1.0.1".to_string(),
            linked: true,
            source_path: Some("/tmp/homeboy-extensions/wordpress".to_string()),
            git_root: Some("/tmp/homeboy-extensions".to_string()),
            source_update: ExtensionSourceUpdate {
                old_source_revision: Some("abc1234".to_string()),
                new_source_revision: Some("def5678".to_string()),
                old_branch: Some("feature".to_string()),
                new_branch: Some("main".to_string()),
                update_note: Some("updated linked source".to_string()),
            },
            repaired_source_metadata: None,
        };

        let value = serde_json::to_value(entry).expect("serialize update entry");

        assert_eq!(value["old_source_revision"], "abc1234");
        assert_eq!(value["new_source_revision"], "def5678");
        assert_eq!(value["old_branch"], "feature");
        assert_eq!(value["new_branch"], "main");
        assert!(value.get("source_update").is_none());
    }
}
