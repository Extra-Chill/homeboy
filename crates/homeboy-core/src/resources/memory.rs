//! System memory probing.
//!
//! Generic, platform-aware probing of total/available physical memory. This is
//! pure system-resource orchestration (reading `/proc/meminfo`, invoking
//! `sysctl`/`vm_stat`) and therefore lives in core rather than the command
//! layer. Callers map the raw byte counts into their own presentation types.

use std::fs;
use std::process::Command;

/// Raw physical memory snapshot in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemMemory {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Probe physical memory, returning total/available byte counts.
///
/// Tries the Linux `/proc/meminfo` interface first, then falls back to the
/// macOS `sysctl`/`vm_stat` interface. Returns `None` when no supported probe
/// is available on the host.
pub fn probe_system_memory() -> Option<SystemMemory> {
    memory_from_proc_meminfo().or_else(memory_from_vm_stat)
}

fn memory_from_proc_meminfo() -> Option<SystemMemory> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    let total_kb = meminfo_value_kb(&raw, "MemTotal")?;
    let available_kb = meminfo_value_kb(&raw, "MemAvailable")?;
    Some(SystemMemory {
        total_bytes: total_kb.saturating_mul(1024),
        available_bytes: available_kb.saturating_mul(1024),
    })
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

fn memory_from_vm_stat() -> Option<SystemMemory> {
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

    Some(SystemMemory {
        total_bytes,
        available_bytes: free_pages.saturating_mul(page_size),
    })
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
