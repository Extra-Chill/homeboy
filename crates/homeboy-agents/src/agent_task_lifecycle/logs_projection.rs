//! Canonical ledger projection for `agent-task logs` and control-plane events.

use super::*;

pub fn logs(run_id: &str) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    logs_in_store(&lifecycle_store, run_id)
}

pub fn logs_from_cursor(
    run_id: &str,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    event_page_in_store(&lifecycle_store, run_id, cursor)
}

/// [`logs`] against explicitly injected durable lifecycle roots.
pub fn logs_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage> {
    event_page_in_store(lifecycle_store, run_id, None)
}

pub fn control_plane_events_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> std::result::Result<
    homeboy_control_plane_contract::ControlPlaneEventPage,
    homeboy_control_plane_contract::ControlPlaneError,
> {
    let (run, events) =
        event_stream_in_store(lifecycle_store, run_id).map_err(control_plane_event_read_error)?;
    crate::orchestration::event_page(run, events, cursor)
}

pub fn control_plane_event_retention_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> std::result::Result<
    homeboy_control_plane_contract::ControlPlaneEventRetention,
    homeboy_control_plane_contract::ControlPlaneError,
> {
    let (run, events) =
        event_stream_in_store(lifecycle_store, run_id).map_err(control_plane_event_read_error)?;
    crate::orchestration::event_retention(run, &events)
}

fn event_page_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    cursor: Option<&homeboy_control_plane_contract::EventCursor>,
) -> Result<homeboy_control_plane_contract::ControlPlaneEventPage> {
    control_plane_events_in_store(lifecycle_store, run_id, cursor).map_err(|error| {
        match error.class {
            homeboy_control_plane_contract::ControlPlaneErrorClass::Unavailable => {
                Error::internal_unexpected(error.message)
            }
            _ => Error::validation_invalid_argument("cursor", error.message, None, None),
        }
    })
}

fn event_stream_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<(
    homeboy_control_plane_contract::RunId,
    Vec<homeboy_control_plane_contract::ControlPlaneEvent>,
)> {
    let record = status_in_store(lifecycle_store, run_id)?;
    let run = homeboy_control_plane_contract::RunId::new(&record.run_id).map_err(|error| {
        Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let events = lifecycle_store
        .open_observation_readonly()?
        .control_plane_event_stream(&run)?
        .unwrap_or_default();
    Ok((run, events))
}

fn control_plane_event_read_error(
    error: Error,
) -> homeboy_control_plane_contract::ControlPlaneError {
    if error.code == homeboy_core::ErrorCode::ValidationInvalidArgument
        && error.message.contains("not found")
    {
        homeboy_control_plane_contract::ControlPlaneError::not_found(error.message)
    } else {
        homeboy_control_plane_contract::ControlPlaneError::unavailable(error.message)
    }
}
