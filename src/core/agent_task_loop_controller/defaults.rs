use super::*;
use crate::core::paths;
use chrono::Utc;
use serde_json::Value;

pub(crate) fn default_controller_phase() -> String {
    "init".to_string()
}

pub const DEFAULT_FAN_OUT_MAX_ITEMS: usize = 50;

pub(crate) fn default_fan_out_max_items() -> usize {
    DEFAULT_FAN_OUT_MAX_ITEMS
}

pub(crate) fn is_default_fan_out_max_items(value: &usize) -> bool {
    *value == DEFAULT_FAN_OUT_MAX_ITEMS
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn is_true(value: &bool) -> bool {
    *value
}

pub(crate) fn default_config_version() -> String {
    "v1".to_string()
}

pub(crate) fn entity_dedupe_key(entity_type: &str, key: &str) -> String {
    format!("entity:{entity_type}:{key}")
}

pub(crate) fn sanitize_loop_id(raw: &str) -> String {
    paths::sanitize_path_segment(raw)
}

pub(crate) fn controller_schema() -> String {
    AGENT_TASK_LOOP_CONTROLLER_SCHEMA.to_string()
}

pub(crate) fn open_wait_status() -> AgentTaskLoopWaitStatus {
    AgentTaskLoopWaitStatus::Open
}

pub(crate) fn default_candidate_max_attempts() -> u32 {
    3
}

pub(crate) fn default_pr_ownership_max_retries() -> u32 {
    3
}

pub(crate) fn merge_json_object(left: Value, right: Value) -> Value {
    let mut merged = left.as_object().cloned().unwrap_or_default();
    if let Some(right) = right.as_object() {
        for (key, value) in right {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

pub(crate) fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
