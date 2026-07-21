mod implementation;

pub(crate) use implementation::{
    bind_run_dir_owner, managed_run_temp_dir, mark_run_dir_succeeded, pin_runtime_temp_dir,
    retain_failed_run_dir, RuntimeTempPin,
};
pub use implementation::{
    cleanup_runtime_tmp, cleanup_runtime_tmp_bounded, runtime_temp_dir, unique_name,
    CleanupSizeTotals, RuntimeTempCleanupOptions, RuntimeTempCleanupOutput, RuntimeTempCleanupRow,
};

// Keep implementation references scoped to the engine-owned sibling modules.
#[cfg(test)]
pub(super) use super::{invocation, run_dir};
