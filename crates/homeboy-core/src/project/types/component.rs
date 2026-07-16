use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectComponentAttachment {
    pub id: String,
    pub local_path: String,
    /// Project-specific deploy target for this attached component.
    ///
    /// Repo-owned `homeboy.json` is portable component metadata, while the
    /// install path can vary by project layout. Keeping this optional field on
    /// the attachment lets one component deploy to multiple projects without
    /// rewriting the repo-tracked `remote_path` for each environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

pub type ProjectComponentOverrides = crate::component::ComponentOverrideConfig;
