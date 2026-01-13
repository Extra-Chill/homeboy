use serde::{Deserialize, Serialize};

use super::{ComponentConfiguration, ProjectConfiguration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreatePayload {
    pub project: ProjectConfiguration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<ComponentConfiguration>>,
}
