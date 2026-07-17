//! Runner workspace-root lookup hook.
//!
//! The daemon's runner file API (`/files/mkdir|upload|download`) resolves a
//! path against a runner's configured `workspace_root` when the request does
//! not carry one. That lookup loads the runner config, which is runner-owned,
//! so it is inverted behind this small provider: core's daemon asks for the
//! workspace root, the runner layer supplies it from the runner registry.
//!
//! With no provider registered the daemon simply has no configured root to fall
//! back on, so the caller reports the same "requires workspace_root" error it
//! would for a runner without one.

use std::sync::Mutex;

/// Looks up a runner's configured workspace root by id.
pub trait RunnerWorkspaceRootProvider: Send + Sync {
    /// The configured `workspace_root` for `runner_id`, if any.
    fn runner_workspace_root(&self, runner_id: &str) -> Option<String>;
}

struct NoopProvider;

impl RunnerWorkspaceRootProvider for NoopProvider {
    fn runner_workspace_root(&self, _runner_id: &str) -> Option<String> {
        None
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn RunnerWorkspaceRootProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn RunnerWorkspaceRootProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the runner workspace-root provider. Called once at startup by the
/// runner layer.
pub fn register_runner_workspace_root_provider(provider: Box<dyn RunnerWorkspaceRootProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("runner workspace root provider lock");
    *slot = Some(provider);
}

/// Resolve a runner's configured workspace root via the registered provider.
pub(crate) fn runner_workspace_root(runner_id: &str) -> Option<String> {
    let slot = provider_slot()
        .lock()
        .expect("runner workspace root provider lock");
    match slot.as_deref() {
        Some(provider) => provider.runner_workspace_root(runner_id),
        None => NoopProvider.runner_workspace_root(runner_id),
    }
}
