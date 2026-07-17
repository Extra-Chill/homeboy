//! Pure serializable data types for the homeboy extension system.
//!
//! This crate holds behavior-free contract types shared between core, the
//! extension execution subsystem, and downstream consumers. It depends only on
//! leaf crates (`homeboy-error`, `homeboy-audit-contract`), which keeps it a
//! lightweight crate that others can depend on without pulling in the whole
//! core compile unit.
//!
//! Modules and types are re-exported from `homeboy_core::extension` so existing
//! `crate::extension::*` call sites keep working unchanged.

pub mod action_types;
pub mod core_compat;
pub mod exec_context;
pub mod extension_contract_producer;
pub mod manifest_action_config;
pub mod manifest_capability_config;
pub mod manifest_deploy_config;
pub mod manifest_test_config;
pub mod manifest_toolchain_config;
pub mod runner_contract;
pub mod source_metadata_repair;
pub mod test_drift;
pub mod update_output;
pub mod version;

pub use core_compat::{
    core_incompatible_error, evaluate_core_compatibility, installed_homeboy_version,
    validate_core_compatibility, CoreCompatibilityReport, CORE_COMPAT_REMEDIATION_COMMAND,
    CORE_INCOMPATIBLE_DIAGNOSTIC,
};
pub use manifest_deploy_config::{DeployArchiveInstallPolicy, DeployRequiredHeader};
pub use manifest_test_config::{TestPassthroughFilter, TestPassthroughFilterStrategy};
pub use runner_contract::{
    phase_failure_category_from_exit_code, phase_status_from_exit_code, ExtensionPhaseTiming,
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, RunnerStepFilter,
    VerificationPhase, GENERIC_INFRASTRUCTURE_FAILURE_MARKERS,
};
pub use test_drift::TestDriftConfig;
pub use version::{parse_extension_version, VersionConstraint};
