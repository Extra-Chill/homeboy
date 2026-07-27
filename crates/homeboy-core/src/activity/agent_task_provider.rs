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
    /// Resolve a single agent-task activity item by id through an indexed
    /// lookup, without listing every durable record.
    ///
    /// Defaults to `Ok(None)` so a provider without an indexed lookup — and the
    /// no-op provider when no agent-task subsystem is present — contributes
    /// nothing and id resolution falls through to the next probe.
    fn probe_by_id(&self, _id: &str) -> Result<Option<ActivityItem>> {
        Ok(None)
    }

    /// Project every durable agent-task record into an activity item, together
    /// with the record-health summary for the same records.
    ///
    /// Items and health are one call because they are one read of the same
    /// durable records. Asking for them separately made the activity report
    /// walk the corpus twice (#10308). The health summary is serialized as JSON
    /// so core does not depend on the agent-task health type.
    fn agent_task_activity(&self) -> Result<(Vec<ActivityItem>, Value)>;
}

struct NoopProvider;

impl ActivityAgentTaskProvider for NoopProvider {
    fn agent_task_activity(&self) -> Result<(Vec<ActivityItem>, Value)> {
        Ok((Vec::new(), Value::Null))
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

/// Resolve one agent-task activity item by id via the registered provider's
/// indexed lookup (or none when the agent-task subsystem is absent).
pub(crate) fn probe_by_id(id: &str) -> Result<Option<ActivityItem>> {
    with_provider(|p| p.probe_by_id(id))
}

/// The agent-task activity items and record-health summary via the registered
/// provider (or no items and an empty summary when the agent-task subsystem is
/// absent).
pub(crate) fn agent_task_activity() -> Result<(Vec<ActivityItem>, Value)> {
    with_provider(|p| p.agent_task_activity())
}
