//! Agent-task implementation of the controller pin-reference hook.
//!
//! Core's controller-runtime retention logic asks which pinned executables are
//! still referenced by a durable agent-task record. That query is agent-task
//! behavior — it reads the lifecycle store and inspects each record's
//! non-terminal state — so it is provided to core
//! through the `ControllerPinReferenceProvider` hook instead of core calling
//! the agent-task subsystem directly.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use homeboy_core::controller_pin_reference::{
    register_controller_pin_reference_provider, ControllerPinProtectionReason,
    ControllerPinReferenceProvider, ReferencedControllerPin,
};
use homeboy_core::Result;

/// JSON pointers to a record's originating controller-runtime executables.
/// Mirrors core's `CONTROLLER_RUNTIME_METADATA_KEY` layout.
const ORIGINATING_EXECUTABLE_POINTER: &str = "/controller_runtime/originating/executable";
const PINNED_EXECUTABLE_POINTER: &str = "/controller_runtime/originating/pinned_executable";

struct AgentTaskControllerPinReferenceProvider;

impl ControllerPinReferenceProvider for AgentTaskControllerPinReferenceProvider {
    fn referenced_controller_pins(&self) -> Result<Vec<ReferencedControllerPin>> {
        // Pins are retained against the records read here, so both halves must
        // name one installation (#7505).
        let lifecycle_store = super::AgentTaskLifecycleStore::from_current_environment()?;
        let (records, _) = super::list_records_with_health_in_store(&lifecycle_store)?;
        let mut referenced = BTreeMap::new();
        for record in records {
            let Some(reason) = state_retains_pin(&record) else {
                continue;
            };
            for pointer in [ORIGINATING_EXECUTABLE_POINTER, PINNED_EXECUTABLE_POINTER] {
                if let Some(path) = record
                    .metadata
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                {
                    referenced
                        .entry(path)
                        .and_modify(|existing: &mut ControllerPinProtectionReason| {
                            *existing = (*existing).max(reason);
                        })
                        .or_insert(reason);
                }
            }
        }
        Ok(referenced
            .into_iter()
            .map(|(path, reason)| ReferencedControllerPin { path, reason })
            .collect())
    }
}

/// Only non-terminal work needs its Cook child runtime copy.
fn state_retains_pin(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<ControllerPinProtectionReason> {
    (!record.state().is_terminal()).then_some(ControllerPinProtectionReason::ProtectedInFlight)
}

/// Register the agent-task controller pin-reference provider. Called once at
/// startup so core's controller-runtime retention report can discover
/// still-referenced pins without depending on the agent-task subsystem.
pub fn register() {
    register_controller_pin_reference_provider(Box::new(AgentTaskControllerPinReferenceProvider));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_lifecycle::{AgentTaskRunRecord, AgentTaskRunState};
    use homeboy_core::run_lifecycle_record::ArtifactRetentionStatus;

    fn record(state: AgentTaskRunState, submitted_at: &str) -> AgentTaskRunRecord {
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": "run",
            "plan_id": "plan",
            "state": state,
            "submitted_at": submitted_at,
            "plan_path": "/plan"
        }))
        .expect("record")
    }

    #[test]
    fn controller_pin_in_flight_records_retain_regardless_of_age() {
        for state in [AgentTaskRunState::Queued, AgentTaskRunState::Running] {
            assert_eq!(
                state_retains_pin(&record(state, "2020-01-01T00:00:00Z")),
                Some(ControllerPinProtectionReason::ProtectedInFlight)
            );
        }
    }

    #[test]
    fn terminal_records_never_retain_pins_even_with_open_mutations_and_retained_artifacts() {
        for state in [
            AgentTaskRunState::Succeeded,
            AgentTaskRunState::Failed,
            AgentTaskRunState::Cancelled,
        ] {
            let mut retained = record(state, &chrono::Utc::now().to_rfc3339());
            retained.lifecycle.artifact_retention.status = ArtifactRetentionStatus::Retained;
            assert_eq!(state_retains_pin(&retained), None);
        }
    }
}
