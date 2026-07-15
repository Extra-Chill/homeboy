//! Lab-runnable command labels.
//!
//! These identify the command families that can be dispatched to a Lab runner.
//! `RunnerWorkload` classification (see `workload.rs`) matches on them, so they
//! live with the lab contract. `command_contract::spec` re-exports them to keep
//! its existing call sites stable.

pub const LINT_LAB_LABEL: &str = "lint";
pub const TEST_LAB_LABEL: &str = "test";
pub const AUDIT_LAB_LABEL: &str = "audit";
pub const REVIEW_LAB_LABEL: &str = "review";
pub const BENCH_LAB_LABEL: &str = "bench";
pub const FUZZ_LAB_LABEL: &str = "fuzz";
pub const FUZZ_DOCTOR_LAB_LABEL: &str = "fuzz doctor";
pub const TRACE_LAB_LABEL: &str = "trace";
pub const REFACTOR_LAB_LABEL: &str = "refactor";
pub const RIG_CHECK_LAB_LABEL: &str = "rig check";
pub const RIG_RUN_LAB_LABEL: &str = "rig run";
pub const RUNTIME_REFRESH_LAB_LABEL: &str = "runtime refresh";
