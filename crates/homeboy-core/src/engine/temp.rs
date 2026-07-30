mod implementation;

pub(crate) use implementation::{
    bind_run_dir_owner, managed_run_temp_dir, managed_run_temp_dir_for_producer,
    mark_run_dir_succeeded, pin_runtime_temp_dir, retain_failed_run_dir, RuntimeTempPin,
};
pub use implementation::{
    cleanup_runtime_tmp, cleanup_runtime_tmp_bounded, present_runtime_temp_cleanup,
    runtime_temp_dir, unique_name, CleanupSizeTotals, RuntimeTempCleanupOptions,
    RuntimeTempCleanupOutput, RuntimeTempCleanupRow, RuntimeTempOwner,
};

// Keep implementation references scoped to the engine-owned sibling modules.
#[cfg(test)]
pub(super) use super::{invocation, run_dir};
