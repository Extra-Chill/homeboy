//! Agent and operator scratch: verification, replay and recovery bytes that
//! are not part of any Cook (#17240).
//!
//! A scratch allocation is a runtime-temp directory with producer
//! [`SCRATCH_PRODUCER`] and a [`ScratchLease`] in its owner record. It lives
//! under the same root, and goes through the same cleanup, automatic retention
//! and `retained-storage` inventory, as every other runtime-temp owner. The
//! lease protects it until its TTL passes or while it is pinned. After that,
//! runtime-temp cleanup removes it.

use super::*;

pub const SCRATCH_PRODUCER: &str = "scratch";
const SCRATCH_PREFIX: &str = "scratch";

/// Who owns a scratch allocation, why it exists, and how long it is kept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScratchLease {
    /// Issue, run or session the bytes belong to.
    pub owner: String,
    pub purpose: String,
    pub expires_at: String,
    /// Set while a tracker or run still references the bytes; keeps them
    /// past `expires_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

impl ScratchLease {
    /// Why cleanup must keep this allocation now, if it must.
    pub(super) fn protection(&self) -> Option<String> {
        if let Some(reason) = &self.pinned {
            return Some(format!(
                "scratch for {} is pinned: {reason}; release with `homeboy scratch unpin`",
                self.owner
            ));
        }
        let unexpired = self
            .expires_at
            .parse::<chrono::DateTime<chrono::Utc>>()
            .is_ok_and(|expires| expires > chrono::Utc::now());
        unexpired.then(|| {
            format!(
                "scratch for {} ({}) is kept until {}",
                self.owner, self.purpose, self.expires_at
            )
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScratchAllocation {
    pub id: String,
    pub path: String,
    pub created_at: String,
    #[serde(flatten)]
    pub lease: ScratchLease,
    /// Whether cleanup may remove it now.
    pub reclaimable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Allocate a scratch directory. The owner record carries the lease before
/// the path is returned, so no caller ever holds unowned scratch.
pub fn allocate_scratch(owner: &str, purpose: &str, ttl: Duration) -> Result<ScratchAllocation> {
    for (field, value) in [("owner", owner), ("purpose", purpose)] {
        if value.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                field,
                format!("scratch {field} must not be empty"),
                None,
                None,
            ));
        }
    }
    let ttl = chrono::Duration::from_std(ttl).map_err(|_| {
        Error::validation_invalid_argument("ttl", "scratch TTL is too large", None, None)
    })?;
    let (path, pin) = managed_run_temp_dir_for_producer(SCRATCH_PREFIX, Some(SCRATCH_PRODUCER))?;
    let lease = ScratchLease {
        owner: owner.trim().to_string(),
        purpose: purpose.trim().to_string(),
        expires_at: (chrono::Utc::now() + ttl).to_rfc3339(),
        pinned: None,
    };
    // The allocating pin protects the directory until the lease is durable.
    let leased = update_lease(&path, |owner| {
        owner.state = SCRATCH_PRODUCER.to_string();
        owner.scratch = Some(lease);
        Ok(())
    });
    drop(pin);
    match leased {
        Ok(owner) => Ok(allocation(&path, &owner, false)),
        Err(error) => {
            let _ = fs::remove_dir_all(&path);
            Err(error)
        }
    }
}

/// Every scratch allocation on this host, oldest first.
pub fn list_scratch(measure: bool) -> Result<Vec<ScratchAllocation>> {
    let root = runtime_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(format!("read runtime tmp {}", root.display())),
            ))
        }
    };
    let mut allocations = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let path = entry.path();
            let owner = read_run_owner(&path).ok()?;
            owner
                .scratch
                .is_some()
                .then(|| allocation(&path, &owner, measure))
        })
        .collect::<Vec<_>>();
    allocations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(allocations)
}

/// Keep an allocation past its TTL until [`unpin_scratch`].
pub fn pin_scratch(id: &str, reason: &str) -> Result<ScratchAllocation> {
    if reason.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "reason",
            "a scratch pin needs a reason, such as the tracker that references it",
            None,
            None,
        ));
    }
    let path = scratch_path(id)?;
    let owner = update_lease(&path, |owner| {
        lease_mut(owner)?.pinned = Some(reason.trim().to_string());
        Ok(())
    })?;
    Ok(allocation(&path, &owner, false))
}

/// Return a pinned allocation to its TTL.
pub fn unpin_scratch(id: &str) -> Result<ScratchAllocation> {
    let path = scratch_path(id)?;
    let owner = update_lease(&path, |owner| {
        lease_mut(owner)?.pinned = None;
        Ok(())
    })?;
    Ok(allocation(&path, &owner, false))
}

/// Delete an allocation now, pinned or not.
pub fn release_scratch(id: &str) -> Result<ScratchAllocation> {
    let path = scratch_path(id)?;
    let owner = read_run_owner(&path)?;
    let mut released = allocation(&path, &owner, true);
    fs::remove_dir_all(&path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("release scratch {}", path.display())),
        )
    })?;
    released.reclaimable = true;
    Ok(released)
}

fn allocation(path: &Path, owner: &RuntimeRunOwner, measure: bool) -> ScratchAllocation {
    let lease = owner
        .scratch
        .clone()
        .expect("scratch allocations carry a lease");
    ScratchAllocation {
        id: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: path.display().to_string(),
        created_at: owner.created_at.clone(),
        reclaimable: lease.protection().is_none(),
        lease,
        size_bytes: measure
            .then(|| path_storage_measure(path).ok())
            .flatten()
            .map(|storage| storage.allocated_bytes),
    }
}

fn lease_mut(owner: &mut RuntimeRunOwner) -> Result<&mut ScratchLease> {
    owner.scratch.as_mut().ok_or_else(|| {
        Error::validation_invalid_argument(
            "id",
            "runtime temp entry is not a scratch allocation",
            None,
            None,
        )
    })
}

/// Rewrite the owner record under its lock, so a concurrent cleanup either
/// sees the old lease or the new one.
fn update_lease(
    path: &Path,
    change: impl FnOnce(&mut RuntimeRunOwner) -> Result<()>,
) -> Result<RuntimeRunOwner> {
    let _lock = FsIndexLock::acquire_in(path, RUN_OWNER_LOCK).map_err(|error| {
        Error::internal_unexpected(format!(
            "lock scratch owner {}: {}",
            path.display(),
            error.message
        ))
    })?;
    let mut owner = read_run_owner(path)?;
    change(&mut owner)?;
    write_run_owner(path, &owner)?;
    Ok(owner)
}

/// Resolve an allocation id (its directory name) or path to a scratch
/// directory directly under the runtime-temp root.
fn scratch_path(id: &str) -> Result<PathBuf> {
    let root = runtime_root()?;
    let name = Path::new(id.trim())
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let path = root.join(&name);
    let is_allocation = !name.is_empty()
        && Path::new(id.trim())
            .parent()
            .is_none_or(|parent| parent.as_os_str().is_empty() || parent == root)
        && fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir())
        && read_run_owner(&path).is_ok_and(|owner| owner.scratch.is_some());
    if !is_allocation {
        return Err(Error::validation_invalid_argument(
            "id",
            "no scratch allocation with this id on this host; see `homeboy scratch list`",
            Some(id.to_string()),
            None,
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    fn expire(id: &str) {
        let path = scratch_path(id).expect("allocation");
        update_lease(&path, |owner| {
            lease_mut(owner)?.expires_at = "2020-01-01T00:00:00+00:00".to_string();
            Ok(())
        })
        .expect("expire lease");
    }

    fn retention_pass() -> RuntimeTempCleanupOutput {
        cleanup_runtime_tmp(true, 365, None, 100).expect("runtime tmp cleanup")
    }

    #[test]
    fn scratch_is_kept_until_its_ttl_and_while_pinned() {
        with_isolated_home(|_| {
            let live = allocate_scratch("#17240", "fresh", Duration::from_secs(3600)).unwrap();
            let expired = allocate_scratch("#17240", "expired", Duration::from_secs(3600)).unwrap();
            let pinned = allocate_scratch("#17240", "pinned", Duration::from_secs(3600)).unwrap();
            for allocation in [&live, &expired, &pinned] {
                fs::write(Path::new(&allocation.path).join("proof.bin"), b"bytes").unwrap();
            }
            expire(&expired.id);
            expire(&pinned.id);
            pin_scratch(&pinned.id, "referenced by #17240").unwrap();

            let listed = list_scratch(true).unwrap();
            assert_eq!(listed.len(), 3);
            let expired_row = listed.iter().find(|row| row.id == expired.id).unwrap();
            assert!(expired_row.reclaimable);
            assert!(expired_row.size_bytes.is_some());

            let output = retention_pass();
            let removed = output
                .rows
                .iter()
                .filter(|row| row.action == "removed")
                .map(|row| row.name.clone())
                .collect::<Vec<_>>();
            assert_eq!(removed, vec![expired.id.clone()], "{:#?}", output.rows);
            assert!(Path::new(&live.path).exists());
            assert!(Path::new(&pinned.path).exists());

            unpin_scratch(&pinned.id).unwrap();
            retention_pass();
            assert!(!Path::new(&pinned.path).exists());
            assert!(Path::new(&live.path).exists());

            release_scratch(&live.id).unwrap();
            assert!(!Path::new(&live.path).exists());
            assert!(list_scratch(false).unwrap().is_empty());
        });
    }

    #[test]
    fn scratch_verbs_refuse_paths_that_are_not_allocations() {
        with_isolated_home(|_| {
            let allocation = allocate_scratch("#17240", "proof", Duration::from_secs(60)).unwrap();
            let (other, _pin) = managed_run_temp_dir_for_producer("other", Some("gate")).unwrap();
            for id in [
                "",
                "missing",
                other.to_str().unwrap(),
                "/elsewhere/scratch-x",
                "../scratch",
            ] {
                assert!(release_scratch(id).is_err(), "{id}");
            }
            assert!(other.exists());
            // A path to the allocation resolves the same as its id.
            assert_eq!(
                pin_scratch(&allocation.path, "proof").unwrap().id,
                allocation.id
            );
            assert!(allocate_scratch(" ", "proof", Duration::from_secs(60)).is_err());
            assert!(pin_scratch(&allocation.id, "").is_err());
        });
    }
}
