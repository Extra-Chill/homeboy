//! Routing to the single durable Cook readers shared with the domain status view.
use homeboy::agents::agent_task_lifecycle::{self, AgentTaskLifecycleStore};

pub(crate) struct LifecycleReadArgs {
    pub run_id: String,
    pub full: bool,
    pub show: Option<String>,
    pub tail: Option<usize>,
}

pub(crate) struct EvidenceArgs {
    pub run_id: String,
    pub kind: Option<String>,
    pub task: Option<String>,
    pub failure_only: bool,
    pub full: bool,
}

impl super::RunsArgs {
    /// Read-only Cook surfaces retain the former lifecycle-reader routing policy.
    pub(crate) fn cook_reader_id(&self) -> Option<&str> {
        let id = match &self.command {
            super::types::RunsCommand::Artifacts(args) => args.run_id.as_str(),
            super::types::RunsCommand::Evidence { run_id, .. } => run_id.as_str(),
            _ => return None,
        };
        (id.starts_with("agent-task-") || owns(id).unwrap_or(false)).then_some(id)
    }
}

pub(crate) fn owns(id: &str) -> homeboy::core::Result<bool> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    Ok(store.cook_index_exists(id)
        || agent_task_lifecycle::run_record_exists_readonly_in_store(&store, id)?)
}
