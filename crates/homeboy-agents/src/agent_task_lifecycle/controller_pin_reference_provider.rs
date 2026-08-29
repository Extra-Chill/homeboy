//! Agent-task implementation of the controller pin-reference hook.
//!
//! Core's controller-runtime retention logic asks which pinned executables are
//! still referenced by a durable agent-task record. That query is agent-task
//! behavior — it reads the lifecycle store and inspects each record's
//! canonical lifecycle-action eligibility and age — so it is provided to core
//! through the `ControllerPinReferenceProvider` hook instead of core calling
//! the agent-task subsystem directly.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::agent_task_lifecycle::{lifecycle_action_eligibility, AgentTaskActionAvailability};
use homeboy_core::controller_pin_reference::{
    register_controller_pin_reference_provider, ControllerPinProtectionReason,
    ControllerPinReferenceProvider, ReferencedControllerPin,
};
use homeboy_core::controller_runtime::{
    resolve_cleanup_options, ControllerRuntimeRetentionOverrides,
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
        let min_age =
            resolve_cleanup_options(false, ControllerRuntimeRetentionOverrides::default()).min_age;
        let mut referenced = BTreeMap::new();
        for record in records {
            let Some(reason) = state_retains_pin(&record, min_age) else {
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

/// A record retains its pin while a mutating lifecycle action remains
/// available or indeterminate and the record is still inside the configured
/// retention window. In-flight records keep the pin regardless of age.
fn state_retains_pin(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    min_age: Duration,
) -> Option<ControllerPinProtectionReason> {
    if !record.state.is_terminal() {
        return Some(ControllerPinProtectionReason::ProtectedInFlight);
    }
    if !mutating_lifecycle_action_remains_open(record) {
        return None;
    }
    if record_age_seconds(record).is_some_and(|age| age >= min_age.as_secs()) {
        return None;
    }
    Some(ControllerPinProtectionReason::ProtectedByPendingMutation)
}

fn mutating_lifecycle_action_remains_open(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    lifecycle_action_eligibility(record, None)
        .actions
        .iter()
        .any(|eligibility| {
            eligibility.action.is_mutating()
                && matches!(
                    eligibility.availability,
                    AgentTaskActionAvailability::Available
                        | AgentTaskActionAvailability::Indeterminate
                )
        })
}

fn record_age_seconds(record: &crate::agent_task_lifecycle::AgentTaskRunRecord) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(&record.submitted_at)
        .ok()
        .map(|submitted| {
            (chrono::Utc::now() - submitted.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0) as u64
        })
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
    use crate::agent_task_lifecycle::{
        AgentTaskAcceptanceVerdict, AgentTaskRunRecord, AgentTaskRunState,
    };
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

    fn exhaust_repair_budget(record: &mut AgentTaskRunRecord) {
        record.acceptance = serde_json::from_value(serde_json::json!({
            "requirement": { "authority": "test", "policy": "test" },
            "verdict": AgentTaskAcceptanceVerdict::Accepted,
            "candidate": {
                "schema": "homeboy/agent-task-candidate-fingerprint/v1",
                "target_path": "/tmp",
                "head": "0",
                "base": "0",
                "changed_files": [],
                "sha256": "0"
            },
            "base_sha": "0",
            "repair_attempts": 2
        }))
        .expect("acceptance");
    }

    #[test]
    fn controller_pin_in_flight_records_retain_regardless_of_age() {
        let min_age = Duration::ZERO;
        for state in [AgentTaskRunState::Queued, AgentTaskRunState::Running] {
            assert_eq!(
                state_retains_pin(&record(state, "2020-01-01T00:00:00Z"), min_age),
                Some(ControllerPinProtectionReason::ProtectedInFlight)
            );
        }
    }

    #[test]
    fn controller_pin_pending_mutation_inside_the_window_retains() {
        let min_age = Duration::from_secs(86_400);
        let now = chrono::Utc::now().to_rfc3339();
        assert_eq!(
            state_retains_pin(&record(AgentTaskRunState::Succeeded, &now), min_age),
            Some(ControllerPinProtectionReason::ProtectedByPendingMutation)
        );
        assert_eq!(
            state_retains_pin(&record(AgentTaskRunState::Failed, &now), min_age),
            Some(ControllerPinProtectionReason::ProtectedByPendingMutation)
        );
    }

    #[test]
    fn controller_pin_terminal_outside_the_window_does_not_retain_artifact_retention() {
        let min_age = Duration::from_secs(86_400);
        let mut retained = record(AgentTaskRunState::Succeeded, "2020-01-01T00:00:00Z");
        retained.lifecycle.artifact_retention.status = ArtifactRetentionStatus::Retained;
        assert_eq!(state_retains_pin(&retained, min_age), None);
    }

    #[test]
    fn controller_pin_read_only_actions_do_not_retain() {
        let min_age = Duration::from_secs(u64::MAX);
        let mut failed = record(AgentTaskRunState::Failed, "2026-01-01T00:00:00Z");
        exhaust_repair_budget(&mut failed);
        assert_eq!(state_retains_pin(&failed, min_age), None);
    }
}
