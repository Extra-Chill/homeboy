use crate::core::config;
use crate::core::error::Result;
use crate::core::{CreateOutput, MergeOutput, RemoveResult};

mod entity;
mod lifecycle;
mod preview;
mod readiness;
mod runtime;
mod types;
mod validation;

pub use entity::{expose, native_preview_token_record, native_preview_token_sha256};
pub use lifecycle::{local_url, start, status, stop};
pub use preview::validate_native_preview_claim;
pub use types::*;

// Re-exported for sibling `tunnel_tests` integration coverage under `core`,
// which pulls these in via `use super::tunnel::*`. The glob consumer is not
// seen by the unused-imports lint, so the allow keeps a clean non-test build.
#[allow(unused_imports)]
pub(in crate::core) use preview::{preview_artifact_for, preview_policy_allows};

entity_crud!(ServiceTunnel; list_ids, merge);
