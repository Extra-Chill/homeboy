//! CI execution-context contract type.

use serde::{Deserialize, Serialize};

use crate::ci_config::{CiJobMapping, CiLocalContext};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CiContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub job_id: String,
    #[serde(flatten)]
    pub mapping: CiJobMapping,
    #[serde(flatten)]
    pub local_context: CiLocalContext,
}
