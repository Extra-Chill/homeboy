//! Pure bench responsiveness/memory-sample contract types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchResponsivenessSummary {
    pub missed_ping_count: u64,
    pub max_ping_gap_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ping_at: Option<String>,
    pub ping_count: u64,
    pub missed_ping_window_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchFailureMemorySample {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampled_peak_rss_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampled_peak_cpu_percent: Option<f64>,
}

impl BenchResponsivenessSummary {
    pub fn responsiveness_lost(&self) -> bool {
        self.missed_ping_count > 0
    }
}
