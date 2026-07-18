//! Pure serializable gate / plan / proof contract types.
//!
//! This crate holds the mutually-dependent data cluster at the heart of
//! Homeboy's workflow model:
//!
//! - **gate** — `HomeboyGateResult` and friends: the outcome of a quality gate.
//! - **plan** — `HomeboyPlan` / `PlanStep` / builders: declared workflow steps.
//! - **proof** — `HomeboyProof` and friends: recorded run evidence.
//!
//! `plan` references `HomeboyGateResult` and `HomeboyProof`; `gate` references
//! `HomeboyPlan`; `proof` references `HomeboyGateResult`. Because these types
//! depend on one another they share a single crate. They are behavior-free
//! (serde + clap `ValueEnum` derives + pure classification helpers), so this is
//! a leaf crate other crates can depend on without pulling in core.
//!
//! The proof *validation* behavior (`validate_proof_value`) and the
//! `loop_spec_validation` provider stay in `homeboy-core`, because validation
//! reaches into core's `artifact_address` / observation layer.

pub mod gate;
pub mod plan;
pub mod proof;
