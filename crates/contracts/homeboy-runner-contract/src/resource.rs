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
    /// The memory cgroup containing the command at launch, sampled before and
    /// after it ran. This is separate from process RSS: it proves whether the
    /// runtime's memory controller recorded an OOM kill during the command.
    #[serde(default)]
    pub cgroup_memory: RunnerCgroupMemoryEvidence,
    pub source: String,
}

/// Best-effort cgroup-v2 memory evidence owned by the runner execution boundary.
///
/// `oom_kill_delta` is authoritative evidence that the containing memory cgroup
/// killed at least one task while the command ran. It does not attribute a
/// SIGKILL by itself: a zero or absent delta is not proof that the command was
/// not killed for some other reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerCgroupMemoryEvidence {
    /// `observed`, `partial`, `unavailable`, or `unsupported`.
    pub status: String,
    pub source: String,
    /// The metric scope. `child_cgroup` is a group-level observation and can
    /// include processes other than the launched child.
    pub scope: String,
    /// Attribution of a cgroup `oom_kill` event to the launched child. This is
    /// `unknown` unless a runtime independently proves isolation or identifies
    /// the kernel victim.
    pub oom_kill_attribution: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_current_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_peak_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oom_kill_count_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oom_kill_count_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oom_kill_delta: Option<u64>,
}

impl Default for RunnerCgroupMemoryEvidence {
    fn default() -> Self {
        Self {
            status: "unsupported".to_string(),
            source: "unsupported".to_string(),
            scope: "unavailable".to_string(),
            oom_kill_attribution: "unknown".to_string(),
            memory_limit_bytes: None,
            memory_current_bytes: None,
            memory_peak_bytes: None,
            oom_kill_count_before: None,
            oom_kill_count_after: None,
            oom_kill_delta: None,
        }
    }
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

impl RunnerResourceGuardViolation {
    pub fn blocker_code(&self) -> String {
        format!("resource_guard.{}", self.reason)
    }

    pub fn observed_summary(&self) -> String {
        if self.reason == "rss_limit_exceeded" {
            format!(
                "rss_bytes={} exceeds limit {}",
                self.rss_bytes, self.rss_limit_bytes
            )
        } else {
            format!(
                "process_count={} exceeds limit {}",
                self.process_count, self.process_count_limit
            )
        }
    }

    pub fn operator_message(&self) -> String {
        format!("{}: {}", self.blocker_code(), self.observed_summary())
    }

    pub fn remedy(&self) -> String {
        if self.reason == "rss_limit_exceeded" {
            format!(
                "raise HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES above {} (limit {}); do not resume into the same guard",
                self.rss_bytes, self.rss_limit_bytes
            )
        } else {
            format!(
                "raise HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT above {} (limit {}); do not resume into the same guard",
                self.process_count, self.process_count_limit
            )
        }
    }
}

/// A guard stop found in a runner result, job event, or operator log payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceGuardStop {
    pub violation: RunnerResourceGuardViolation,
    pub exit_code: Option<i64>,
}

const GUARD_SEARCH_DEPTH: usize = 16;

/// Find a resource-guard stop nested in runner job events, result metrics, or
/// the `runner.result.transport.metrics.guard_violation` log shape.
pub fn find_resource_guard_stop(value: &serde_json::Value) -> Option<ResourceGuardStop> {
    find_resource_guard_stop_at(value, 0)
}

fn find_resource_guard_stop_at(
    value: &serde_json::Value,
    depth: usize,
) -> Option<ResourceGuardStop> {
    if depth > GUARD_SEARCH_DEPTH {
        return None;
    }
    match value {
        serde_json::Value::Object(fields) => {
            if let Some(mut stop) = stop_on_object(fields) {
                if stop.exit_code.is_none() {
                    stop.exit_code = fields.get("exit_code").and_then(serde_json::Value::as_i64);
                }
                return Some(stop);
            }
            let mut found = None;
            for nested in fields.values() {
                if let Some(stop) = find_resource_guard_stop_at(nested, depth + 1) {
                    found = Some(stop);
                    break;
                }
            }
            if let Some(stop) = found.as_mut() {
                if stop.exit_code.is_none() {
                    stop.exit_code = fields.get("exit_code").and_then(serde_json::Value::as_i64);
                }
            }
            found
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|item| find_resource_guard_stop_at(item, depth + 1)),
        _ => None,
    }
}

fn stop_on_object(
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Option<ResourceGuardStop> {
    let raw = fields
        .get("guard_violation")
        .or_else(|| fields.get("resource_guard_violation"))?;
    let violation = serde_json::from_value::<RunnerResourceGuardViolation>(raw.clone()).ok()?;
    if violation.reason.trim().is_empty() {
        return None;
    }
    Some(ResourceGuardStop {
        exit_code: fields.get("exit_code").and_then(serde_json::Value::as_i64),
        violation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_metrics_keep_their_minimal_wire_shape() {
        let value = serde_json::json!({
            "duration_ms": 25,
            "sample_count": 2,
            "cgroup_memory": {
                "status": "unsupported",
                "source": "unsupported",
                "scope": "unavailable",
                "oom_kill_attribution": "unknown",
            },
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
            cgroup_memory: RunnerCgroupMemoryEvidence {
                status: "unsupported".to_string(),
                source: "unsupported".to_string(),
                scope: "unavailable".to_string(),
                oom_kill_attribution: "unknown".to_string(),
                ..RunnerCgroupMemoryEvidence::default()
            },
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
            "cgroup_memory": {
                "status": "observed",
                "source": "linux_cgroup_v2",
                "scope": "child_cgroup",
                "oom_kill_attribution": "unknown",
                "memory_limit_bytes": 1024,
                "memory_current_bytes": 512,
                "memory_peak_bytes": 768,
                "oom_kill_count_before": 3,
                "oom_kill_count_after": 4,
                "oom_kill_delta": 1,
            },
            "source": "process-tree",
        });

        let metrics =
            serde_json::from_value::<RunnerResourceMetrics>(value.clone()).expect("deserialize");
        assert_eq!(serde_json::to_value(metrics).expect("serialize"), value);
    }

    #[test]
    fn nested_log_shape_becomes_a_typed_guard_stop() {
        let rss_bytes: u64 = 13_173_456_896;
        let value = serde_json::json!({
            "runner": {
                "result": {
                    "transport": {
                        "exit_code": 1,
                        "metrics": {
                            "guard_violation": {
                                "reason": "process_count_limit_exceeded",
                                "message": "stopped",
                                "rss_bytes": rss_bytes,
                                "rss_limit_bytes": 1024,
                                "process_count": 141,
                                "process_count_limit": 128
                            }
                        }
                    }
                }
            }
        });

        let stop = find_resource_guard_stop(&value).expect("guard stop");
        assert_eq!(stop.exit_code, Some(1));
        assert_eq!(
            stop.violation.blocker_code(),
            "resource_guard.process_count_limit_exceeded"
        );
        assert!(stop.violation.observed_summary().contains("141"));
        assert!(stop
            .violation
            .remedy()
            .contains("HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT"));
    }
}
