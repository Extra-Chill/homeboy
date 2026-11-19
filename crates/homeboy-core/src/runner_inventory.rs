//! Runner pressure: host disk, memory, and load plus the size of every
//! Homeboy-owned root.
//!
//! Root sizes come from a recorded estimate when one is fresh, and otherwise
//! from a bounded, non-following, deadline-limited walk. A walk that runs out
//! of time reports a lower bound marked `estimated`; only a walk that visited
//! every entry is `measured`. Every walk is recorded in process memory, so the
//! daemon serving repeated reports on a busy runner reuses it instead of
//! walking again. Reporting never writes to disk: `status` stays read-only.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PRESSURE_SCHEMA: &str = "homeboy/runner-pressure/v1";

/// A recorded estimate younger than this is served instead of walking again.
const RECORD_TTL: Duration = Duration::from_secs(10 * 60);
/// Bound memory as well as time for very wide directories.
const MAX_PENDING: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    /// A walk visited every entry during this report.
    Measured,
    /// A lower bound from a deadline-limited walk, or a recorded estimate.
    Estimated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeSource {
    Walk,
    Recorded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootUsage {
    pub name: String,
    pub root: PathBuf,
    pub bytes: u64,
    pub entries: u64,
    pub measurement: Measurement,
    pub source: SizeSource,
    pub complete: bool,
    pub errors: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeUsage {
    pub path: String,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub total_bytes: u64,
    pub available_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadUsage {
    pub load_1m: f64,
    pub logical_cpus: usize,
    pub load_per_cpu: f64,
}

/// One bounded pressure observation of the host holding Homeboy data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PressureReport {
    pub schema: String,
    pub disk: Option<VolumeUsage>,
    pub memory: Option<MemoryUsage>,
    pub load: Option<LoadUsage>,
    pub roots: Vec<RootUsage>,
    pub deadline_ms: u64,
    pub elapsed_ms: u64,
}

/// The Homeboy-owned roots, by name. `_lab_workspaces` is only known for a
/// runner with a workspace root.
pub fn owned_roots(
    workspace_root: Option<PathBuf>,
) -> crate::error::Result<Vec<(String, PathBuf)>> {
    let data = crate::paths::homeboy_data()?;
    let daemon = crate::paths::daemon_state_file()?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate::paths::homeboy().unwrap_or_default().join("daemon"));
    let mut roots = vec![
        (
            "controller-scratch",
            crate::paths::controller_scratch_store()?,
        ),
        ("task-worktrees", data.join("task-worktrees")),
        ("runtime/tmp", data.join("runtime/tmp")),
        ("gate-inputs", data.join("gate-inputs")),
        ("daemon", daemon),
        ("artifacts", crate::paths::artifact_root()?),
        ("cargo-targets", crate::cleanup::shared_cargo_target_root()?),
        // Managed cargo/rustup packages for isolated gates.
        (
            "gate-rust-cache",
            data.join("controller-state/gate-rust-cache"),
        ),
        (
            "playwright-browsers",
            crate::paths::home_root()?.join(".cache/homeboy/playwright-browsers"),
        ),
    ];
    if let Some(root) = workspace_root {
        roots.insert(1, ("_lab_workspaces", root.join("_lab_workspaces")));
    }
    Ok(roots
        .into_iter()
        .map(|(name, path)| (name.to_string(), path))
        .collect())
}

/// Observe runner pressure within `budget`. Host probes are constant-time; the
/// budget bounds the root walks.
pub fn pressure_report(
    workspace_root: Option<PathBuf>,
    budget: Duration,
) -> crate::error::Result<PressureReport> {
    let started = Instant::now();
    let data = crate::paths::homeboy_data()?;
    static RECORD: std::sync::Mutex<SizeRecord> = std::sync::Mutex::new(SizeRecord {
        roots: BTreeMap::new(),
    });
    let roots = owned_roots(workspace_root)?;
    Ok(PressureReport {
        schema: PRESSURE_SCHEMA.to_string(),
        disk: volume_usage(&data),
        memory: memory_usage(),
        load: load_usage(),
        roots: root_sizes(roots, budget, &RECORD),
        deadline_ms: budget.as_millis() as u64,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[derive(Debug, Default)]
struct SizeRecord {
    roots: BTreeMap<String, RootUsage>,
}

/// Serve fresh recorded estimates, walk the rest, and record what was walked.
/// Concurrent requests never wait on each other's walk: a request that finds
/// the record held reports its roots unwalked.
fn root_sizes(
    roots: Vec<(String, PathBuf)>,
    budget: Duration,
    record: &std::sync::Mutex<SizeRecord>,
) -> Vec<RootUsage> {
    let Ok(mut record) = record.try_lock() else {
        return roots
            .into_iter()
            .map(|(name, root)| unwalked(name, root))
            .collect();
    };
    let now = chrono::Utc::now();
    let deadline = Instant::now() + budget;
    let total = roots.len();
    roots
        .into_iter()
        .enumerate()
        .map(|(index, (name, root))| {
            if let Some(recorded) = record
                .roots
                .get(&name)
                .filter(|recorded| recorded.root == root)
                .filter(|recorded| record_is_fresh(recorded, now))
            {
                return RootUsage {
                    measurement: Measurement::Estimated,
                    source: SizeSource::Recorded,
                    ..recorded.clone()
                };
            }
            // Share the remaining budget across the roots still to walk, so
            // one huge root cannot starve the others.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = remaining / (total - index) as u32;
            let mut usage = walk(name.clone(), root, Instant::now() + slice);
            usage.recorded_at = Some(now.to_rfc3339());
            record.roots.insert(name, usage.clone());
            usage
        })
        .collect()
}

fn record_is_fresh(recorded: &RootUsage, now: chrono::DateTime<chrono::Utc>) -> bool {
    recorded
        .recorded_at
        .as_deref()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .and_then(|at| (now - at.with_timezone(&chrono::Utc)).to_std().ok())
        .is_some_and(|age| age < RECORD_TTL)
}

fn unwalked(name: String, root: PathBuf) -> RootUsage {
    RootUsage {
        name,
        root,
        bytes: 0,
        entries: 0,
        measurement: Measurement::Estimated,
        source: SizeSource::Walk,
        complete: false,
        errors: 0,
        recorded_at: None,
    }
}

/// Bounded, non-following walk. Partial totals are lower bounds.
fn walk(name: String, root: PathBuf, deadline: Instant) -> RootUsage {
    let mut usage = unwalked(name, root.clone());
    usage.complete = true;
    let mut pending = vec![root.clone()];
    while let Some(path) = pending.pop() {
        if Instant::now() >= deadline {
            usage.complete = false;
            break;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                if path != root {
                    usage.entries += 1;
                }
                usage.bytes += allocated_bytes(&meta);
                if meta.is_dir() {
                    match std::fs::read_dir(&path) {
                        Ok(entries) => {
                            for entry in entries {
                                if pending.len() >= MAX_PENDING || Instant::now() >= deadline {
                                    usage.complete = false;
                                    break;
                                }
                                match entry {
                                    Ok(entry) => pending.push(entry.path()),
                                    Err(_) => usage.errors += 1,
                                }
                            }
                        }
                        Err(_) => usage.errors += 1,
                    }
                }
            }
            // An absent root is measured as empty.
            Err(error) if path == root && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => usage.errors += 1,
        }
        if !usage.complete {
            break;
        }
    }
    usage.complete &= usage.errors == 0;
    usage.measurement = if usage.complete {
        Measurement::Measured
    } else {
        Measurement::Estimated
    };
    usage
}

fn allocated_bytes(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

fn volume_usage(data: &Path) -> Option<VolumeUsage> {
    // The data root may not exist yet; measure its nearest existing ancestor.
    let probe = data.ancestors().find(|path| path.exists())?;
    let budget = crate::observation::disk_budget::disk_budget(
        probe,
        "homeboy data",
        "disk capacity is not measurable on this platform",
    );
    (budget.total_bytes.is_some() || budget.available_bytes.is_some()).then(|| VolumeUsage {
        path: data.display().to_string(),
        total_bytes: budget.total_bytes,
        used_bytes: budget
            .total_bytes
            .zip(budget.available_bytes)
            .map(|(total, free)| total.saturating_sub(free)),
        free_bytes: budget.available_bytes,
    })
}

fn memory_usage() -> Option<MemoryUsage> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kib| kib * 1024)
    };
    Some(MemoryUsage {
        total_bytes: field("MemTotal")?,
        available_bytes: field("MemAvailable"),
        swap_used_bytes: field("SwapTotal")
            .zip(field("SwapFree"))
            .map(|(total, free)| total.saturating_sub(free)),
    })
}

#[cfg(unix)]
fn load_usage() -> Option<LoadUsage> {
    let mut values = [0.0_f64; 1];
    if unsafe { libc::getloadavg(values.as_mut_ptr(), 1) } != 1 {
        return None;
    }
    let logical_cpus = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    Some(LoadUsage {
        load_1m: values[0],
        logical_cpus,
        load_per_cpu: values[0] / logical_cpus as f64,
    })
}

#[cfg(not(unix))]
fn load_usage() -> Option<LoadUsage> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn walk_does_not_follow_links_and_records_totals() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let record = std::sync::Mutex::new(SizeRecord::default());
        std::fs::write(outside.path().join("large"), vec![0; 8192]).unwrap();
        std::fs::create_dir(root.path().join("tree")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("tree/link")).unwrap();
        let roots = vec![("tree".to_string(), root.path().join("tree"))];
        let first = root_sizes(roots.clone(), Duration::from_secs(1), &record);
        assert_eq!(first[0].measurement, Measurement::Measured);
        assert_eq!(first[0].entries, 1);
        assert!(first[0].bytes < 8192);
        std::fs::write(root.path().join("tree/new"), vec![0; 8192]).unwrap();
        let recorded = root_sizes(roots, Duration::ZERO, &record);
        assert_eq!(recorded[0].source, SizeSource::Recorded);
        assert_eq!(recorded[0].measurement, Measurement::Estimated);
        assert_eq!(first[0].bytes, recorded[0].bytes);
    }

    #[test]
    fn walk_respects_its_deadline() {
        let started = Instant::now();
        let usage = walk("root".to_string(), PathBuf::from("/"), Instant::now());
        assert!(!usage.complete);
        assert_eq!(usage.measurement, Measurement::Estimated);
        assert_eq!(usage.bytes, 0);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
