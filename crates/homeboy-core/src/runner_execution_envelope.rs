//! Compatibility facade for the transport-neutral runner contract.
//!
//! New consumers should depend on `homeboy-runner-contract` directly. Core
//! retains this module path because persisted command and extension consumers
//! already import it.

pub use homeboy_runner_contract::*;
