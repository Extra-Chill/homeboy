//! Upgrade subsystem for homeboy: self-upgrade orchestration, install-method
//! detection, update checks, runner/extension upgrade coordination, and the
//! `self status` reporting surface.
//!
//! Depends on homeboy-core; core does not depend on it.

pub mod self_status;
pub mod upgrade;

// Re-export the upgrade module surface at the crate root so external consumers
// that previously used `homeboy_core::upgrade::X` can migrate to
// `homeboy_upgrade::upgrade::X` (or the flattened `homeboy_upgrade::X`).
pub use upgrade::*;
