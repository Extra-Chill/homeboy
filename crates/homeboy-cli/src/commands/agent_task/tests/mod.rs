//! Tests for the `agent-task` command tree, split by behavioral concern so each
//! file stays focused and under the structural item-count threshold.
//!
//! Shared imports, executors, and fixtures live in [`support`]; each concern
//! submodule does `use super::support::*`.

pub(super) mod support;

mod candidate_ref;
mod contract;
mod dispatch;
mod lifecycle;
mod promotion_review;
mod status_purity;
