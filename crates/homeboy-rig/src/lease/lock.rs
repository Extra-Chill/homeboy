//! Advisory lock guarding read-modify-write of the rig lease index.
//!
//! The mkdir-lock mechanics live in
//! [`homeboy_engine_primitives::fs_index_lock`]; this module only supplies the
//! directory and the error-message subject.

use homeboy_core::error::Result;
use homeboy_core::paths;
use homeboy_engine_primitives::fs_index_lock::{FsIndexLock, FsIndexLockConfig};

const CONFIG: FsIndexLockConfig = FsIndexLockConfig::index("rig lease");

pub(super) type LeaseIndexLock = FsIndexLock;

/// Block until the rig lease index lock is held. Released when the returned
/// guard drops.
pub(super) fn acquire() -> Result<LeaseIndexLock> {
    FsIndexLock::acquire_in(&paths::rig_leases_dir()?, CONFIG)
}
