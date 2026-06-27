use homeboy::core::resources::memory::{probe_system_memory, SystemMemory};

use super::classification::classify_memory;
use super::{round1, MemorySummary};

pub(super) fn collect_memory_summary() -> Result<MemorySummary, String> {
    probe_system_memory()
        .map(memory_summary_from_probe)
        .ok_or_else(|| "memory probe unavailable on this platform".to_string())
}

fn memory_summary_from_probe(probe: SystemMemory) -> MemorySummary {
    let SystemMemory {
        total_bytes,
        available_bytes,
    } = probe;
    let total_mb = bytes_to_mb(total_bytes);
    let available_mb = bytes_to_mb(available_bytes);
    let used_percent = if total_bytes == 0 {
        0.0
    } else {
        ((total_bytes.saturating_sub(available_bytes)) as f64 / total_bytes as f64) * 100.0
    };
    let recommendation = classify_memory(total_bytes, available_bytes);

    MemorySummary {
        total_mb,
        available_mb,
        used_percent: round1(used_percent),
        recommendation,
    }
}

fn bytes_to_mb(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}
