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
//! [`WorkspaceCleanupPolicy::DeleteAlways`] reaps every authoritatively observed
//! terminal outcome.
//! The explicit `PreserveOnFailure` debugging policy retains failed workspaces
//! through the registered TTL lifecycle. Reap is best-effort: a teardown error
//! is logged, never propagated, and the controller-side `runner workspace prune`
//! remains the backstop.

use homeboy_core::resource_lifecycle_index::ResourceLifecycleResourceStatus;

use super::sync::{reap_run_workspace, record_workspace_terminal_evidence};
use super::types::{RunnerWorkspaceReconciliation, RunnerWorkspaceTerminalEvidence};

/// Teardown policy for a run-owned materialized workspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum WorkspaceCleanupPolicy {
    /// Reap every terminal outcome. Use this for job-private runtime state that
    /// must never survive a completed or cancelled offload.
    #[default]
    DeleteAlways,
    /// Reap the workspace when the run succeeds; preserve it on failure so
    /// post-mortem evidence survives on the lab through its registered TTL.
    PreserveOnFailure,
}

impl WorkspaceCleanupPolicy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DeleteAlways => "delete-on-terminal",
            Self::PreserveOnFailure => "preserve-on-failure",
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

/// Run-owned handle to a materialized remote runner workspace. Reaps the remote
/// `_lab_workspaces/<snapshot>` checkout (and its sibling Homeboy artifact
/// directory) on drop, according to its [`WorkspaceCleanupPolicy`].
///
/// Create one right after the run materializes its workspace and let it own the
/// remainder of the offload scope. Mark the run outcome with
/// [`set_terminal_outcome`] on
/// the success path; call [`preserve`] on any path that hands the checkout off
/// to a still-running remote job (detach, in-flight daemon disconnect) so the
/// live job keeps its workspace.
///
/// [`set_terminal_outcome`]: MaterializedWorkspace::set_terminal_outcome
/// [`preserve`]: MaterializedWorkspace::preserve
pub(crate) struct MaterializedWorkspace {
    runner_id: String,
    remote_path: String,
    artifact_dir: Option<String>,
    policy: WorkspaceCleanupPolicy,
    outcome: WorkspaceTerminalOutcome,
    authoritative_terminal_outcome: bool,
    reconciliation: Option<RunnerWorkspaceReconciliation>,
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
            // Workspace creation and setup errors happen before daemon
            // admission, so they retain the existing immediate cleanup policy.
            outcome: WorkspaceTerminalOutcome::Failure,
            authoritative_terminal_outcome: true,
            reconciliation: None,
            relinquished: false,
        }
    }

    /// Set a known terminal outcome when the caller can distinguish cancellation
    /// from failure before this run-owned handle is dropped.
    pub(crate) fn set_terminal_outcome(&mut self, outcome: WorkspaceTerminalOutcome) {
        self.outcome = outcome;
        self.authoritative_terminal_outcome = true;
    }

    /// Relinquish run-scoped ownership without reaping — e.g. the remote run
    /// continues detached, or its daemon job is still in flight, and still owns
    /// the checkout. After this the handle never reaps on drop.
    pub(crate) fn preserve(&mut self) {
        self.relinquished = true;
        self.outcome = WorkspaceTerminalOutcome::UncertainHandoff;
    }

    /// Retain a workspace after an accepted daemon job becomes unobservable.
    /// The exact owner makes reconnect/reconciliation actionable while the
    /// standard TTL lifecycle bounds retention if recovery never completes.
    pub(crate) fn retain_for_reconciliation(
        &mut self,
        job_id: &str,
        daemon_generation: Option<&str>,
    ) {
        self.outcome = WorkspaceTerminalOutcome::UncertainHandoff;
        self.authoritative_terminal_outcome = false;
        self.reconciliation = Some(RunnerWorkspaceReconciliation {
            job_id: job_id.to_string(),
            daemon_generation: daemon_generation.map(ToString::to_string),
            reconnect_command: format!(
                "homeboy runner status {} --full",
                homeboy_core::engine::shell::quote_arg(&self.runner_id),
            ),
            reconcile_command: format!(
                "homeboy runner job reconcile {}",
                homeboy_core::engine::shell::quote_arg(&self.runner_id),
            ),
            job_logs_command: format!(
                "homeboy runner job logs {} {} --follow",
                homeboy_core::engine::shell::quote_arg(&self.runner_id),
                homeboy_core::engine::shell::quote_arg(job_id),
            ),
        });
    }

    fn should_reap(&self) -> bool {
        // Preserve evidence if we are unwinding from a panic, and honor an
        // explicit relinquish handing the workspace to a live remote job.
        if self.relinquished || !self.authoritative_terminal_outcome || std::thread::panicking() {
            return false;
        }
        match self.policy {
            WorkspaceCleanupPolicy::DeleteAlways => true,
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
            cleanup_trigger: (!retained && self.authoritative_terminal_outcome)
                .then(|| "authoritative_terminal_result_and_handoff".to_string()),
            retained_location: retained.then(|| self.remote_path.clone()),
            reclaim_command: (retained && !self.relinquished).then(|| self.reclaim_command()),
            reconciliation_needed: !self.authoritative_terminal_outcome && !self.relinquished,
            reconciliation_ttl: (!self.authoritative_terminal_outcome && !self.relinquished)
                .then(runner_workspace_ttl),
            reconciliation: self.reconciliation.clone(),
        };
        if let Err(error) = record_workspace_terminal_evidence(
            &self.runner_id,
            &self.remote_path,
            evidence,
            status,
            self.relinquished,
        ) {
            eprintln!(
                "Lab offload: warning: could not persist terminal workspace evidence for `{}` on runner `{}`: {}",
                self.remote_path, self.runner_id, error.message
            );
        }
    }
}

fn runner_workspace_ttl() -> String {
    homeboy_core::defaults::load_config()
        .lab
        .runner_workspace_ttl
        .filter(|ttl| !ttl.trim().is_empty())
        .unwrap_or_else(|| "P7D".to_string())
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
