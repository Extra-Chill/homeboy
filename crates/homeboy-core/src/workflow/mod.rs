//! Declarative workflows: `homeboy/workflow/v1`.
//!
//! A workflow instance is one daemon `workflow` job. It never runs work
//! itself: it submits `step` jobs and evaluates gates. Every decision is made
//! by the pure [`advance::advance`] function, and the daemon driver commits
//! its result in one store transaction.

pub mod advance;
pub mod definition;
pub mod gate;
pub mod template;

pub use advance::{
    advance, Command, Event, FinishedStatus, InstanceState, InstanceStatus, StepPhase, StepResult,
    StepSubmission, SubmissionKind,
};
pub use definition::{Every, WorkflowDefinition, WORKFLOW_SCHEMA};
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
    vec![WorkflowDefinition::parse(RETENTION_WORKFLOW).expect("built-in workflow parses")]
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
    fn retention_is_an_hourly_cleanup_workflow() {
        let retention = super::builtin(super::RETENTION_WORKFLOW_ID).expect("built-in");
        retention.validate(&|_| false).expect("valid");
        let every = retention.every.as_ref().expect("cadence");
        assert_eq!(every.interval().unwrap().as_secs(), 3600);
        assert_eq!(
            retention.steps[0].homeboy.as_deref(),
            Some(&["cleanup".to_string(), "automatic-retention".to_string()][..])
        );
    }
}
