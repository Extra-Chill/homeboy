//! Pure serializable control-plane identity contract types.
//!
//! These behavior-free data structures name the resource identities Homeboy
//! already persists as untyped strings — mission (Cook / fanout portfolio),
//! run, task, attempt, execution (runner job), and provider session — and
//! resolve those strings deterministically. They depend only on serde, which
//! keeps this a leaf crate other crates can depend on without pulling in core.
//!
//! This crate does not serve endpoints, migrate call sites, or change any
//! existing serialized document. Later slices consume these types.

pub mod capabilities;
pub mod control_plane_ref;
pub mod identity;
pub mod resolve;

pub use capabilities::{
    CompatibilityWindow, ControlPlaneCapabilities, ControlPlaneResource,
    CONTROL_PLANE_CAPABILITIES_SCHEMA, LEGACY_COMPATIBILITY_MINOR_VERSIONS,
};
pub use control_plane_ref::{ControlPlaneRef, ControlPlaneRefError};
pub use identity::{
    AttemptId, ExecutionId, IdentityError, MissionId, ProviderSessionId, RunId, TaskId,
};
pub use resolve::{resolve, IdentityKind, ResolveError, ResolvedIdentities};
