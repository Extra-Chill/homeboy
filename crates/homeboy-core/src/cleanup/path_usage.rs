//! Shared non-following filesystem accounting for cleanup and observation.
//!
//! Logical bytes count file and link content, not directory entry lengths.
//! Allocated bytes include directory blocks. I/O failures are errors unless
//! explicitly ignored, in which case `errors` makes partial totals observable.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use crate::{Error, Result};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PathUsage {
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub newest_modified: Option<SystemTime>,
}

impl PathUsage {
    pub(crate) fn age_seconds(&self) -> Option<u64> {
        SystemTime::now()
            .duration_since(self.newest_modified?)
            .ok()
            .map(|elapsed| elapsed.as_secs())
    }
}

/// Symlinks are never followed; optionally count the link itself.
#[derive(Default)]
pub struct PathUsageOptions<'a> {
    pub skip_symlinks: bool,
    pub dedupe_hard_links: bool,
    pub visit_budget: Option<usize>,
    pub deadline: Option<Instant>,
    pub entry_filter: Option<&'a dyn Fn(&Path) -> bool>,
    pub ignore_errors: bool,
    pub allow_first_step: bool,
    pub max_pending: Option<usize>,
    pub max_depth: Option<usize>,
}

#[derive(Default)]
pub struct PathUsageWalk {
    pub usage: PathUsage,
    pub visited: usize,
    pub errors: usize,
    /// Unvisited paths, suitable for resuming an interrupted measurement.
    pub pending: Vec<PathBuf>,
}

pub fn measure_path(path: &Path, options: PathUsageOptions<'_>) -> Result<PathUsageWalk> {
    walk_paths(vec![path.to_path_buf()], options, &mut Instant::now)
}

pub(super) fn walk_paths(
    mut pending: Vec<PathBuf>,
    options: PathUsageOptions<'_>,
    now: &mut impl FnMut() -> Instant,
) -> Result<PathUsageWalk> {
    let mut result = PathUsageWalk::default();
    let base_depth = pending
        .iter()
        .map(|path| path.components().count())
        .min()
        .unwrap_or(0);
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    while let Some(path) = pending.pop() {
        if options
            .visit_budget
            .is_some_and(|limit| result.visited >= limit)
            || ((!options.allow_first_step || result.visited > 0)
                && options.deadline.is_some_and(|deadline| now() >= deadline))
        {
            pending.push(path);
            break;
        }
        if options.entry_filter.is_some_and(|filter| !filter(&path)) {
            continue;
        }
        result.visited = result.visited.saturating_add(1);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) if options.ignore_errors => {
                result.errors += 1;
                continue;
            }
            Err(error) => return Err(io_error(&path, error)),
        };
        if options.skip_symlinks && metadata.file_type().is_symlink() {
            continue;
        }
        #[cfg(unix)]
        if options.dedupe_hard_links {
            use std::os::unix::fs::MetadataExt;
            if !seen.insert((metadata.dev(), metadata.ino())) {
                continue;
            }
        }
        #[cfg(not(unix))]
        let _ = &mut seen as &mut HashSet<(u64, u64)>;
        let usage = &mut result.usage;
        if let Ok(modified) = metadata.modified() {
            usage.newest_modified = Some(
                usage
                    .newest_modified
                    .map_or(modified, |old| old.max(modified)),
            );
        }
        let logical = if metadata.is_dir() { 0 } else { metadata.len() };
        usage.logical_bytes = usage.logical_bytes.saturating_add(logical);
        usage.allocated_bytes = usage
            .allocated_bytes
            .saturating_add(allocated_bytes(&metadata, logical));
        if !metadata.is_dir() {
            continue;
        }
        if options
            .max_depth
            .is_some_and(|limit| path.components().count().saturating_sub(base_depth) >= limit)
        {
            return Err(Error::internal_unexpected(
                "filesystem measurement depth limit reached",
            ));
        }
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(_) if options.ignore_errors => {
                result.errors += 1;
                continue;
            }
            Err(error) => return Err(io_error(&path, error)),
        };
        for entry in entries {
            match entry {
                Ok(entry) => pending.push(entry.path()),
                Err(_) if options.ignore_errors => result.errors += 1,
                Err(error) => return Err(io_error(&path, error)),
            }
            if options
                .max_pending
                .is_some_and(|limit| pending.len() >= limit)
            {
                result.errors += 1;
                break;
            }
        }
    }
    result.pending = pending;
    Ok(result)
}

fn io_error(path: &Path, error: std::io::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some(format!("measure {}", path.display())),
    )
}

fn allocated_bytes(metadata: &fs::Metadata, fallback: u64) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = fallback;
        metadata.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_preserve_partial_totals_and_filters_skip_subtrees() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("payload"), b"abc").unwrap();
        let skipped = root.path().join("sidecar");
        fs::create_dir(&skipped).unwrap();
        fs::write(skipped.join("ignored"), b"ignored").unwrap();
        let include = |path: &Path| path != skipped;
        let measured = measure_path(
            root.path(),
            PathUsageOptions {
                entry_filter: Some(&include),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(measured.usage.logical_bytes, 3);
        let bounded = measure_path(
            root.path(),
            PathUsageOptions {
                visit_budget: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(bounded.visited, 1);
        assert_eq!(bounded.pending.len(), 2);
        let expired = measure_path(
            root.path(),
            PathUsageOptions {
                deadline: Some(Instant::now()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(expired.visited, 0);
        assert_eq!(expired.pending.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_optionally_deduplicated() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        fs::write(&original, b"abc").unwrap();
        fs::hard_link(original, root.path().join("linked")).unwrap();
        let measured = measure_path(
            root.path(),
            PathUsageOptions {
                dedupe_hard_links: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(measured.usage.logical_bytes, 3);
        let measured = measure_path(root.path(), Default::default()).unwrap();
        assert_eq!(measured.usage.logical_bytes, 6);
    }

    #[cfg(unix)]
    #[test]
    fn outside_symlink_is_never_followed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(root.path().join("inside"), b"abc").unwrap();
        fs::write(outside.path().join("outside"), vec![0; 8192]).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let measured = measure_path(root.path(), PathUsageOptions::default()).unwrap();
        assert_eq!(
            measured.usage.logical_bytes,
            3 + fs::symlink_metadata(link).unwrap().len()
        );
        let measured = measure_path(
            root.path(),
            PathUsageOptions {
                skip_symlinks: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(measured.usage.logical_bytes, 3);
    }
}
