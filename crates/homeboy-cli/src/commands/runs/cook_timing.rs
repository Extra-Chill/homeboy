//! Shared submission-to-publication timing from durable Cook evidence.
use homeboy::core::observation::{ObservationStore, RunRecord};
use serde::Serialize;
use serde_json::Value;

pub(super) fn submission_to_pr_ms(
    store: &ObservationStore,
    run: &RunRecord,
) -> homeboy::core::Result<Option<f64>> {
    if run
        .metadata_json
        .pointer("/agent_task_run/metadata/cook_delivery/delivery/publication/published")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Ok(None);
    }
    Ok(store
        .phase_spans(&run.id)?
        .span("publish")
        .filter(|span| span.is_ok())
        .and_then(|span| {
            let submitted = chrono::DateTime::parse_from_rfc3339(&run.started_at).ok()?;
            let published = chrono::DateTime::parse_from_rfc3339(&span.started_at).ok()?;
            Some(
                (published - submitted).num_milliseconds().max(0) as f64
                    + span.duration_ms.unwrap_or(0.0),
            )
        }))
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CookWallTimeSummary {
    pub count: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub mean_ms: Option<f64>,
}

impl CookWallTimeSummary {
    pub(super) fn from_samples(mut samples: Vec<f64>) -> Self {
        samples.retain(|ms| ms.is_finite() && *ms >= 0.0);
        samples.sort_by(f64::total_cmp);
        let count = samples.len();
        let percentile = |p: f64| {
            (count > 0).then(|| samples[((count as f64 * p).ceil() as usize).saturating_sub(1)])
        };
        Self {
            count,
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            mean_ms: (count > 0).then(|| samples.iter().sum::<f64>() / count as f64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_uses_nearest_rank_and_handles_empty_samples() {
        for (samples, count, p50, p95, mean) in [
            (vec![], 0, None, None, None),
            (vec![42.0], 1, Some(42.0), Some(42.0), Some(42.0)),
            (
                vec![40.0, 10.0, 30.0, 20.0],
                4,
                Some(20.0),
                Some(40.0),
                Some(25.0),
            ),
        ] {
            assert_eq!(
                CookWallTimeSummary::from_samples(samples),
                CookWallTimeSummary {
                    count,
                    p50_ms: p50,
                    p95_ms: p95,
                    mean_ms: mean,
                }
            );
        }
    }
}
