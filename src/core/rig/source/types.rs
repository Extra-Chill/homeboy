use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RigSourceListResult {
    pub sources: Vec<RigSourceGroup>,
    pub invalid: Vec<InvalidRigSourceMetadata>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigSourceGroup {
    pub source: String,
    pub package_path: String,
    pub package_id: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub rigs: Vec<RigSourceRig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigSourceRig {
    pub id: String,
    pub rig_path: String,
    pub config_path: String,
    pub config_present: bool,
    pub config_owned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InvalidRigSourceMetadata {
    pub id: String,
    pub metadata_path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigSourceRemoveResult {
    pub selector: String,
    pub source: RigSourceGroup,
    pub removed: Vec<RemovedRigSourceRig>,
    pub skipped: Vec<SkippedRigSourceRig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_package_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemovedRigSourceRig {
    pub id: String,
    pub config_path: String,
    pub metadata_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedRigSourceRig {
    pub id: String,
    pub config_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigSourceUpdateResult {
    pub updated: Vec<RigSourceUpdatedRig>,
    pub skipped: Vec<SkippedRigSourceUpdate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigSourceUpdatedRig {
    pub id: String,
    pub source: String,
    pub path: String,
    pub spec_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedRigSourceUpdate {
    pub id: String,
    pub source: String,
    pub reason: String,
}
