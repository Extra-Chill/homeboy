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

use std::sync::Mutex;

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

fn provider_slot() -> &'static Mutex<Option<Box<dyn ActivityAgentTaskProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn ActivityAgentTaskProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the agent-task activity provider. Called once at startup by the
/// agent-task layer.
pub fn register_activity_agent_task_provider(provider: Box<dyn ActivityAgentTaskProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("activity agent-task provider lock");
    *slot = Some(provider);
}

/// The agent-task activity items via the registered provider (or none when the
/// agent-task subsystem is absent).
pub(crate) fn agent_task_activity_items() -> Result<Vec<ActivityItem>> {
    let slot = provider_slot()
        .lock()
        .expect("activity agent-task provider lock");
    match slot.as_deref() {
        Some(provider) => provider.agent_task_activity_items(),
        None => NoopProvider.agent_task_activity_items(),
    }
}

/// The agent-task record-health summary (as JSON) via the registered provider
/// (or an empty summary when the agent-task subsystem is absent).
pub(crate) fn agent_task_record_health() -> Result<Value> {
    let slot = provider_slot()
        .lock()
        .expect("activity agent-task provider lock");
    match slot.as_deref() {
        Some(provider) => provider.agent_task_record_health(),
        None => NoopProvider.agent_task_record_health(),
    }
}
