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

use homeboy_core::resource_lifecycle_index::ResourceLifecycleResourceStatus;

use super::sync::{reap_run_workspace, record_workspace_terminal_evidence};
use super::types::RunnerWorkspaceTerminalEvidence;

/// Teardown policy for a run-owned materialized workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceCleanupPolicy {
    /// Reap every terminal outcome. Use this for job-private runtime state that
    /// must never survive a completed or cancelled offload.
    DeleteAlways,
    /// Reap the workspace when the run succeeds; preserve it on failure so
    /// post-mortem evidence survives on the lab. Default — chosen to avoid
    /// behavior shock (failed runs keep their evidence as before) while still
    /// reclaiming the common success path that previously leaked.
    PreserveOnFailure,
    /// Never auto-reap; rely entirely on the operator-driven
    /// `runner workspace prune` CLI (the legacy behavior).
    PreserveAlways,
}

impl WorkspaceCleanupPolicy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DeleteAlways => "delete-on-terminal",
            Self::PreserveOnFailure => "preserve-on-failure",
            Self::PreserveAlways => "preserve-always",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceTerminalOutcome {
    Success,
    Failure,
    Cancelled,
    Panic,
    UncertainHandoff,
}

impl WorkspaceTerminalOutcome {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Panic => "panic",
            Self::UncertainHandoff => "uncertain_handoff",
        }
    }
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
    outcome: WorkspaceTerminalOutcome,
    relinquished: bool,
}

impl MaterializedWorkspace {
    pub fn new(
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
            outcome: WorkspaceTerminalOutcome::Failure,
            relinquished: false,
        }
    }

    /// Set a known terminal outcome when the caller can distinguish cancellation
    /// from failure before this run-owned handle is dropped.
    pub(crate) fn set_terminal_outcome(&mut self, outcome: WorkspaceTerminalOutcome) {
        self.outcome = outcome;
    }

    /// Relinquish run-scoped ownership without reaping — e.g. the remote run
    /// continues detached, or its daemon job is still in flight, and still owns
    /// the checkout. After this the handle never reaps on drop.
    pub(crate) fn preserve(&mut self) {
        self.relinquished = true;
        self.outcome = WorkspaceTerminalOutcome::UncertainHandoff;
    }

    fn should_reap(&self) -> bool {
        // Preserve evidence if we are unwinding from a panic, and honor an
        // explicit relinquish handing the workspace to a live remote job.
        if self.relinquished || std::thread::panicking() {
            return false;
        }
        match self.policy {
            WorkspaceCleanupPolicy::DeleteAlways => true,
            WorkspaceCleanupPolicy::PreserveAlways => false,
            WorkspaceCleanupPolicy::PreserveOnFailure => {
                self.outcome == WorkspaceTerminalOutcome::Success
            }
        }
    }

    fn terminal_outcome(&self) -> WorkspaceTerminalOutcome {
        if std::thread::panicking() {
            WorkspaceTerminalOutcome::Panic
        } else {
            self.outcome
        }
    }

    fn reclaim_command(&self) -> String {
        format!(
            "homeboy runner workspace prune {} --apply --min-age-hours 0",
            homeboy_core::engine::shell::quote_arg(&self.runner_id),
        )
    }

    fn record_terminal_evidence(&self, retained: bool, status: ResourceLifecycleResourceStatus) {
        let evidence = RunnerWorkspaceTerminalEvidence {
            schema: "homeboy/runner-workspace-terminal-evidence/v1".to_string(),
            policy: self.policy.label().to_string(),
            final_outcome: self.terminal_outcome().label().to_string(),
            lifecycle_owner: if self.relinquished {
                "runner.job".to_string()
            } else {
                "runner.workspace".to_string()
            },
            retained_location: retained.then(|| self.remote_path.clone()),
            reclaim_command: retained.then(|| self.reclaim_command()),
        };
        if let Err(error) =
            record_workspace_terminal_evidence(&self.runner_id, &self.remote_path, evidence, status)
        {
            eprintln!(
                "Lab offload: warning: could not persist terminal workspace evidence for `{}` on runner `{}`: {}",
                self.remote_path, self.runner_id, error.message
            );
        }
    }
}

impl Drop for MaterializedWorkspace {
    fn drop(&mut self) {
        if !self.should_reap() {
            self.record_terminal_evidence(true, ResourceLifecycleResourceStatus::Retained);
            if !self.relinquished && !std::thread::panicking() {
                eprintln!(
                    "Lab offload: retained run-scoped workspace `{}` on runner `{}` (policy={}, outcome={}, lifecycle_owner=runner.workspace, reclaim=`homeboy runner workspace prune {} --apply --min-age-hours 0`).",
                    self.remote_path,
                    self.runner_id,
                    self.policy.label(),
                    self.terminal_outcome().label(),
                    homeboy_core::engine::shell::quote_arg(&self.runner_id),
                );
            }
            return;
        }
        // Persist cleanup-pending truth before deletion. The metadata is then
        // removed atomically with the workspace, so it cannot be mistaken for
        // retained evidence after a successful reap.
        self.record_terminal_evidence(false, ResourceLifecycleResourceStatus::CleanupPending);
        match reap_run_workspace(
            &self.runner_id,
            &self.remote_path,
            self.artifact_dir.as_deref(),
        ) {
            Ok(()) => eprintln!(
                "Lab offload: reaped run-scoped workspace `{}` on runner `{}` (policy={}, outcome={}, lifecycle_owner=runner.workspace).",
                self.remote_path,
                self.runner_id,
                self.policy.label(),
                self.terminal_outcome().label()
            ),
            Err(err) => {
                self.record_terminal_evidence(true, ResourceLifecycleResourceStatus::Failed);
                eprintln!(
                    "Lab offload: warning: could not reap run-scoped workspace `{}` on runner `{}`: {}. Reclaim leftover lab workspaces with `{}`.",
                    self.remote_path, self.runner_id, err.message, self.reclaim_command()
                );
            }
        }
    }
}
