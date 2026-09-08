//! Control-plane HTTP provider hook.
//!
//! Core owns the versioned HTTP adapter. The orchestration service that
//! assembles [`ControlPlaneRun`] lives in the agent-task layer and registers
//! here so core stays agent-task-agnostic.

use homeboy_control_plane_contract::{
    ControlPlaneActionAcknowledgement, ControlPlaneActionRequest, ControlPlaneCapabilities,
    ControlPlaneError, ControlPlaneEventPage, ControlPlaneMission, ControlPlaneMissionListRequest,
    ControlPlaneMissionPage, ControlPlaneOperation, ControlPlaneRun, ControlPlaneRunListRequest,
    ControlPlaneRunPage, ControlPlaneRunReview, ControlPlaneRunReviewRequest,
    ControlPlaneSubmissionAcknowledgement, ControlPlaneSubmissionRequest, ControlPlaneTask,
    ControlPlaneTaskListRequest, ControlPlaneTaskPage, EventCursor, MissionId, RunId, TaskId,
};

/// Supplies control-plane capabilities and resource reads to the HTTP adapter.
pub trait ControlPlaneProvider: Send + Sync {
    fn capabilities(&self) -> ControlPlaneCapabilities {
        ControlPlaneCapabilities::new(Vec::new(), vec![ControlPlaneOperation::GetCapabilities])
    }

    fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn mission(&self, requested_id: &MissionId) -> Result<ControlPlaneMission, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane mission not found: {requested_id}"
        )))
    }

    fn missions(
        &self,
        _request: &ControlPlaneMissionListRequest,
    ) -> Result<ControlPlaneMissionPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane mission discovery is unavailable",
        ))
    }

    fn runs(
        &self,
        _request: &ControlPlaneRunListRequest,
    ) -> Result<ControlPlaneRunPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane run discovery is unavailable",
        ))
    }

    fn task(&self, run: &RunId, task: &TaskId) -> Result<ControlPlaneTask, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane task not found in run {run}: {task}"
        )))
    }

    fn tasks(
        &self,
        _run: &RunId,
        _request: &ControlPlaneTaskListRequest,
    ) -> Result<ControlPlaneTaskPage, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane task discovery is unavailable",
        ))
    }

    fn submit(
        &self,
        _request: &ControlPlaneSubmissionRequest,
    ) -> Result<ControlPlaneSubmissionAcknowledgement, ControlPlaneError> {
        Err(ControlPlaneError::unavailable(
            "control-plane run submission is unavailable",
        ))
    }

    fn events(
        &self,
        requested_id: &RunId,
        _cursor: Option<&EventCursor>,
    ) -> Result<ControlPlaneEventPage, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn review(
        &self,
        requested_id: &RunId,
        _request: &ControlPlaneRunReviewRequest,
    ) -> Result<ControlPlaneRunReview, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }

    fn execute_action(
        &self,
        requested_id: &RunId,
        _request: &ControlPlaneActionRequest,
    ) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
        Err(ControlPlaneError::not_found(format!(
            "control-plane run not found: {requested_id}"
        )))
    }
}

struct NoopProvider;

impl ControlPlaneProvider for NoopProvider {}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ControlPlaneProvider,
    noop: NoopProvider,
    /// Register the control-plane orchestration provider. Called once at
    /// startup by the agent-task layer.
    register: pub fn register_control_plane_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

pub fn capabilities() -> ControlPlaneCapabilities {
    with_provider(|provider| provider.capabilities())
}

pub fn run(requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
    with_provider(|provider| provider.run(requested_id))
}

pub fn mission(requested_id: &MissionId) -> Result<ControlPlaneMission, ControlPlaneError> {
    with_provider(|provider| provider.mission(requested_id))
}

pub fn missions(
    request: &ControlPlaneMissionListRequest,
) -> Result<ControlPlaneMissionPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.missions(request))
}

pub fn runs(
    request: &ControlPlaneRunListRequest,
) -> Result<ControlPlaneRunPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.runs(request))
}

pub fn task(run: &RunId, task: &TaskId) -> Result<ControlPlaneTask, ControlPlaneError> {
    with_provider(|provider| provider.task(run, task))
}

pub fn tasks(
    run: &RunId,
    request: &ControlPlaneTaskListRequest,
) -> Result<ControlPlaneTaskPage, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.tasks(run, request))
}

pub fn submit(
    request: &ControlPlaneSubmissionRequest,
) -> Result<ControlPlaneSubmissionAcknowledgement, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.submit(request))
}

pub fn events(
    requested_id: &RunId,
    cursor: Option<&EventCursor>,
) -> Result<ControlPlaneEventPage, ControlPlaneError> {
    with_provider(|provider| provider.events(requested_id, cursor))
}

pub fn review(
    requested_id: &RunId,
    request: &ControlPlaneRunReviewRequest,
) -> Result<ControlPlaneRunReview, ControlPlaneError> {
    request.validate()?;
    with_provider(|provider| provider.review(requested_id, request))
}

pub fn execute_action(
    requested_id: &RunId,
    request: &ControlPlaneActionRequest,
) -> Result<ControlPlaneActionAcknowledgement, ControlPlaneError> {
    with_provider(|provider| provider.execute_action(requested_id, request))
}

#[cfg(test)]
mod tests {
    use super::{ControlPlaneProvider, NoopProvider};
    use homeboy_control_plane_contract::ControlPlaneOperation;

    #[test]
    fn noop_provider_advertises_discovery_without_run_reads() {
        let capabilities = NoopProvider.capabilities();
        assert!(capabilities.resources.is_empty());
        assert_eq!(
            capabilities.operations,
            vec![ControlPlaneOperation::GetCapabilities]
        );
    }
}
