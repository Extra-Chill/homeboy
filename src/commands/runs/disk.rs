//! Thin re-export of the core disk-budget probe.
//!
//! The probing logic moved to `core::observation::disk_budget` so the
//! observation evidence report service can compute disk budgets without
//! depending on a CLI command module. Command submodules keep referencing
//! `disk::DiskBudget` / `disk::disk_budget` through this re-export.

pub use homeboy::core::observation::disk_budget::{disk_budget, DiskBudget};
