use std::collections::HashMap;

use crate::core::observation::LAB_OFFLOAD_METADATA_ENV;

pub(super) fn forward_env_if_present(env: &mut HashMap<String, String>, name: &str) {
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            env.insert(name.to_string(), value);
        }
    }
}

pub(super) fn build_lab_offload_env(lab_metadata: &serde_json::Value) -> HashMap<String, String> {
    HashMap::from([(
        LAB_OFFLOAD_METADATA_ENV.to_string(),
        serde_json::to_string(lab_metadata).unwrap_or_default(),
    )])
}
