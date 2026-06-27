#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ComponentReconcileReport {
    pub component_id: String,
    pub registration_path: String,
    pub registered_local_path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discovered_local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    pub applied: bool,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ComponentLocalPathDiagnostic {
    pub component_id: String,
    pub local_path: String,
    pub exists: bool,
    pub is_git_checkout: bool,
    pub is_temp_checkout: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub discovered_candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
}
