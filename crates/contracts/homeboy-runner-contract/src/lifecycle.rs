//! Transport-neutral runner lifecycle metadata.

use serde::{Deserialize, Serialize};

/// Which side of a runner exchange owns a lifecycle resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerLifecycleOwner {
    Controller,
    Runner,
    Broker,
    Local,
}

impl RunnerLifecycleOwner {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Controller => "controller",
            Self::Runner => "runner",
            Self::Broker => "broker",
            Self::Local => "local",
        }
    }
}

/// Lifecycle facts carried by runner jobs and execution envelopes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobLifecycleMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_child_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_cell_count: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_owner_keeps_its_wire_values() {
        for (owner, expected) in [
            (RunnerLifecycleOwner::Controller, "controller"),
            (RunnerLifecycleOwner::Runner, "runner"),
            (RunnerLifecycleOwner::Broker, "broker"),
            (RunnerLifecycleOwner::Local, "local"),
        ] {
            assert_eq!(owner.as_str(), expected);
            assert_eq!(serde_json::to_value(owner).expect("serialize"), expected);
        }
    }

    #[test]
    fn empty_lifecycle_metadata_keeps_its_wire_shape() {
        assert_eq!(
            serde_json::to_value(RunnerJobLifecycleMetadata::default()).expect("serialize"),
            serde_json::json!({})
        );
    }

    #[test]
    fn lifecycle_metadata_keeps_its_complete_wire_shape() {
        let metadata = RunnerJobLifecycleMetadata {
            source: Some("daemon".to_string()),
            kind: Some("agent_task".to_string()),
            durable_run_id: Some("run-1".to_string()),
            active_child_count: Some(2),
            active_cell_count: Some(3),
        };

        assert_eq!(
            serde_json::to_value(metadata).expect("serialize"),
            serde_json::json!({
                "source": "daemon",
                "kind": "agent_task",
                "durable_run_id": "run-1",
                "active_child_count": 2,
                "active_cell_count": 3,
            })
        );
    }
}
