use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::engine::resource::ExtensionChildResourceSummary;
use crate::core::error::{Error, Result};

const DEFAULT_MISSED_PING_WINDOW_MS: u64 = 10_000;

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

#[derive(Debug, Clone, Deserialize)]
struct BenchResponsivenessPing {
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    t_ms: Option<u64>,
}

impl BenchResponsivenessSummary {
    pub fn responsiveness_lost(&self) -> bool {
        self.missed_ping_count > 0
    }
}

pub fn missed_ping_window_ms() -> u64 {
    std::env::var("HOMEBOY_BENCH_RESPONSIVENESS_MISSED_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MISSED_PING_WINDOW_MS)
}

pub fn read_responsiveness_summary(
    path: &Path,
    observed_elapsed_ms: Option<u128>,
) -> Result<Option<BenchResponsivenessSummary>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read bench responsiveness file {}: {}",
                path.display(),
                e
            ),
            Some("bench.responsiveness.read".to_string()),
        )
    })?;

    summarize_responsiveness_pings(&content, missed_ping_window_ms(), observed_elapsed_ms).map(Some)
}

pub fn summarize_responsiveness_pings(
    raw: &str,
    missed_ping_window_ms: u64,
    observed_elapsed_ms: Option<u128>,
) -> Result<BenchResponsivenessSummary> {
    let mut pings = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let ping: BenchResponsivenessPing = serde_json::from_str(trimmed).map_err(|e| {
            Error::internal_json(
                format!(
                    "Failed to parse bench responsiveness ping {}: {}",
                    index + 1,
                    e
                ),
                Some("bench.responsiveness.deserialize".to_string()),
            )
        })?;
        pings.push(ping);
    }

    let mut max_ping_gap_ms = 0;
    let mut missed_ping_count = 0;
    let mut previous_t_ms = None;
    let mut last_ping_at = None;

    let mut accumulate_gap = |current: u64, previous: u64| {
        let gap = current.saturating_sub(previous);
        max_ping_gap_ms = max_ping_gap_ms.max(gap);
        if gap > missed_ping_window_ms {
            missed_ping_count += gap / missed_ping_window_ms;
        }
    };

    for ping in &pings {
        if ping.at.is_some() {
            last_ping_at = ping.at.clone();
        }
        if let Some(t_ms) = ping.t_ms {
            if let Some(previous) = previous_t_ms {
                accumulate_gap(t_ms, previous);
            }
            previous_t_ms = Some(t_ms);
        }
    }

    if let (Some(previous), Some(observed_elapsed_ms)) = (previous_t_ms, observed_elapsed_ms) {
        let observed_elapsed_ms = observed_elapsed_ms.min(u128::from(u64::MAX)) as u64;
        accumulate_gap(observed_elapsed_ms, previous);
    }

    Ok(BenchResponsivenessSummary {
        missed_ping_count,
        max_ping_gap_ms,
        last_ping_at,
        ping_count: pings.len() as u64,
        missed_ping_window_ms,
    })
}

pub fn memory_sample(
    child_resource: Option<&ExtensionChildResourceSummary>,
) -> Option<BenchFailureMemorySample> {
    child_resource.map(|resource| BenchFailureMemorySample {
        sampled_peak_rss_bytes: resource.sampled_peak_rss_bytes,
        sampled_peak_cpu_percent: resource.sampled_peak_cpu_percent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_ping_gaps_and_last_ping() {
        let summary = summarize_responsiveness_pings(
            r#"{"at":"2026-06-08T00:00:00Z","t_ms":0}
{"at":"2026-06-08T00:00:02Z","t_ms":2000}
{"at":"2026-06-08T00:00:18Z","t_ms":18000}
"#,
            5_000,
            None,
        )
        .expect("summary");

        assert_eq!(summary.ping_count, 3);
        assert_eq!(summary.max_ping_gap_ms, 16_000);
        assert_eq!(summary.missed_ping_count, 3);
        assert_eq!(
            summary.last_ping_at.as_deref(),
            Some("2026-06-08T00:00:18Z")
        );
    }

    #[test]
    fn summarizes_final_gap_after_last_ping() {
        let summary = summarize_responsiveness_pings(
            r#"{"at":"2026-06-08T00:00:00Z","t_ms":0}
{"at":"2026-06-08T00:00:02Z","t_ms":2000}
"#,
            5_000,
            Some(18_000),
        )
        .expect("summary");

        assert_eq!(summary.max_ping_gap_ms, 16_000);
        assert_eq!(summary.missed_ping_count, 3);
    }
}
