//! Pure serializable data types for the homeboy extension system.
//!
//! This crate holds behavior-free contract types shared between core, the
//! extension execution subsystem, and downstream consumers. It has no
//! dependency on `homeboy-core`, which keeps it a leaf that other crates can
//! depend on without pulling in the whole core compile unit.
//!
//! Types are re-exported from `homeboy_core::extension` so existing
//! `crate::extension::*` call sites keep working unchanged.

mod manifest_deploy_config;
mod manifest_test_config;

pub use manifest_deploy_config::{DeployArchiveInstallPolicy, DeployRequiredHeader};
pub use manifest_test_config::{TestPassthroughFilter, TestPassthroughFilterStrategy};
