//! Release + deploy subsystem for homeboy.
//!
//! `deploy` (build + ship components to targets) and `release` (versioning,
//! changelog, tagging, GitHub releases) are mutually dependent, so they live
//! together in one crate. Both depend on `homeboy-core`; core does not depend
//! on them — core's status mechanics reach release/deploy behavior through the
//! `homeboy_core::release_provider` hook, implemented here in `provider_impl`.

pub mod deploy;
pub mod release;

pub use release::provider_impl;
