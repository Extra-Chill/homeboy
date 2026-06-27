use clap::Args;
use serde::Serialize;
use std::cmp::Ordering;
use std::process::Command;

use crate::commands::CmdResult;

mod classification;
mod load;
mod memory;

use classification::{classify_processes, classify_rig_leases, overall_recommendation};

const RELEVANT_PROCESS_EXECUTABLES: &[&str] = &["homeboy"];
const RESOURCE_PROCESS_MATCHES_ENV: &str = "HOMEBOY_DOCTOR_RESOURCE_PROCESS_MATCHES";

#[derive(Args)]
pub struct ResourcesArgs {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRecommendation {
    Ok,
    Warm,
    Hot,
}

#[derive(Debug, Serialize)]
pub struct DoctorOutput {
    pub command: &'static str,
    pub recommendation: ResourceRecommendation,
    pub load: LoadSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySummary>,
    pub processes: ProcessSummary,
    pub rig_leases: RigLeaseSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fifteen: Option<f64>,
    pub cpu_count: usize,
    pub recommendation: ResourceRecommendation,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemorySummary {
    pub total_mb: u64,
    pub available_mb: u64,
    pub used_percent: f64,
    pub recommendation: ResourceRecommendation,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessSummary {
    pub relevant_count: usize,
    pub top_cpu: Vec<ProcessRow>,
    pub top_rss: Vec<ProcessRow>,
    pub recommendation: ResourceRecommendation,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessRow {
    pub pid: u32,
    pub cpu_percent: f64,
    pub rss_mb: u64,
    pub command: String,
    pub args: String,
}

#[derive(Debug, Serialize)]
pub struct RigLeaseSummary {
    pub active_count: usize,
    pub leases: Vec<RigLeaseRow>,
    pub recommendation: ResourceRecommendation,
}

#[derive(Debug, Serialize)]
pub struct RigLeaseRow {
    pub rig_id: String,
    pub command: String,
    pub pid: u32,
    pub started_at: String,
}

pub fn run(_args: ResourcesArgs) -> CmdResult<DoctorOutput> {
    run_with_mode(ResourceProbeMode::Full)
}

pub fn run_preflight() -> CmdResult<DoctorOutput> {
    run_with_mode(ResourceProbeMode::Preflight)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceProbeMode {
    Full,
    Preflight,
}

fn run_with_mode(mode: ResourceProbeMode) -> CmdResult<DoctorOutput> {
    let mut notes = Vec::new();
    let load = load::collect_load_summary();

    let memory = match mode {
        ResourceProbeMode::Full => match memory::collect_memory_summary() {
            Ok(summary) => Some(summary),
            Err(note) => {
                notes.push(note);
                None
            }
        },
        ResourceProbeMode::Preflight => None,
    };

    let processes = match mode {
        ResourceProbeMode::Full => match collect_process_summary() {
            Ok(summary) => summary,
            Err(note) => {
                notes.push(note);
                empty_process_summary()
            }
        },
        ResourceProbeMode::Preflight => empty_process_summary(),
    };

    let rig_leases = match mode {
        ResourceProbeMode::Full => match collect_rig_leases() {
            Ok(summary) => summary,
            Err(note) => {
                notes.push(note);
                empty_rig_lease_summary()
            }
        },
        ResourceProbeMode::Preflight => empty_rig_lease_summary(),
    };

    let recommendation = overall_recommendation(&[
        load.recommendation,
        memory
            .as_ref()
            .map(|summary| summary.recommendation)
            .unwrap_or(ResourceRecommendation::Ok),
        processes.recommendation,
        rig_leases.recommendation,
    ]);

    Ok((
        DoctorOutput {
            command: "self.resources",
            recommendation,
            load,
            memory,
            processes,
            rig_leases,
            notes,
        },
        0,
    ))
}

fn empty_process_summary() -> ProcessSummary {
    ProcessSummary {
        relevant_count: 0,
        top_cpu: Vec::new(),
        top_rss: Vec::new(),
        recommendation: ResourceRecommendation::Ok,
    }
}

fn empty_rig_lease_summary() -> RigLeaseSummary {
    RigLeaseSummary {
        active_count: 0,
        leases: Vec::new(),
        recommendation: ResourceRecommendation::Ok,
    }
}

fn collect_process_summary() -> Result<ProcessSummary, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,pcpu=,rss=,comm=,args="])
        .output()
        .map_err(|e| format!("process probe unavailable: {e}"))?;
    if !output.status.success() {
        return Err("process probe failed".to_string());
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut rows: Vec<ProcessRow> = raw
        .lines()
        .filter_map(parse_process_row)
        .filter(is_relevant_process)
        .collect();
    let recommendation = classify_processes(&rows);
    let relevant_count = rows.len();

    let mut top_cpu = rows.clone();
    top_cpu.sort_by(compare_cpu_desc);
    top_cpu.truncate(8);

    rows.sort_by(|a, b| b.rss_mb.cmp(&a.rss_mb).then_with(|| compare_cpu_desc(a, b)));
    rows.truncate(8);

    Ok(ProcessSummary {
        relevant_count,
        top_cpu,
        top_rss: rows,
        recommendation,
    })
}

fn parse_process_row(line: &str) -> Option<ProcessRow> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let cpu_percent = parts.next()?.parse::<f64>().ok()?;
    let rss_kb = parts.next()?.parse::<u64>().ok()?;
    let command = parts.next()?.to_string();
    let args = parts.collect::<Vec<_>>().join(" ");
    Some(ProcessRow {
        pid,
        cpu_percent: round1(cpu_percent),
        rss_mb: rss_kb / 1024,
        command,
        args,
    })
}

fn is_relevant_process(row: &ProcessRow) -> bool {
    is_relevant_process_with_matches(row, &configured_process_matches())
}

fn is_relevant_process_with_matches(row: &ProcessRow, configured_matches: &[String]) -> bool {
    let command_name = executable_name(&row.command);
    let arg0_name = row
        .args
        .split_whitespace()
        .next()
        .map(executable_name)
        .unwrap_or_default();
    if RELEVANT_PROCESS_EXECUTABLES
        .iter()
        .any(|needle| command_name == *needle || arg0_name == *needle)
    {
        return true;
    }

    let haystack = format!("{} {}", row.command, row.args).to_lowercase();
    configured_matches.iter().any(|needle| {
        let needle = needle.to_lowercase();
        command_name == needle || arg0_name == needle || haystack.contains(&needle)
    })
}

fn configured_process_matches() -> Vec<String> {
    std::env::var(RESOURCE_PROCESS_MATCHES_ENV)
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn executable_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase()
}

fn compare_cpu_desc(a: &ProcessRow, b: &ProcessRow) -> Ordering {
    b.cpu_percent
        .partial_cmp(&a.cpu_percent)
        .unwrap_or(Ordering::Equal)
        .then_with(|| b.rss_mb.cmp(&a.rss_mb))
}

fn collect_rig_leases() -> Result<RigLeaseSummary, String> {
    let leases = homeboy::core::rig::active_run_leases()
        .map_err(|e| format!("rig lease probe failed: {e}"))?;
    let rows: Vec<RigLeaseRow> = leases
        .into_iter()
        .map(|lease| RigLeaseRow {
            rig_id: lease.rig_id,
            command: lease.command,
            pid: lease.pid,
            started_at: lease.started_at,
        })
        .collect();
    let recommendation = classify_rig_leases(rows.len());

    Ok(RigLeaseSummary {
        active_count: rows.len(),
        leases: rows,
        recommendation,
    })
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relevant_process_rows_without_using_host_processes() {
        let row =
            parse_process_row("123 88.5 1048576 /usr/bin/homeboy homeboy worker run").unwrap();
        assert_eq!(row.pid, 123);
        assert_eq!(row.cpu_percent, 88.5);
        assert_eq!(row.rss_mb, 1024);
        assert!(is_relevant_process(&row));
    }

    #[test]
    fn configured_matches_select_additional_processes() {
        let row = ProcessRow {
            pid: 2,
            cpu_percent: 1.0,
            rss_mb: 100,
            command: "/usr/local/bin/preview-worker".to_string(),
            args: "serve --public".to_string(),
        };

        assert!(is_relevant_process_with_matches(
            &row,
            &["preview-worker".to_string()]
        ));
    }

    #[test]
    fn ignores_unrelated_processes_without_configured_match() {
        let row = ProcessRow {
            pid: 2,
            cpu_percent: 1.0,
            rss_mb: 100,
            command: "/Applications/Chat.app/Chat Helper".to_string(),
            args: "--enable-worker-mode".to_string(),
        };

        assert!(!is_relevant_process(&row));
    }

    #[test]
    fn preflight_probe_uses_load_only_snapshot() {
        let (output, _) = run_preflight().expect("preflight resources");

        assert!(output.memory.is_none());
        assert_eq!(output.processes.relevant_count, 0);
        assert!(output.processes.top_cpu.is_empty());
        assert!(output.processes.top_rss.is_empty());
        assert_eq!(output.rig_leases.active_count, 0);
        assert!(output.rig_leases.leases.is_empty());
    }
}
