//! Transport-neutral runner discovery and readiness resources.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const RUNNER_DESCRIPTOR_SCHEMA: &str = "homeboy/runner-descriptor/v1";
pub const RUNNER_CAPABILITIES_SCHEMA: &str = "homeboy/runner-capabilities/v1";
pub const RUNNER_READINESS_SCHEMA: &str = "homeboy/runner-readiness/v1";
pub const RUNNER_INSPECTION_SCHEMA: &str = "homeboy/runner-inspection/v1";

/// The implementation kind backing a runner definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    Local,
    Ssh,
}

/// Stable configured identity used for runner discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerDescriptor {
    pub schema: String,
    pub runner_id: String,
    pub kind: RunnerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_limit: Option<usize>,
}

/// Capabilities observed through the runner's capability transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerCapabilities {
    pub schema: String,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub runtime_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub capabilities: BTreeSet<String>,
}

/// Authoritative admission and capacity projection for one runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerReadiness {
    pub schema: String,
    pub runner_id: String,
    pub connected: bool,
    pub accepting_jobs: bool,
    pub active_job_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// One complete read-only inspection assembled by the Runner API service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerInspection {
    pub schema: String,
    pub descriptor: RunnerDescriptor,
    pub readiness: RunnerReadiness,
    pub capabilities: RunnerCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_kind_keeps_the_established_wire_values() {
        assert_eq!(serde_json::to_value(RunnerKind::Local).unwrap(), "local");
        assert_eq!(serde_json::to_value(RunnerKind::Ssh).unwrap(), "ssh");
    }

    #[test]
    fn discovery_resources_are_versioned_and_omit_empty_optional_data() {
        let descriptor = RunnerDescriptor {
            schema: RUNNER_DESCRIPTOR_SCHEMA.to_string(),
            runner_id: "local".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: None,
            concurrency_limit: None,
        };
        let value = serde_json::to_value(descriptor).unwrap();

        assert_eq!(value["schema"], RUNNER_DESCRIPTOR_SCHEMA);
        assert_eq!(value["runner_id"], "local");
        assert_eq!(value["kind"], "local");
        assert!(value.get("server_id").is_none());
        assert!(value.get("concurrency_limit").is_none());
    }
}
