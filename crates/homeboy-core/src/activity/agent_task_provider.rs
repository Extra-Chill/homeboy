//! Agent-task activity hook.
//!
//! The activity report aggregates work from several sources; the agent-task
//! source lists durable agent-task records and projects each into an
//! [`ActivityItem`], plus a record-health summary. That projection reads
//! agent-task lifecycle records and is therefore agent-task behavior, so it is
//! inverted behind this provider: core owns the activity report and the
//! `ActivityItem` shape, the agent-task layer supplies the items and health.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! provider contributes no items and an empty health summary.

use serde_json::Value;

use super::ActivityItem;
use crate::Result;

/// Supplies the agent-task contribution to the activity report.
pub trait ActivityAgentTaskProvider: Send + Sync {
    /// Project every durable agent-task record into an activity item.
    fn agent_task_activity_items(&self) -> Result<Vec<ActivityItem>>;

    /// The agent-task record-health summary, serialized as JSON so core does not
    /// depend on the agent-task health type.
    fn agent_task_record_health(&self) -> Result<Value>;
}

struct NoopProvider;

impl ActivityAgentTaskProvider for NoopProvider {
    fn agent_task_activity_items(&self) -> Result<Vec<ActivityItem>> {
        Ok(Vec::new())
    }

    fn agent_task_record_health(&self) -> Result<Value> {
        Ok(Value::Null)
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ActivityAgentTaskProvider,
    noop: NoopProvider,
    /// Register the agent-task activity provider. Called once at startup by the
    /// agent-task layer.
    register: pub fn register_activity_agent_task_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// The agent-task activity items via the registered provider (or none when the
/// agent-task subsystem is absent).
pub(crate) fn agent_task_activity_items() -> Result<Vec<ActivityItem>> {
    with_provider(|p| p.agent_task_activity_items())
}

/// The agent-task record-health summary (as JSON) via the registered provider
/// (or an empty summary when the agent-task subsystem is absent).
pub(crate) fn agent_task_record_health() -> Result<Value> {
    with_provider(|p| p.agent_task_record_health())
}
