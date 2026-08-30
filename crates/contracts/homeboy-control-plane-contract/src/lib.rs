//! Pure serializable control-plane identity and resource contract types.
//!
//! These behavior-free data structures name the resource identities Homeboy
//! already persists as untyped strings — mission (Cook / fanout portfolio),
//! run, task, attempt, execution (runner job), and provider session — and
//! resolve those strings deterministically. They also version the run resource
//! and the shared result envelope once. They depend only on serde, which
//! keeps this a leaf crate other crates can depend on without pulling in core.

pub mod action;
pub mod capabilities;
pub mod control_plane_ref;
pub mod event;
pub mod identity;
pub mod resolve;
pub mod resource;

pub use action::{
    ControlPlaneActionAcknowledgement, ControlPlaneActionOutcome, ControlPlaneActionPayload,
    ControlPlaneActionRequest, ControlPlaneCancelParameters, ControlPlaneRetryParameters,
    CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA, CONTROL_PLANE_ACTION_REQUEST_SCHEMA,
    CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA, CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
    CONTROL_PLANE_RESUME_RESULT_SCHEMA, CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA,
    CONTROL_PLANE_RETRY_RESULT_SCHEMA,
};
pub use capabilities::{
    ControlPlaneCapabilities, ControlPlaneOperation, ControlPlaneResource,
    CONTROL_PLANE_CAPABILITIES_SCHEMA,
};
pub use control_plane_ref::{ControlPlaneRef, ControlPlaneRefError};
pub use event::{
    ControlPlaneEvent, ControlPlaneEventPage, ControlPlaneEventSource,
    CONTROL_PLANE_EVENT_PAGE_SCHEMA, CONTROL_PLANE_EVENT_SCHEMA,
};
pub use identity::{
    AttemptId, EventCursor, EventId, ExecutionId, IdentityError, MissionId, ProviderSessionId,
    RunId, TaskId,
};
pub use resolve::{resolve, IdentityKind, ResolveError, ResolvedIdentities};
pub use resource::{
    ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionConfirmation,
    ControlPlaneActionEligibility, ControlPlaneActionEligibilityReport, ControlPlaneBlocker,
    ControlPlaneError, ControlPlaneErrorClass, ControlPlaneEvidenceRef, ControlPlaneLocation,
    ControlPlaneOwner, ControlPlaneProviderSummary, ControlPlaneResult, ControlPlaneRun,
    ControlPlaneRunState, ControlPlaneRuntime, ControlPlaneStateSummary,
    CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA, CONTROL_PLANE_RESULT_SCHEMA, CONTROL_PLANE_RUN_SCHEMA,
};
