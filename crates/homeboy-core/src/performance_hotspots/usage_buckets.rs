//! Cook usage grouped by recorded attempt identity and phase vocabulary.
use std::collections::BTreeMap;

use homeboy_engine_primitives::phase_timing::{PhaseSpan, PhaseStatus};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct BucketUsage {
    pub bucket: &'static str,
    pub cpu_seconds: f64,
    pub homeboy_cpu_seconds: f64,
    /// Total minus Homeboy CPU, only when usage was recorded.
    pub child_cpu_seconds: Option<f64>,
    /// Sum of span durations, not elapsed time (spans may overlap).
    pub wall_ms: f64,
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttemptUsage {
    /// `None` means the spans did not carry an attempt identity.
    pub attempt_id: Option<String>,
    pub buckets: Vec<BucketUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CookUsage {
    pub run_id: String,
    pub attempts: Vec<AttemptUsage>,
}

/// Partition all non-skipped spans without inventing attempt identities or
/// measurements. Every span belongs to exactly one bucket. These are inclusive
/// span totals, not a reconstruction of exclusive CPU or elapsed run time.
pub fn usage_buckets(spans: &[PhaseSpan]) -> Vec<AttemptUsage> {
    let mut attempts = BTreeMap::new();
    for span in spans
        .iter()
        .filter(|span| span.status != PhaseStatus::Skipped)
    {
        let buckets = attempts.entry(span.attempt_id.clone()).or_insert_with(|| {
            ["setup", "agent", "verify", "finalize", "other"].map(|bucket| BucketUsage {
                bucket,
                ..Default::default()
            })
        });
        let id = span.id.as_str();
        let index = if id.starts_with("stage.") || id.starts_with("admit.") || id == "schedule" {
            0
        } else if id.starts_with("provider.") || id == "cook.execution" {
            1
        } else if id.starts_with("gate.") {
            2
        } else if id.starts_with("publish.") || id.starts_with("terminal.") {
            3
        } else {
            4
        };
        let bucket = &mut buckets[index];
        bucket.wall_ms += span.duration_ms.unwrap_or_else(|| {
            span.usage
                .as_ref()
                .map_or(0.0, |usage| usage.wall_ms as f64)
        });
        if let Some(usage) = &span.usage {
            bucket.cpu_seconds += usage.cpu_usage_usec as f64 / 1_000_000.0;
            bucket.homeboy_cpu_seconds += usage.homeboy_cpu_usec as f64 / 1_000_000.0;
            *bucket.child_cpu_seconds.get_or_insert(0.0) +=
                usage.cpu_usage_usec.saturating_sub(usage.homeboy_cpu_usec) as f64 / 1_000_000.0;
            bucket.estimated |= usage.estimated;
        }
    }
    attempts
        .into_iter()
        .map(|(attempt_id, buckets)| AttemptUsage {
            attempt_id,
            buckets: buckets.into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partitions_spans_by_bucket_and_recorded_attempt() {
        let cases = [
            ("stage.workspace", 0),
            ("admit.policy", 0),
            ("schedule", 0),
            ("provider.command", 1),
            ("provider.poll", 1),
            ("cook.execution", 1),
            ("gate.test", 2),
            ("publish.result", 3),
            ("terminal.decision", 3),
            ("unknown", 4),
            ("stagecoach", 4),
            ("gate", 4),
        ];
        let mut spans = Vec::new();
        for attempt in [None, Some("attempt-1"), Some("attempt-2")] {
            for (id, _) in cases {
                spans.push(
                    serde_json::from_value(json!({
                        "id": id, "attempt_id": attempt, "status": "failed",
                        "started_at": "2026-11-19T00:00:00Z", "duration_ms": 2.5,
                        "usage": {"cpu_usage_usec": 10, "cpu_user_usec": 8,
                            "cpu_system_usec": 2, "homeboy_cpu_usec": 4,
                            "wall_ms": 2, "source": "rusage", "estimated": true}
                    }))
                    .unwrap(),
                );
            }
        }
        let result = usage_buckets(&spans);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].attempt_id, None);
        assert_eq!(result[1].attempt_id.as_deref(), Some("attempt-1"));
        for attempt in result {
            for (index, bucket) in attempt.buckets.iter().enumerate() {
                let count = cases.iter().filter(|(_, bucket)| *bucket == index).count() as f64;
                assert!((bucket.cpu_seconds - count * 0.000010).abs() < 1e-12);
                assert!((bucket.homeboy_cpu_seconds - count * 0.000004).abs() < 1e-12);
                assert!((bucket.child_cpu_seconds.unwrap() - count * 0.000006).abs() < 1e-12);
                assert_eq!(bucket.wall_ms, count * 2.5);
                assert!(bucket.estimated);
            }
            assert_eq!(attempt.buckets.iter().map(|b| b.wall_ms).sum::<f64>(), 30.0);
        }
        let unmeasured: PhaseSpan = serde_json::from_value(json!({
            "id": "gate.test", "status": "ok", "started_at": "2026-11-19T00:00:00Z",
            "duration_ms": 7.0
        }))
        .unwrap();
        let mut skipped = unmeasured.clone();
        skipped.status = PhaseStatus::Skipped;
        let rows = usage_buckets(&[unmeasured, skipped]);
        assert_eq!(rows[0].buckets[2].wall_ms, 7.0);
        assert_eq!(rows[0].buckets[2].child_cpu_seconds, None);
        assert!(usage_buckets(&[]).is_empty());
    }
}
