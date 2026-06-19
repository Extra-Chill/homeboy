//! Tests for the `agent-task` command tree, split by behavioral concern so each
//! file stays focused and under the structural item-count threshold.
//!
//! Shared imports, executors, and fixtures live in [`support`]; each concern
//! submodule does `use super::support::*`.

mod support;

mod contract;
mod controller_run;
mod dispatch;
mod lifecycle;
mod loop_compile;
mod promotion_review;
