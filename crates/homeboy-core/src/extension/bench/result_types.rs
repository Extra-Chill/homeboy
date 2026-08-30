//! Re-exports the bench result types from the contract crate.
//!
//! The full bench result type graph lives in `homeboy-extension-contract`;
//! this module re-exports it so `super::result_types::*` paths stay stable.

pub use homeboy_extension_contract::bench_result::{
    BenchChildCommandFailure, BenchMemory, BenchMetricDirection, BenchMetricPhase,
    BenchMetricPolicy, BenchMetrics, BenchProvenance, BenchProvenanceLink, BenchRunExecution,
    BenchRunnerMetadata, BenchWorkloadMetadata, RegressionTest, RigPackageEvidence,
    RigPackageFreshness,
};
pub use homeboy_extension_contract::bench_results::{
    BenchResults, BenchRunMetadata, BenchRunSnapshot, BenchScenario,
};
