//! Pure serializable control-plane identity and resource contract types.
//!
//! These behavior-free data structures name the resource identities Homeboy
//! already persists as untyped strings — mission (Cook / fanout portfolio),
//! run, task, attempt, execution, and provider session — and
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
pub mod review;
pub mod submission;

pub use action::{
    ControlPlaneActionAcknowledgement, ControlPlaneActionFence, ControlPlaneActionIntent,
    ControlPlaneActionOutcome, ControlPlaneActionPayload, ControlPlaneActionRequest,
    ControlPlaneActionResource, ControlPlaneCancelDisposition, ControlPlaneCancelParameters,
    ControlPlaneCancelResult, ControlPlaneEffectAudit, ControlPlaneEffectExecutionState,
    ControlPlaneEffectLease, ControlPlaneEffectState, ControlPlaneEffectStatus,
    ControlPlaneEffectTerminal, ControlPlanePlacementUpdateParameters,
    ControlPlaneProviderRouteOverride, ControlPlaneQuarantineParameters,
    ControlPlaneRetryParameters, EffectId, CONTROL_PLANE_ACTION_ACKNOWLEDGEMENT_SCHEMA,
    CONTROL_PLANE_ACTION_FENCE_SCHEMA, CONTROL_PLANE_ACTION_INTENT_SCHEMA,
    CONTROL_PLANE_ACTION_REQUEST_SCHEMA, CONTROL_PLANE_CANCEL_PARAMETERS_SCHEMA,
    CONTROL_PLANE_CANCEL_RESULT_SCHEMA, CONTROL_PLANE_EFFECT_AUDIT_SCHEMA,
    CONTROL_PLANE_EFFECT_LEASE_SCHEMA, CONTROL_PLANE_EFFECT_STATUS_SCHEMA,
    CONTROL_PLANE_EFFECT_TERMINAL_SCHEMA, CONTROL_PLANE_EMPTY_ACTION_PAYLOAD_SCHEMA,
    CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA, CONTROL_PLANE_PLACEMENT_UPDATE_RESULT_SCHEMA,
    CONTROL_PLANE_PROMOTE_PARAMETERS_SCHEMA, CONTROL_PLANE_PROMOTE_RESULT_SCHEMA,
    CONTROL_PLANE_QUARANTINE_PARAMETERS_SCHEMA, CONTROL_PLANE_QUARANTINE_RESULT_SCHEMA,
    CONTROL_PLANE_REARM_RESULT_SCHEMA, CONTROL_PLANE_RESUME_RESULT_SCHEMA,
    CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA, CONTROL_PLANE_RETRY_RESULT_SCHEMA,
};
pub use capabilities::{
    ControlPlaneCapabilities, ControlPlaneCompatibilityWindow, ControlPlaneOperation,
    ControlPlaneResource, CONTROL_PLANE_CAPABILITIES_SCHEMA,
};
pub use control_plane_ref::{ControlPlaneRef, ControlPlaneRefError};
pub use event::{
    ControlPlaneEvent, ControlPlaneEventAppendRequest, ControlPlaneEventPage,
    ControlPlaneEventRetention, ControlPlaneEventSource, CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA,
    CONTROL_PLANE_EVENT_PAGE_SCHEMA, CONTROL_PLANE_EVENT_RETENTION_SCHEMA,
    CONTROL_PLANE_EVENT_SCHEMA,
};
pub use identity::{
    AttemptCursor, AttemptId, EventCursor, EventId, ExecutionId, IdentityError, MissionCursor,
    MissionId, ProviderSessionId, ReferenceId, RunCursor, RunId, TaskCursor, TaskId,
};
pub use resolve::{resolve, IdentityKind, ResolveError, ResolvedIdentities};
pub use resource::{
    ControlPlaneAction, ControlPlaneActionAvailability, ControlPlaneActionConfirmation,
    ControlPlaneActionEligibility, ControlPlaneActionEligibilityReport, ControlPlaneAdmissionRetry,
    ControlPlaneAdmissionRetryDisposition, ControlPlaneAttempt, ControlPlaneAttemptListRequest,
    ControlPlaneAttemptPage, ControlPlaneBlocker, ControlPlaneError, ControlPlaneErrorClass,
    ControlPlaneEvidenceRef, ControlPlaneExecution, ControlPlaneExecutionPage,
    ControlPlaneLiveness, ControlPlaneLocation, ControlPlaneMission,
    ControlPlaneMissionListRequest, ControlPlaneMissionPage, ControlPlaneOwner,
    ControlPlaneProviderSummary, ControlPlaneReference, ControlPlaneReferencePage,
    ControlPlaneReferenceRegistration, ControlPlaneReferenceType, ControlPlaneResult,
    ControlPlaneRun, ControlPlaneRunListRequest, ControlPlaneRunPage, ControlPlaneRunPlacement,
    ControlPlaneRunPlacementEffective, ControlPlaneRunPlacementRequested,
    ControlPlaneRunPlacementSelected, ControlPlaneRunState, ControlPlaneRuntime, ControlPlaneState,
    ControlPlaneStateSummary, ControlPlaneTask, ControlPlaneTaskListRequest, ControlPlaneTaskPage,
    CONTROL_PLANE_ACTION_ELIGIBILITY_SCHEMA, CONTROL_PLANE_ATTEMPT_PAGE_SCHEMA,
    CONTROL_PLANE_ATTEMPT_SCHEMA, CONTROL_PLANE_EXECUTION_PAGE_SCHEMA,
    CONTROL_PLANE_EXECUTION_SCHEMA, CONTROL_PLANE_MISSION_PAGE_SCHEMA,
    CONTROL_PLANE_MISSION_SCHEMA, CONTROL_PLANE_REFERENCE_PAGE_SCHEMA,
    CONTROL_PLANE_REFERENCE_REGISTRATION_SCHEMA, CONTROL_PLANE_REFERENCE_SCHEMA,
    CONTROL_PLANE_RESULT_SCHEMA, CONTROL_PLANE_RUN_PAGE_SCHEMA,
    CONTROL_PLANE_RUN_PLACEMENT_ID_BOUND, CONTROL_PLANE_RUN_SCHEMA, CONTROL_PLANE_TASK_PAGE_SCHEMA,
    CONTROL_PLANE_TASK_SCHEMA,
};
pub use review::{
    ControlPlaneRunReview, ControlPlaneRunReviewRequest, CONTROL_PLANE_RUN_REVIEW_SCHEMA,
};
pub use submission::{
    ControlPlaneSubmissionAcknowledgement, ControlPlaneSubmissionRequest,
    CONTROL_PLANE_SUBMISSION_ACKNOWLEDGEMENT_SCHEMA, CONTROL_PLANE_SUBMISSION_REQUEST_SCHEMA,
};
