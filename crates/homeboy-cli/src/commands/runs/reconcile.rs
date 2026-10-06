//! `homeboy runs reconcile` adapter.
//!
//! The reconciliation rule lives in
//! `homeboy::core::observation::runs_service` (`reconcile.rs`); this module
//! only owns the clap arguments and the `RunsOutput` rendering.

use clap::Args;
use serde::Serialize;

use homeboy::core::observation::runs_service::{
    reconcile_orphaned_running_runs, running_runs, ReconciledRunSummary,
};
use homeboy::core::observation::ObservationStore;
use homeboy::core::process::pid_is_running;

use crate::commands::runs::RunsOutput;
use crate::commands::CmdResult;

#[derive(Args, Clone, Default)]
pub struct RunsReconcileArgs {
    /// Preview orphaned running observation records without mutation. Omit this
    /// flag to mark only the reported orphaned records stale.
    #[arg(long)]
    pub dry_run: bool,
    /// Maximum running records to inspect
    #[arg(long, default_value_t = 1000)]
    pub limit: i64,
}

#[derive(Serialize)]
pub struct RunsReconcileOutput {
    pub command: &'static str,
    /// State plane that owns this reconciliation.
    pub owner: &'static str,
    /// Bounded records considered by this invocation.
    pub scope: String,
    /// State guaranteed when this command completes successfully.
    pub postcondition: &'static str,
    pub dry_run: bool,
    pub inspected: usize,
    pub reconciled: Vec<ReconciledRunSummary>,
}

pub(crate) fn reconcile_runs(
    store: &ObservationStore,
    args: RunsReconcileArgs,
) -> CmdResult<RunsOutput> {
    let running = running_runs(&store, args.limit)?;
    let inspected = running.len();
    let reconciled =
        reconcile_orphaned_running_runs(&store, running, args.dry_run, pid_is_running)?;

    Ok((
        RunsOutput::Reconcile(RunsReconcileOutput {
            command: "runs.reconcile",
            owner: "observation_runs",
            scope: format!(
                "up to {} running observation records",
                args.limit.clamp(1, 1000)
            ),
            postcondition: if args.dry_run {
                "reports orphaned observation records without persisted mutation"
            } else {
                "every reported orphaned observation record is stale"
            },
            dry_run: args.dry_run,
            inspected,
            reconciled,
        }),
        0,
    ))
}
