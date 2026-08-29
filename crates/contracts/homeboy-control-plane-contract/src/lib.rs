//! Pure serializable control-plane identity and resource contract types.
//!
//! These behavior-free data structures name the resource identities Homeboy
//! already persists as untyped strings — mission (Cook / fanout portfolio),
//! run, task, attempt, execution (runner job), and provider session — and
//! resolve those strings deterministically. They also version the run resource
//! and the shared result envelope once. They depend only on serde, which
//! keeps this a leaf crate other crates can depend on without pulling in core.

pub mod capabilities;
pub mod control_plane_ref;
pub mod identity;
pub mod resolve;
pub mod resource;

pub use capabilities::{
    ControlPlaneCapabilities, ControlPlaneOperation, ControlPlaneResource,
    CONTROL_PLANE_CAPABILITIES_SCHEMA,
};
pub use control_plane_ref::{ControlPlaneRef, ControlPlaneRefError};
pub use identity::{
    AttemptId, ExecutionId, IdentityError, MissionId, ProviderSessionId, RunId, TaskId,
};
pub use resolve::{resolve, IdentityKind, ResolveError, ResolvedIdentities};
pub use resource::{
    ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEvidenceRef, ControlPlaneLocation,
    ControlPlaneResult, ControlPlaneRun, ControlPlaneRunState, CONTROL_PLANE_RESULT_SCHEMA,
    CONTROL_PLANE_RUN_SCHEMA,
};
