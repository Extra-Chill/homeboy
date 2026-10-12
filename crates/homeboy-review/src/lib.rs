//! Review subsystem for homeboy: renders the scoped lint + test umbrella (the
//! `review` command) into human- and machine-readable output, including
//! artifact-derived finding summaries.
//!
//! Depends on homeboy-core; core does not depend on it.

pub mod review;

pub use review::*;
