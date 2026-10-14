//! The generic `HOMEBOY_WORKLOAD_*` runner environment contract.
//!
//! Every workload kind's runner reads the same names. Kind-specific inputs
//! travel as extension settings (`HOMEBOY_SETTINGS_JSON`) or script args.

/// The kind being executed.
pub const KIND: &str = "HOMEBOY_WORKLOAD_KIND";
/// Where the runner writes its `homeboy/workload-result/v1` envelope (or, in
/// list-only mode, its `{"workloads": [...]}` listing).
pub const RESULTS_FILE: &str = "HOMEBOY_WORKLOAD_RESULTS_FILE";
/// Directory the runner writes artifacts into; envelope artifact paths are
/// relative to it.
pub const ARTIFACT_DIR: &str = "HOMEBOY_WORKLOAD_ARTIFACT_DIR";
/// `1` when Homeboy only wants the runner's workload listing.
pub const LIST_ONLY: &str = "HOMEBOY_WORKLOAD_LIST_ONLY";
/// Selected workload id, when one was selected.
pub const ID: &str = "HOMEBOY_WORKLOAD_ID";
/// Homeboy run id of this execution (run) or of the source run (replay).
pub const RUN_ID: &str = "HOMEBOY_WORKLOAD_RUN_ID";
/// Deterministic seed requested for a run, or the seed of a replayed case.
pub const SEED: &str = "HOMEBOY_WORKLOAD_SEED";
/// Case being replayed or minimized.
pub const CASE_ID: &str = "HOMEBOY_WORKLOAD_CASE_ID";
/// The case's `artifact_ref` exactly as the envelope declared it.
pub const CASE_ARTIFACT_REF: &str = "HOMEBOY_WORKLOAD_CASE_ARTIFACT_REF";
/// Local file the case's `artifact_ref` resolved to, when it resolved.
pub const CASE_ARTIFACT_FILE: &str = "HOMEBOY_WORKLOAD_CASE_ARTIFACT_FILE";
/// The persisted envelope of the source run, for replay and minimize.
pub const SOURCE_RESULTS_FILE: &str = "HOMEBOY_WORKLOAD_SOURCE_RESULTS_FILE";
/// `replay` or `minimize`.
pub const CASE_MODE: &str = "HOMEBOY_WORKLOAD_CASE_MODE";
/// `1` only when a destructive kind was explicitly authorized.
pub const ALLOW_DESTRUCTIVE: &str = "HOMEBOY_WORKLOAD_ALLOW_DESTRUCTIVE";
/// The verified `homeboy/isolation-proof/v1` a destructive run was
/// authorized with.
pub const ISOLATION_PROOF_FILE: &str = "HOMEBOY_WORKLOAD_ISOLATION_PROOF_FILE";
