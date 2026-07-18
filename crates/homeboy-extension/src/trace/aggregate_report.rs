use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct TraceAggregateSpanSampleOutput {
    pub run_index: usize,
    pub duration_ms: u64,
    pub artifact_path: String,
}
