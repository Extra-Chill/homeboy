//! Review subsystem for homeboy: renders the scoped audit + lint + test
//! umbrella (the `review` command) into human- and machine-readable output,
//! including audit findings and artifact-derived finding summaries.
//!
//! Depends on homeboy-core; core does not depend on it.

pub mod review;

pub use review::*;
