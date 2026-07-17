//! Agent-task secret-resolution hook.
//!
//! Trace secret-env resolution consults several sources, one of which is the
//! agent-task secret store. That lookup is agent-task behavior, so it is
//! inverted behind this provider: core owns trace secret resolution, the
//! agent-task layer resolves agent-task secrets.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! resolves nothing, so trace resolution falls through to its other sources.

use std::sync::Mutex;

/// Resolves secret-env values from the agent-task secret store.
pub trait AgentTaskSecretProvider: Send + Sync {
    /// Resolve the named secret-env variables from the agent-task secret store,
    /// returning the `(name, value)` pairs that were found.
    fn resolve_agent_task_secret_env(&self, names: &[String]) -> Vec<(String, String)>;
}

struct NoopProvider;

impl AgentTaskSecretProvider for NoopProvider {
    fn resolve_agent_task_secret_env(&self, _names: &[String]) -> Vec<(String, String)> {
        Vec::new()
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn AgentTaskSecretProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn AgentTaskSecretProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the agent-task secret provider. Called once at startup by the
/// agent-task layer.
pub fn register_agent_task_secret_provider(provider: Box<dyn AgentTaskSecretProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("agent-task secret provider lock");
    *slot = Some(provider);
}

/// Resolve agent-task secret-env values via the registered provider (or none
/// when the agent-task subsystem is absent).
pub(crate) fn resolve_agent_task_secret_env(names: &[String]) -> Vec<(String, String)> {
    let slot = provider_slot()
        .lock()
        .expect("agent-task secret provider lock");
    match slot.as_deref() {
        Some(provider) => provider.resolve_agent_task_secret_env(names),
        None => NoopProvider.resolve_agent_task_secret_env(names),
    }
}
