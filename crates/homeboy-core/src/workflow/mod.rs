//! Declarative workflows: `homeboy/workflow/v1`.
//!
//! A workflow instance is one daemon `workflow` job. It never runs work
//! itself: it submits `step` jobs and evaluates gates. Every decision is made
//! by the pure [`advance::advance`] function, and the daemon driver commits
//! its result in one store transaction.

pub mod ab;
pub mod advance;
pub mod definition;
pub mod gate;
pub mod template;
pub mod timing;

pub use advance::{
    advance, Command, Event, FinishedStatus, InstanceState, InstanceStatus, StepPhase, StepResult,
    StepSubmission, SubmissionKind,
};
pub use definition::{Every, Watch, WatchRef, WorkflowDefinition, WORKFLOW_SCHEMA};
pub use gate::Gate;

/// Directory of registered workflow definitions: `<config>/workflows/<id>.json`.
pub fn registry_dir() -> crate::error::Result<std::path::PathBuf> {
    Ok(crate::paths::homeboy()?.join("workflows"))
}

/// The built-in workflow that keeps Homeboy-managed storage bounded. The
/// daemon registers its cadence when it starts; `workflow cancel
/// homeboy.retention` opts out and the opt-out is kept.
pub const RETENTION_WORKFLOW_ID: &str = "homeboy.retention";

const RETENTION_WORKFLOW: &str = r#"{
  "schema": "homeboy/workflow/v1",
  "id": "homeboy.retention",
  "every": { "interval": "1h" },
  "steps": [
    { "id": "retention", "timeout": "50m", "homeboy": ["cleanup", "automatic-retention"] }
  ]
}"#;

/// Workflows that ship with Homeboy, addressable by id like registered ones.
pub fn builtins() -> Vec<WorkflowDefinition> {
    vec![
        WorkflowDefinition::parse(RETENTION_WORKFLOW).expect("built-in workflow parses"),
        ab::definition(),
    ]
}

/// The built-in workflow named `id`, if any.
pub fn builtin(id: &str) -> Option<WorkflowDefinition> {
    builtins()
        .into_iter()
        .find(|definition| definition.id == id)
}

#[cfg(test)]
mod builtin_tests {
    #[test]
    fn every_builtin_workflow_validates() {
        for definition in super::builtins() {
            definition
                .validate(&|_| false)
                .unwrap_or_else(|error| panic!("{}: {error}", definition.id));
        }
        assert!(super::builtin(super::RETENTION_WORKFLOW_ID).is_some());
    }
}
