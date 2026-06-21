//! Shared bounded-stream capture metadata.
//!
//! Several execution paths (runner script capture, agent-task promotion,
//! agent-tool control plane, deploy/upgrade artifact reads, extension
//! self-checks) bound a captured stream to a retained-byte cap and record how
//! much was seen versus retained. They independently declared identical
//! truncation-metadata structs (`{ limit_bytes, seen_bytes, retained_bytes,
//! truncated }`). This module owns that single shared shape so the concept is
//! defined once and the serialized contract stays consistent across every
//! capture site.

use serde::{Deserialize, Serialize};

/// Truncation metadata describing how much of a captured stream was retained.
///
/// `seen_bytes` is the total observed length of the source; `retained_bytes`
/// is how many bytes survived the `limit_bytes` cap; `truncated` records
/// whether the source exceeded the cap (so the overflow is observable rather
/// than silently dropped).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamCaptureMetadata {
    pub limit_bytes: usize,
    pub seen_bytes: usize,
    pub retained_bytes: usize,
    pub truncated: bool,
}
