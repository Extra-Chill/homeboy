//! Bounded, non-following inventory. Partial byte totals are lower bounds.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootUsage {
    pub root: PathBuf,
    pub bytes: u64,
    pub complete: bool,
    pub errors: u64,
}

/// Coalesce concurrent requests and reuse recent totals rather than repeatedly
/// scanning while a runner is busy. The lock is never waited on by callers.
pub fn cached_scan(roots: Vec<PathBuf>, budget: Duration) -> Vec<RootUsage> {
    type Cached = Option<(Instant, Vec<PathBuf>, Vec<RootUsage>)>;
    static CACHE: std::sync::Mutex<Cached> = std::sync::Mutex::new(None);
    let Ok(mut cache) = CACHE.try_lock() else {
        return roots
            .into_iter()
            .map(|root| RootUsage {
                root,
                bytes: 0,
                complete: false,
                errors: 0,
            })
            .collect();
    };
    if let Some((at, previous_roots, report)) = cache.as_ref() {
        if previous_roots == &roots && at.elapsed() < Duration::from_secs(60) {
            return report.clone();
        }
    }
    let report = scan(roots.clone(), budget);
    *cache = Some((Instant::now(), roots, report.clone()));
    report
}

pub fn scan(roots: Vec<PathBuf>, budget: Duration) -> Vec<RootUsage> {
    let deadline = Instant::now() + budget;
    roots
        .into_iter()
        .map(|root| {
            let mut usage = RootUsage {
                root: root.clone(),
                bytes: 0,
                complete: true,
                errors: 0,
            };
            let mut pending = vec![root];
            while let Some(path) = pending.pop() {
                if Instant::now() >= deadline {
                    usage.complete = false;
                    break;
                }
                match std::fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_dir() => match std::fs::read_dir(path) {
                        Ok(entries) => {
                            for entry in entries {
                                if Instant::now() >= deadline {
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
                    },
                    Ok(meta) => {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            usage.bytes += meta.blocks() * 512;
                        }
                        #[cfg(not(unix))]
                        {
                            usage.bytes += meta.len();
                        }
                    }
                    Err(_) => usage.errors += 1,
                }
                if !usage.complete {
                    break;
                }
            }
            usage.complete &= usage.errors == 0;
            usage
        })
        .collect()
}

pub fn owned_roots(workspace_root: Option<PathBuf>) -> crate::error::Result<Vec<PathBuf>> {
    let data = crate::paths::homeboy_data()?;
    let mut roots = vec![
        crate::paths::artifact_root()?,
        crate::cleanup::shared_cargo_target_root()?,
        data.join("runtime/tmp"), // isolated gate homes and invocation scratch
        data.join("controller-state/gate-rust-cache"), // managed cargo/rustup packages
        crate::paths::home_root()?.join(".cache/homeboy/playwright-browsers"),
    ];
    if let Some(root) = workspace_root {
        roots.insert(0, root.join("_lab_workspaces"));
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_respects_time_budget() {
        let started = Instant::now();
        let report = scan(vec![PathBuf::from("/")], Duration::ZERO);
        assert!(!report[0].complete);
        assert_eq!(report[0].bytes, 0);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
