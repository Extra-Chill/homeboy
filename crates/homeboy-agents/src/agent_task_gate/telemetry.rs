//! Best-effort evidence only: never change a gate verdict on sampling failure.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateResources {
    pub cpu_user_ms: u64,
    pub cpu_system_ms: u64,
    pub peak_rss_bytes: u64,
}

pub fn host_pressure() -> serde_json::Value {
    serde_json::json!({
        "load": homeboy_engine_primitives::host_telemetry::load_average(),
        "psi": {
            "cpu": std::fs::read_to_string("/proc/pressure/cpu").ok(),
            "memory": std::fs::read_to_string("/proc/pressure/memory").ok(),
            "io": std::fs::read_to_string("/proc/pressure/io").ok(),
        }
    })
}

/// gate-execute is a dedicated process. RUSAGE_CHILDREN includes waited-for
/// descendants (including short-lived rustc processes a polling sampler misses).
/// RSS is a high-water mark, NOT a delta or a sum of simultaneous resident sets.
pub fn children() -> Option<GateResources> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the supplied rusage on success.
        if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        let usage = unsafe { usage.assume_init() };
        let millis = |time: libc::timeval| {
            (time.tv_sec as u64).saturating_mul(1000) + time.tv_usec as u64 / 1000
        };
        let rss = usage.ru_maxrss as u64;
        Some(GateResources {
            cpu_user_ms: millis(usage.ru_utime),
            cpu_system_ms: millis(usage.ru_stime),
            peak_rss_bytes: if cfg!(target_os = "macos") {
                rss
            } else {
                rss.saturating_mul(1024)
            },
        })
    }
    #[cfg(not(unix))]
    None
}

pub fn delta(before: Option<GateResources>, after: Option<GateResources>) -> Option<GateResources> {
    let (before, mut after) = (before?, after?);
    after.cpu_user_ms = after.cpu_user_ms.saturating_sub(before.cpu_user_ms);
    after.cpu_system_ms = after.cpu_system_ms.saturating_sub(before.cpu_system_ms);
    Some(after)
}

/// Called on the controller AFTER dispatch, so Lab receipt metrics are retained
/// in the same observation store as local gates, without private command output.
pub fn record(environment: &super::AgentTaskGateEnvironment) {
    let Some((path, run_id)) = homeboy_core::observation::phase_timing::current() else {
        return;
    };
    if environment.resources.is_none() {
        return;
    }
    let result = (|| -> homeboy_core::Result<()> {
        let store = homeboy_core::observation::ObservationStore::open_initialized_at(path)?;
        let file = tempfile::NamedTempFile::new()
            .map_err(|error| homeboy_core::Error::internal_unexpected(error.to_string()))?;
        let metrics = serde_json::json!({
            "phase": environment.phase_name,
            "resources": environment.resources,
            "cargo_target": environment.cargo_target.as_ref().map(|target| serde_json::json!({
                "state": target.state, "acquire_ms": target.acquire_ms,
                "seed_ms": target.seed_ms, "seed_bytes": target.seed_bytes,
                "growth_bytes": target.growth_bytes,
            })),
            "host_pressure_start": environment.host_pressure_start,
            "host_pressure_end": environment.host_pressure_end,
        });
        serde_json::to_writer(file.as_file(), &metrics)
            .map_err(|error| homeboy_core::Error::internal_unexpected(error.to_string()))?;
        store.record_artifact_with_metadata(
            &run_id,
            "agent_task_gate_resources",
            file.path(),
            metrics,
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        homeboy_core::log_status!("observe", "gate resource persistence failed: {}", error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn agent_task_gate_receipt_metrics_reach_controller_observations() {
        let root = tempfile::tempdir().unwrap();
        let store = homeboy_core::observation::ObservationStore::open_initialized_at(
            root.path().join("observations.db"),
        )
        .unwrap();
        let run = store
            .start_run(homeboy_core::observation::NewRunRecord::builder("agent-task").build())
            .unwrap();
        let _scope = homeboy_core::observation::phase_timing::for_run(&store, &run.id);
        // Simulate the deserialized environment of a Lab receipt on a controller
        // that cannot access any of the runner's paths.
        let environment: super::super::AgentTaskGateEnvironment = serde_json::from_value(serde_json::json!({
            "lab_runner":"lab", "phase_name":"gate.cargo-test",
            "resources":{"cpu_user_ms":42,"cpu_system_ms":7,"peak_rss_bytes":4096},
            "host_pressure_start":{"load":{"one":1.0,"five":2.0,"fifteen":3.0}},
            "host_pressure_end":{"load":{"one":4.0,"five":5.0,"fifteen":6.0}},
            "cargo_target":{"path":"/unreachable/lab/target", "resolution":"isolated", "owner":"agent-task-gate", "state":"seeded", "acquire_ms":3, "seed_ms":2, "seed_bytes":100, "growth_bytes":20}
        })).unwrap();
        record(&environment);
        let artifacts = store.list_artifacts(&run.id).unwrap();
        assert_eq!(artifacts.len(), 1);
        let metrics = &artifacts[0].metadata_json;
        assert_eq!(metrics["resources"]["cpu_user_ms"], 42);
        assert_eq!(metrics["cargo_target"]["growth_bytes"], 20);
        assert_eq!(metrics["host_pressure_end"]["load"]["one"], 4.0);
        assert!(metrics["cargo_target"].get("path").is_none());
    }

    #[test]
    fn agent_task_gate_resource_delta_keeps_peak_not_difference() {
        for (start, end, expected) in [(10, 30, 20), (30, 10, 0)] {
            let sample = |cpu| {
                Some(GateResources {
                    cpu_user_ms: cpu,
                    cpu_system_ms: cpu,
                    peak_rss_bytes: 4096,
                })
            };
            let result = delta(sample(start), sample(end)).unwrap();
            assert_eq!(result.cpu_user_ms, expected);
            assert_eq!(result.cpu_system_ms, expected);
            assert_eq!(result.peak_rss_bytes, 4096);
        }
        assert!(delta(None, children()).is_none());
    }
}
