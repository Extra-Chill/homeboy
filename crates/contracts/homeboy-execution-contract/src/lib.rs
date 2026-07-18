//! Typed runtime-facing execution surface contract.
//!
//! `ExecutionContract` and its component contracts (`ArtifactUriContract`,
//! `LabOffloadExecutionContract`, `ApplyChangeContract`) describe the concrete
//! runtime values that workflow steps exchange across process boundaries —
//! artifact URI schemes, apply/change wire schemas, and lab-offload metadata.
//! Plus the pure classification/token helpers built on those schemes.
//!
//! These depend only on the artifact-ref contract, the engine-primitives URI
//! codecs, and base64 — no core engine — so this is a leaf crate consumers can
//! depend on directly. The one path-touching classifier
//! (`is_reportable_artifact_evidence_path`, which does `std::fs::metadata`)
//! stays in `homeboy-core` as a free function.

pub mod execution_contract;

pub use execution_contract::{
    artifact_store_locator_from_runner_artifact_id, decode_uri_component,
    decode_uri_component_strict, encode_uri_component, is_remote_runner_artifact_path,
    runner_artifact_store_token, ApplyChangeContract, ArtifactUriContract, ExecutionContract,
    LabOffloadExecutionContract, EXECUTION_CONTRACT,
};
