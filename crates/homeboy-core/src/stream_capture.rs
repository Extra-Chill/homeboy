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

pub use homeboy_extension_contract::StreamCaptureMetadata;
use serde::{Deserialize, Serialize};
