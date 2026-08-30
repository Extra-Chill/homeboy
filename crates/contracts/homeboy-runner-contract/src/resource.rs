//! Transport-neutral runner resource telemetry contracts.

use serde::{Deserialize, Serialize};

/// Resource-usage metrics captured while a runner child process ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceMetrics {
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_user_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_system_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_bytes: Option<u64>,
    pub sample_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_process_count_peak: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_guard: Option<RunnerResourceGuardLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_violation: Option<RunnerResourceGuardViolation>,
    pub source: String,
}

/// The resource-guard limits in force for a runner child process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceGuardLimits {
    pub rss_limit_bytes: u64,
    pub process_count_limit: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_process_count_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit_ceiling: Option<u64>,
    pub concurrency: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_capacity_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_headroom_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_rss_budget_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_rss_bytes: Option<u64>,
    pub rss_limit_source: String,
}

/// A resource-guard violation that terminated or flagged a runner child.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerResourceGuardViolation {
    pub reason: String,
    pub message: String,
    pub rss_bytes: u64,
    pub rss_limit_bytes: u64,
    pub process_count: u64,
    pub process_count_limit: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_metrics_keep_their_minimal_wire_shape() {
        let value = serde_json::json!({
            "duration_ms": 25,
            "sample_count": 2,
            "source": "process-tree",
        });
        let metrics = RunnerResourceMetrics {
            duration_ms: 25,
            cpu_user_ms: None,
            cpu_system_ms: None,
            peak_rss_bytes: None,
            sample_count: 2,
            child_process_count_peak: None,
            resource_guard: None,
            guard_violation: None,
            source: "process-tree".to_string(),
        };

        assert_eq!(serde_json::to_value(&metrics).expect("serialize"), value);
        assert_eq!(
            serde_json::from_value::<RunnerResourceMetrics>(value).expect("deserialize"),
            metrics
        );
    }

    #[test]
    fn resource_metrics_keep_their_complete_wire_shape() {
        let value = serde_json::json!({
            "duration_ms": 25,
            "cpu_user_ms": 11,
            "cpu_system_ms": 7,
            "peak_rss_bytes": 512,
            "sample_count": 2,
            "child_process_count_peak": 3,
            "resource_guard": {
                "rss_limit_bytes": 1024,
                "process_count_limit": 8,
                "process_count_limit_source": "configured",
                "requested_process_count_limit": 10,
                "process_count_limit_ceiling": 8,
                "concurrency": 2,
                "memory_capacity_bytes": 4096,
                "host_headroom_bytes": 512,
                "aggregate_rss_budget_bytes": 2048,
                "active_rss_bytes": 256,
                "aggregate_rss_bytes": 768,
                "rss_limit_source": "host-capacity",
            },
            "guard_violation": {
                "reason": "rss_limit_exceeded",
                "message": "runner child exceeded its RSS limit",
                "rss_bytes": 1536,
                "rss_limit_bytes": 1024,
                "process_count": 3,
                "process_count_limit": 8,
            },
            "source": "process-tree",
        });

        let metrics =
            serde_json::from_value::<RunnerResourceMetrics>(value.clone()).expect("deserialize");
        assert_eq!(serde_json::to_value(metrics).expect("serialize"), value);
    }
}
