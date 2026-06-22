//! Runner upgrade orchestration, split into focused submodules.
//!
//! This module coordinates upgrading configured runners: driving the upgrade
//! executor chain, recovering from failed upgrades, realigning drifted
//! `homeboy_path` configuration, syncing runner extensions, probing runner
//! versions, building remediation commands, and rendering upgrade reports.
//!
//! The submodules are a mechanical split of the former single-file module; all
//! items are re-exported here so existing `crate::core::upgrade::runners::*`
//! paths continue to resolve unchanged.

mod commands;
mod extensions;
mod failure;
mod orchestration;
mod path_alignment;
mod reporting;
mod source_checkout;
mod version;

pub(super) use commands::*;
pub(super) use extensions::*;
pub(super) use failure::*;
pub(super) use orchestration::*;
pub(super) use path_alignment::*;
pub(super) use reporting::*;
pub(super) use source_checkout::*;
pub(super) use version::*;

#[cfg(test)]
mod tests;
