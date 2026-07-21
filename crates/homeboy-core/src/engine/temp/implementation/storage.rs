use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct StorageMeasure {
    pub(super) logical_bytes: u64,
    pub(super) allocated_bytes: u64,
}

pub(super) fn path_storage_measure(path: &Path) -> Result<StorageMeasure> {
    path_storage_measure_inner(path, &mut HashSet::new())
}

fn path_storage_measure_inner(
    path: &Path,
    seen: &mut HashSet<(u64, u64)>,
) -> Result<StorageMeasure> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;
    if file_identity(&metadata).is_some_and(|identity| !seen.insert(identity)) {
        return Ok(StorageMeasure {
            logical_bytes: 0,
            allocated_bytes: 0,
        });
    }
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(StorageMeasure {
            logical_bytes: metadata.len(),
            allocated_bytes: allocated_bytes(&metadata),
        });
    }
    let mut total = StorageMeasure {
        logical_bytes: 0,
        allocated_bytes: allocated_bytes(&metadata),
    };
    for entry in fs::read_dir(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read entry {}", path.display())),
            )
        })?;
        let measure = path_storage_measure_inner(&entry.path(), seen)?;
        total.logical_bytes = total.logical_bytes.saturating_add(measure.logical_bytes);
        total.allocated_bytes = total
            .allocated_bytes
            .saturating_add(measure.allocated_bytes);
    }
    Ok(total)
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    if metadata.is_dir() || metadata.nlink() == 1 {
        metadata.blocks().saturating_mul(512)
    } else {
        0
    }
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

pub(super) fn filesystem_available_bytes(path: &Path) -> Option<u64> {
    fs4::available_space(path.parent().unwrap_or(path)).ok()
}

pub(super) fn verified_reclaimed_bytes(
    before: Option<u64>,
    after: Option<u64>,
    allocated: u64,
) -> u64 {
    match (before, after) {
        (Some(before), Some(after)) => after.saturating_sub(before).min(allocated),
        _ => 0,
    }
}
