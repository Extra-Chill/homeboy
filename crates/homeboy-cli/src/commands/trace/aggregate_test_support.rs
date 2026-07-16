use super::aggregate::TraceAggregateSpanSample;

pub(super) fn aggregate_samples(durations: &[u64]) -> Vec<TraceAggregateSpanSample> {
    durations
        .iter()
        .enumerate()
        .map(|(index, duration_ms)| TraceAggregateSpanSample {
            duration_ms: *duration_ms,
            run_index: index + 1,
            artifact_path: format!("/tmp/trace-run-{}.json", index + 1),
        })
        .collect()
}
