use std::fs;
use std::process::Command;

use super::classification::classify_memory;
use super::{round1, MemorySummary};

pub(super) fn collect_memory_summary() -> Result<MemorySummary, String> {
    memory_from_proc_meminfo()
        .or_else(memory_from_vm_stat)
        .ok_or_else(|| "memory probe unavailable on this platform".to_string())
}

fn memory_from_proc_meminfo() -> Option<MemorySummary> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    let total_kb = meminfo_value_kb(&raw, "MemTotal")?;
    let available_kb = meminfo_value_kb(&raw, "MemAvailable")?;
    Some(memory_summary_from_bytes(
        total_kb.saturating_mul(1024),
        available_kb.saturating_mul(1024),
    ))
}

fn meminfo_value_kb(raw: &str, key: &str) -> Option<u64> {
    raw.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name != key {
            return None;
        }
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

fn memory_from_vm_stat() -> Option<MemorySummary> {
    let total_output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !total_output.status.success() {
        return None;
    }
    let total_bytes = String::from_utf8_lossy(&total_output.stdout)
        .trim()
        .parse::<u64>()
        .ok()?;

    let vm_output = Command::new("vm_stat").output().ok()?;
    if !vm_output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&vm_output.stdout);
    let page_size = vm_page_size(&raw).unwrap_or(4096);
    let free_pages = vm_stat_pages(&raw, "Pages free")
        .unwrap_or(0)
        .saturating_add(vm_stat_pages(&raw, "Pages inactive").unwrap_or(0))
        .saturating_add(vm_stat_pages(&raw, "Pages speculative").unwrap_or(0));

    Some(memory_summary_from_bytes(
        total_bytes,
        free_pages.saturating_mul(page_size),
    ))
}

fn vm_page_size(raw: &str) -> Option<u64> {
    let start = raw.find("page size of ")? + "page size of ".len();
    raw[start..].split_whitespace().next()?.parse::<u64>().ok()
}

fn vm_stat_pages(raw: &str, key: &str) -> Option<u64> {
    raw.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name.trim() != key {
            return None;
        }
        rest.trim()
            .trim_end_matches('.')
            .replace('.', "")
            .parse::<u64>()
            .ok()
    })
}

fn memory_summary_from_bytes(total_bytes: u64, available_bytes: u64) -> MemorySummary {
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
