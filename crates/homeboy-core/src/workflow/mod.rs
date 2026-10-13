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
pub use definition::{WorkflowDefinition, WORKFLOW_SCHEMA};
pub use gate::Gate;

/// Directory of registered workflow definitions: `<config>/workflows/<id>.json`.
pub fn registry_dir() -> crate::error::Result<std::path::PathBuf> {
    Ok(crate::paths::homeboy()?.join("workflows"))
}
