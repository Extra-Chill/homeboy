//! Tests split from `agent_task_controller_service` god file (#5208), further
//! split out of the oversized `tests.rs` into concern-grouped submodules.
//!
//! Shared fixtures, dispatch-hook/executor adapters, and helper builders live in
//! [`common`]; each `*_tests` module groups the test functions for one concern.

mod common;

mod compile_plan_tests;
mod dispatch_tests;
mod failure_summary_tests;
mod gate_tests;
mod resume_tests;
mod run_tests;
mod spec_compile_tests;
mod spec_tests;
