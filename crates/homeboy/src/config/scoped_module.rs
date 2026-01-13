use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModuleConfig {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, Value>,
}
