//! Agent-task implementation of the controller pin-reference hook.
//!
//! Core's controller-runtime retention logic asks which pinned executables are
//! still referenced by a nonterminal durable agent-task record. That query is
//! agent-task behavior — it reads the lifecycle store and inspects each record's
//! state and metadata — so it is provided to core through the
//! `ControllerPinReferenceProvider` hook instead of core calling the agent-task
//! subsystem directly.

use std::path::PathBuf;

use serde_json::Value;

use crate::agent_task_lifecycle::{list_records_with_health, AgentTaskRunState};
use homeboy_core::controller_pin_reference::{
    register_controller_pin_reference_provider, ControllerPinReferenceProvider,
};
use homeboy_core::Result;

/// JSON pointer to a record's originating controller-runtime pinned executable.
/// Mirrors core's `CONTROLLER_RUNTIME_METADATA_KEY` layout.
const PINNED_EXECUTABLE_POINTER: &str = "/controller_runtime/originating/pinned_executable";

struct AgentTaskControllerPinReferenceProvider;

impl ControllerPinReferenceProvider for AgentTaskControllerPinReferenceProvider {
    fn referenced_controller_pins(&self) -> Result<Vec<PathBuf>> {
        let (records, _) = list_records_with_health()?;
        let mut referenced = Vec::new();
        for record in records {
            if !state_retains_pin(&record) {
                continue;
            }
            if let Some(path) = record
                .metadata
                .pointer(PINNED_EXECUTABLE_POINTER)
                .and_then(Value::as_str)
                .map(PathBuf::from)
            {
                referenced.push(path);
            }
        }
        Ok(referenced)
    }
}

/// A record whose state can still operate on its pin retains it: queued,
/// running, and recoverable-partial records may be re-run by lifecycle
/// recovery, so their originating pinned executable must survive pruning.
fn state_retains_pin(record: &crate::agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    if record.lifecycle.artifact_retention.status
        == homeboy_core::run_lifecycle_record::ArtifactRetentionStatus::Retained
    {
        return true;
    }
    matches!(
        record.state,
        AgentTaskRunState::Queued
            | AgentTaskRunState::Running
            | AgentTaskRunState::CandidateRecoverable
            | AgentTaskRunState::PartialRecoverable
            | AgentTaskRunState::PartialFailure
    )
}

/// Register the agent-task controller pin-reference provider. Called once at
/// startup so core's controller-runtime retention report can discover
/// still-referenced pins without depending on the agent-task subsystem.
pub fn register() {
    register_controller_pin_reference_provider(Box::new(AgentTaskControllerPinReferenceProvider));
}
