//! Run-level proof envelope: contract types re-exported from
//! `homeboy-gate-contract`.
//!
//! The pure proof data types, builders, and classification helpers live in the
//! leaf `homeboy-gate-contract` crate (alongside the mutually-dependent gate and
//! plan types).

pub use homeboy_gate_contract::proof::{
    gate_scope_label, gate_status_label, is_ci_equivalent_gate, proof_runner_label, proof_scope,
    HomeboyProof, HomeboyProofArtifactPurpose, HomeboyProofArtifactRef,
    HomeboyProofArtifactRequirement, HomeboyProofEnvironmentDisposition,
    HomeboyProofEnvironmentVariable, HomeboyProofGap, HomeboyProofGapKind, HomeboyProofProvenance,
    HomeboyProofRunner, HomeboyProofScope, HOMEBOY_PROOF_SCHEMA,
};
