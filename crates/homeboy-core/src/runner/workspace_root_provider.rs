//! Runner-side implementation of core's `RunnerWorkspaceRootProvider` hook.
//!
//! Core's daemon file API asks for a runner's configured workspace root; this
//! adapter loads the runner config and returns its `workspace_root`.

use crate::daemon::runner_workspace_root::RunnerWorkspaceRootProvider;

/// The runner layer's `RunnerWorkspaceRootProvider`. Registered with core at startup.
pub struct RunnerWorkspaceRoot;

impl RunnerWorkspaceRootProvider for RunnerWorkspaceRoot {
    fn runner_workspace_root(&self, runner_id: &str) -> Option<String> {
        super::load(runner_id).ok()?.workspace_root
    }
}

/// Register the runner workspace-root provider with core. Called once at startup.
pub fn register() {
    crate::daemon::runner_workspace_root::register_runner_workspace_root_provider(Box::new(
        RunnerWorkspaceRoot,
    ));
}
