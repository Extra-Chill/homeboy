//! Run-scoped RAII ownership of a materialized remote runner workspace (#6678).
//!
//! Offloaded Lab runs stage a per-run checkout under
//! `<workspace_root>/_lab_workspaces/<snapshot>` plus a sibling
//! `<checkout>-homeboy-artifacts` directory. Historically nothing reaped those
//! on the success path — the only teardown was the operator-driven CLI prune
//! (`runner workspace prune`), so every offloaded run left scraps on the lab.
//!
//! [`MaterializedWorkspace`] is the run-owned handle that closes that gap: it
//! carries a [`WorkspaceCleanupPolicy`] and reaps the remote workspace (and its
//! artifact sibling) on drop. The default policy
//! [`WorkspaceCleanupPolicy::PreserveOnFailure`] reaps only when the run is
//! marked successful, preserving the remote tree on failure so post-mortem
//! evidence survives. Reap is best-effort: a teardown error is logged, never
//! propagated, and the controller-side `runner workspace prune` remains the
//! backstop.

use super::sync::reap_run_workspace;

/// Teardown policy for a run-owned materialized workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceCleanupPolicy {
    /// Reap the workspace when the run succeeds; preserve it on failure so
    /// post-mortem evidence survives on the lab. Default — chosen to avoid
    /// behavior shock (failed runs keep their evidence as before) while still
    /// reclaiming the common success path that previously leaked.
    PreserveOnFailure,
    /// Never auto-reap; rely entirely on the operator-driven
    /// `runner workspace prune` CLI (the legacy behavior).
    PreserveAlways,
}

impl Default for WorkspaceCleanupPolicy {
    fn default() -> Self {
        Self::PreserveOnFailure
    }
}

/// Run-owned handle to a materialized remote runner workspace. Reaps the remote
/// `_lab_workspaces/<snapshot>` checkout (and its sibling Homeboy artifact
/// directory) on drop, according to its [`WorkspaceCleanupPolicy`].
///
/// Create one right after the run materializes its workspace and let it own the
/// remainder of the offload scope. Mark the run outcome with [`set_success`] on
/// the success path; call [`preserve`] on any path that hands the checkout off
/// to a still-running remote job (detach, in-flight daemon disconnect) so the
/// live job keeps its workspace.
///
/// [`set_success`]: MaterializedWorkspace::set_success
/// [`preserve`]: MaterializedWorkspace::preserve
pub(crate) struct MaterializedWorkspace {
    runner_id: String,
    remote_path: String,
    artifact_dir: Option<String>,
    policy: WorkspaceCleanupPolicy,
    succeeded: bool,
    relinquished: bool,
}

impl MaterializedWorkspace {
    pub(crate) fn new(
        runner_id: String,
        remote_path: String,
        artifact_dir: Option<String>,
        policy: WorkspaceCleanupPolicy,
    ) -> Self {
        Self {
            runner_id,
            remote_path,
            artifact_dir,
            policy,
            succeeded: false,
            relinquished: false,
        }
    }

    /// Record the run outcome. Under the default `PreserveOnFailure` policy the
    /// workspace is reaped on drop only when `succeeded` is `true`.
    pub(crate) fn set_success(&mut self, succeeded: bool) {
        self.succeeded = succeeded;
    }

    /// Relinquish run-scoped ownership without reaping — e.g. the remote run
    /// continues detached, or its daemon job is still in flight, and still owns
    /// the checkout. After this the handle never reaps on drop.
    pub(crate) fn preserve(&mut self) {
        self.relinquished = true;
    }

    fn should_reap(&self) -> bool {
        // Preserve evidence if we are unwinding from a panic, and honor an
        // explicit relinquish handing the workspace to a live remote job.
        if self.relinquished || std::thread::panicking() {
            return false;
        }
        match self.policy {
            WorkspaceCleanupPolicy::PreserveAlways => false,
            WorkspaceCleanupPolicy::PreserveOnFailure => self.succeeded,
        }
    }
}

impl Drop for MaterializedWorkspace {
    fn drop(&mut self) {
        if !self.should_reap() {
            return;
        }
        match reap_run_workspace(
            &self.runner_id,
            &self.remote_path,
            self.artifact_dir.as_deref(),
        ) {
            Ok(()) => eprintln!(
                "Lab offload: reaped run-scoped workspace `{}` on runner `{}` (delete-on-success cleanup policy).",
                self.remote_path, self.runner_id
            ),
            Err(err) => eprintln!(
                "Lab offload: warning: could not reap run-scoped workspace `{}` on runner `{}`: {}. Reclaim leftover lab workspaces with `homeboy runner workspace prune {}`.",
                self.remote_path, self.runner_id, err.message, self.runner_id
            ),
        }
    }
}
