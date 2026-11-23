//! Best-effort local host telemetry shared by all execution layers.

use serde::{Deserialize, Serialize};
use std::{fs, process::Command};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

/// Sample the host's one-, five-, and fifteen-minute load averages.
pub fn load_average() -> Option<LoadAverage> {
    #[cfg(unix)]
    {
        let mut values = [0.0_f64; 3];
        // SAFETY: values has room for the requested three samples.
        let count = unsafe { libc::getloadavg(values.as_mut_ptr(), 3) };
        (count == 3).then(|| LoadAverage {
            one: values[0],
            five: values[1],
            fifteen: values[2],
        })
    }
    #[cfg(not(unix))]
    None
}

/// Physical memory in bytes; unavailable measurements remain unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemMemory {
    pub total_bytes: u64,
    pub available_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
}

pub fn probe_system_memory() -> Option<SystemMemory> {
    memory_from_proc_meminfo().or_else(memory_from_vm_stat)
}

fn memory_from_proc_meminfo() -> Option<SystemMemory> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    let bytes = |key| meminfo_value_kb(&raw, key).map(|kb| kb.saturating_mul(1024));
    Some(SystemMemory {
        total_bytes: bytes("MemTotal")?,
        available_bytes: bytes("MemAvailable"),
        swap_used_bytes: bytes("SwapTotal")
            .zip(bytes("SwapFree"))
            .map(|(total, free)| total.saturating_sub(free)),
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
        .parse()
        .ok()?;
    let available_bytes = Command::new("vm_stat").output().ok().and_then(|output| {
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let page_size = vm_page_size(&raw).unwrap_or(4096);
        let free_pages = vm_stat_pages(&raw, "Pages free")
            .unwrap_or(0)
            .saturating_add(vm_stat_pages(&raw, "Pages inactive").unwrap_or(0))
            .saturating_add(vm_stat_pages(&raw, "Pages speculative").unwrap_or(0));
        Some(free_pages.saturating_mul(page_size))
    });
    Some(SystemMemory {
        total_bytes,
        available_bytes,
        swap_used_bytes: None,
    })
}

fn vm_page_size(raw: &str) -> Option<u64> {
    let start = raw.find("page size of ")? + "page size of ".len();
    raw[start..].split_whitespace().next()?.parse().ok()
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
            .parse()
            .ok()
    })
}
