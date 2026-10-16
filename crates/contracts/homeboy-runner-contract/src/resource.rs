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
    /// The daemon's [`PROCESS_COUNT_LIMIT_ENV`] value before the ceiling clamp.
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
    /// How the process-count limit was chosen: `default` (scaled with the
    /// runner's available parallelism), `runner_override` (the daemon's
    /// [`PROCESS_COUNT_LIMIT_ENV`]), or `runner_ceiling` (clamped to the
    /// daemon's [`PROCESS_COUNT_CEILING_ENV`]). Absent on records written
    /// before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit_source: Option<String>,
    /// The runner-owned ceiling the process-count limit is clamped to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count_limit_ceiling: Option<u64>,
    /// The most common process names in the tree when the guard tripped,
    /// most frequent first, bounded to [`TOP_PROCESS_NAME_LIMIT`] entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_processes: Vec<RunnerProcessNameCount>,
}

/// How many processes with one name were in a guarded process tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerProcessNameCount {
    pub name: String,
    pub count: u64,
}

/// Bound on [`RunnerResourceGuardViolation::top_processes`].
pub const TOP_PROCESS_NAME_LIMIT: usize = 10;

/// Runner-owned process-count limit, read only from the runner daemon's own
/// environment. Defaults to 16 per available core (at least 128).
pub const PROCESS_COUNT_LIMIT_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT";
/// Runner-owned ceiling the process-count limit is clamped to, read only from
/// the runner daemon's own environment. Defaults to 32 per available core (at
/// least 256); `0` disables the ceiling.
pub const PROCESS_COUNT_CEILING_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT";
/// Runner-owned RSS limit, read only from the runner daemon's own environment.
pub const RSS_LIMIT_ENV: &str = "HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES";
/// Every runner-owned resource-guard policy key. A runner's `env` renders
/// these into its service unit; they are never forwarded with a job.
pub const RESOURCE_GUARD_POLICY_ENV_KEYS: &[&str] = &[
    PROCESS_COUNT_CEILING_ENV,
    PROCESS_COUNT_LIMIT_ENV,
    RSS_LIMIT_ENV,
];

const GIB: u64 = 1024 * 1024 * 1024;

impl RunnerResourceGuardViolation {
    pub fn blocker_code(&self) -> String {
        format!("resource_guard.{}", self.reason)
    }

    pub fn observed_summary(&self) -> String {
        if self.reason == "rss_limit_exceeded" {
            return format!(
                "rss_bytes={} exceeds limit {}",
                self.rss_bytes, self.rss_limit_bytes
            );
        }
        let summary = format!(
            "process_count={} exceeds limit {}",
            self.process_count, self.process_count_limit
        );
        if self.top_processes.is_empty() {
            return summary;
        }
        let top = self
            .top_processes
            .iter()
            .map(|process| format!("{}\u{d7}{}", process.name, process.count))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{summary} (top: {top})")
    }

    pub fn operator_message(&self) -> String {
        format!("{}: {}", self.blocker_code(), self.observed_summary())
    }

    /// The supported action for raising the limit that tripped. Guard policy
    /// is runner-owned: it is set in the runner's `env` and reaches the
    /// daemon only through its service unit, never through a job.
    pub fn remedy(&self) -> String {
        if self.reason == "rss_limit_exceeded" {
            let target = (self.rss_bytes / GIB + 2) * GIB;
            return format!(
                "RSS limit {} is runner-owned; {}",
                self.rss_limit_bytes,
                runner_policy_action(&[(RSS_LIMIT_ENV, target)])
            );
        }
        let target = (self.process_count / 64 + 2) * 64;
        let mut policy = Vec::with_capacity(2);
        if self
            .process_count_limit_ceiling
            .is_some_and(|ceiling| ceiling > 0 && target > ceiling)
        {
            policy.push((PROCESS_COUNT_CEILING_ENV, target));
        }
        policy.push((PROCESS_COUNT_LIMIT_ENV, target));
        format!(
            "process limit {} is {}; {}",
            self.process_count_limit,
            self.process_count_limit_origin(),
            runner_policy_action(&policy)
        )
    }

    fn process_count_limit_origin(&self) -> String {
        match self.process_count_limit_source.as_deref() {
            Some("default") => "the runner default (16/core)".to_string(),
            Some("runner_override") => "the runner's PROCESS_COUNT".to_string(),
            Some("runner_ceiling") => "the runner ceiling".to_string(),
            Some(source) => format!("from `{source}`"),
            None => "from an unrecorded source".to_string(),
        }
    }
}

fn runner_policy_action(policy: &[(&str, u64)]) -> String {
    let env = policy
        .iter()
        .map(|(key, value)| format!("\"{key}\":\"{value}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "run `homeboy runner set <runner> --json '{{\"env\":{{{env}}}}}'` then `homeboy runner service install <runner>`"
    )
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
                "top_processes": [{"name": "rustc", "count": 2}],
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

    fn process_violation(
        limit: u64,
        source: Option<&str>,
        ceiling: Option<u64>,
    ) -> RunnerResourceGuardViolation {
        RunnerResourceGuardViolation {
            reason: "process_count_limit_exceeded".to_string(),
            message: String::new(),
            rss_bytes: 0,
            rss_limit_bytes: 0,
            process_count: 291,
            process_count_limit: limit,
            process_count_limit_source: source.map(str::to_string),
            process_count_limit_ceiling: ceiling,
            top_processes: Vec::new(),
        }
    }

    const SERVICE_INSTALL: &str = "then `homeboy runner service install <runner>`";

    #[test]
    fn default_limit_remedy_names_the_runner_default_and_the_runner_config_action() {
        let remedy = process_violation(512, Some("default"), Some(1024)).remedy();
        assert_eq!(
            remedy,
            format!(
                "process limit 512 is the runner default (16/core); run `homeboy runner set <runner> --json '{{\"env\":{{\"HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT\":\"384\"}}}}'` {SERVICE_INSTALL}"
            )
        );
    }

    #[test]
    fn runner_override_remedy_names_the_runner_env_key() {
        let remedy = process_violation(256, Some("runner_override"), Some(1024)).remedy();
        assert!(remedy.starts_with("process limit 256 is the runner's PROCESS_COUNT; "));
        assert!(
            remedy.contains("{\"env\":{\"HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT\":\"384\"}}")
        );
        assert!(remedy.ends_with(SERVICE_INSTALL));
    }

    #[test]
    fn ceiling_remedy_raises_the_ceiling_with_the_limit() {
        let remedy = process_violation(256, Some("runner_ceiling"), Some(256)).remedy();
        assert_eq!(
            remedy,
            format!(
                "process limit 256 is the runner ceiling; run `homeboy runner set <runner> --json '{{\"env\":{{\"HOMEBOY_RUNNER_RESOURCE_GUARD_MAX_PROCESS_COUNT\":\"384\",\"HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT\":\"384\"}}}}'` {SERVICE_INSTALL}"
            )
        );
        assert!(remedy.len() <= 256, "fits the blocker projection bound");
    }

    #[test]
    fn unrecorded_or_retired_sources_get_the_same_runner_owned_action() {
        for (source, origin) in [
            (None, "from an unrecorded source"),
            (Some("job_override"), "from `job_override`"),
        ] {
            let remedy = process_violation(128, source, None).remedy();
            assert!(remedy.starts_with(&format!("process limit 128 is {origin}; ")));
            assert!(remedy.contains("homeboy runner set <runner> --json"));
            assert!(remedy.ends_with(SERVICE_INSTALL));
        }
    }

    #[test]
    fn remedies_never_suggest_flags_detached_cooks_refuse() {
        let rss = RunnerResourceGuardViolation {
            reason: "rss_limit_exceeded".to_string(),
            rss_bytes: 3 * GIB + 1,
            rss_limit_bytes: 3 * GIB,
            ..process_violation(128, Some("default"), Some(256))
        };
        assert!(rss
            .remedy()
            .contains("{\"env\":{\"HOMEBOY_RUNNER_RESOURCE_GUARD_RSS_BYTES\":\"5368709120\"}}"));
        for remedy in [
            rss.remedy(),
            process_violation(128, Some("default"), Some(256)).remedy(),
            process_violation(256, Some("runner_ceiling"), Some(256)).remedy(),
        ] {
            for refused in ["--runner-env", "--lab-env-json", "--gate-env", "resume"] {
                assert!(!remedy.contains(refused), "{remedy}");
            }
        }
    }

    #[test]
    fn observed_summary_carries_the_process_breakdown() {
        let mut violation = process_violation(256, Some("default"), Some(512));
        violation.top_processes = vec![
            RunnerProcessNameCount {
                name: "rustc".to_string(),
                count: 34,
            },
            RunnerProcessNameCount {
                name: "cc".to_string(),
                count: 21,
            },
        ];
        assert_eq!(
            violation.observed_summary(),
            "process_count=291 exceeds limit 256 (top: rustc\u{d7}34, cc\u{d7}21)"
        );
        let encoded = serde_json::to_value(&violation).expect("encode");
        assert_eq!(encoded["top_processes"][0]["name"], "rustc");
        assert_eq!(
            serde_json::from_value::<RunnerResourceGuardViolation>(encoded).expect("decode"),
            violation
        );
    }

    #[test]
    fn legacy_violation_without_ceiling_or_breakdown_round_trips() {
        let legacy: RunnerResourceGuardViolation = serde_json::from_value(serde_json::json!({
            "reason": "process_count_limit_exceeded",
            "message": "",
            "rss_bytes": 0,
            "rss_limit_bytes": 0,
            "process_count": 141,
            "process_count_limit": 128,
        }))
        .expect("legacy record");
        assert_eq!(legacy.process_count_limit_ceiling, None);
        assert!(legacy.top_processes.is_empty());
        assert_eq!(
            legacy.observed_summary(),
            "process_count=141 exceeds limit 128"
        );
        let encoded = serde_json::to_value(&legacy).expect("encode");
        assert!(encoded.get("process_count_limit_ceiling").is_none());
        assert!(encoded.get("top_processes").is_none());
    }
}
