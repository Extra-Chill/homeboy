//! Stack subsystem for homeboy: combined-fixes branches built from a base
//! branch plus cherry-picked PRs (stack specs, sync/apply/rebase, PR metadata,
//! status/inspect reporting, push).
//!
//! Depends on homeboy-core; core does not depend on it.

pub mod provider;
pub mod stack;

pub use stack::*;
