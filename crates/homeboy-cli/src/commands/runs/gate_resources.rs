//! Gate cost cohorts read controller-retained receipt metadata, not Lab paths.
use homeboy::core::observation::ObservationStore;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateCost {
    pub phase: String,
    pub count: usize,
    pub total_cpu_ms: u64,
    pub p50_cpu_ms: f64,
    pub total_cpu_user_ms: u64,
    pub total_cpu_system_ms: u64,
    pub peak_rss_bytes: u64,
    pub total_growth_bytes: u64,
    pub growth_sample_count: usize,
}

pub fn load(store: &ObservationStore, ids: &[String]) -> homeboy::core::Result<Vec<GateCost>> {
    let artifacts = store.list_artifacts_for_runs(ids)?;
    Ok(aggregate(
        artifacts
            .values()
            .flatten()
            .filter(|artifact| artifact.kind == "agent_task_gate_resources")
            .map(|artifact| &artifact.metadata_json),
    ))
}

fn aggregate<'a>(samples: impl Iterator<Item = &'a Value>) -> Vec<GateCost> {
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for sample in samples {
        if let Some(phase) = sample["phase"]
            .as_str()
            .filter(|phase| phase.starts_with("gate."))
        {
            // Historical receipts with no measurements are missing, not zero.
            if sample
                .pointer("/resources/cpu_user_ms")
                .and_then(Value::as_u64)
                .is_some()
                && sample
                    .pointer("/resources/cpu_system_ms")
                    .and_then(Value::as_u64)
                    .is_some()
            {
                groups.entry(phase.to_owned()).or_default().push(sample);
            }
        }
    }
    let mut result = Vec::new();
    for (phase, samples) in groups {
        let number =
            |sample: &Value, path: &str| sample.pointer(path).and_then(Value::as_u64).unwrap_or(0);
        let mut cpu = samples
            .iter()
            .map(|sample| {
                number(sample, "/resources/cpu_user_ms")
                    .saturating_add(number(sample, "/resources/cpu_system_ms"))
            })
            .collect::<Vec<_>>();
        cpu.sort_unstable();
        let middle = cpu.len() / 2;
        let p50 = if cpu.len() % 2 == 0 {
            cpu[middle - 1] as f64 / 2.0 + cpu[middle] as f64 / 2.0
        } else {
            cpu[middle] as f64
        };
        result.push(GateCost {
            phase,
            count: cpu.len(),
            total_cpu_ms: cpu.iter().sum(),
            p50_cpu_ms: p50,
            total_cpu_user_ms: samples
                .iter()
                .map(|sample| number(sample, "/resources/cpu_user_ms"))
                .sum(),
            total_cpu_system_ms: samples
                .iter()
                .map(|sample| number(sample, "/resources/cpu_system_ms"))
                .sum(),
            peak_rss_bytes: samples
                .iter()
                .map(|sample| number(sample, "/resources/peak_rss_bytes"))
                .max()
                .unwrap_or(0),
            total_growth_bytes: samples
                .iter()
                .map(|sample| number(sample, "/cargo_target/growth_bytes"))
                .sum(),
            growth_sample_count: samples
                .iter()
                .filter(|sample| {
                    sample
                        .pointer("/cargo_target/growth_bytes")
                        .and_then(Value::as_u64)
                        .is_some()
                })
                .count(),
        });
    }
    result.sort_by(|a, b| {
        b.total_cpu_ms
            .cmp(&a.total_cpu_ms)
            .then(a.phase.cmp(&b.phase))
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn runs_gate_cost_cohorts_keep_missing_distinct_from_zero() {
        let samples = [
            json!({"phase":"gate.cargo-test", "resources":{"cpu_user_ms":10,"cpu_system_ms":2,"peak_rss_bytes":100},"cargo_target":{"growth_bytes":50}}),
            json!({"phase":"gate.cargo-test", "resources":{"cpu_user_ms":30,"cpu_system_ms":6,"peak_rss_bytes":80}}),
            json!({"phase":"gate.cargo-test"}),
            json!({"phase":"gate.fmt", "resources":{"cpu_user_ms":0,"cpu_system_ms":0,"peak_rss_bytes":20},"cargo_target":{"growth_bytes":0}}),
        ];
        let costs = aggregate(samples.iter());
        assert_eq!(
            costs[0],
            GateCost {
                phase: "gate.cargo-test".into(),
                count: 2,
                total_cpu_ms: 48,
                p50_cpu_ms: 24.0,
                total_cpu_user_ms: 40,
                total_cpu_system_ms: 8,
                peak_rss_bytes: 100,
                total_growth_bytes: 50,
                growth_sample_count: 1
            }
        );
        assert_eq!(costs[1].count, 1);
        assert_eq!(costs[1].p50_cpu_ms, 0.0);
        assert_eq!(costs[1].growth_sample_count, 1);
    }
}
