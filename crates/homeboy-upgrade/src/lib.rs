//! Upgrade subsystem for homeboy: self-upgrade orchestration, install-method
//! detection, update checks, runner/extension upgrade coordination, and the
//! `self status` reporting surface.
//!
//! Depends on homeboy-core; core does not depend on it.

pub mod controller_staleness;
pub mod self_status;
pub mod upgrade;
