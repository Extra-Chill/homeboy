use clap::Args;
use serde::Serialize;

use homeboy::core::observation::{
    ObservationStore, RecordedHomeboyFinding, RunListFilter, RunRecord,
};
use homeboy::core::Error;

use crate::commands::{
    runs::{run_summary, RunSummary, RunsOutput},
    CmdResult,
};

#[derive(Args, Clone, Default)]
pub struct RunsLatestRunArgs {
    /// Run kind: bench, rig, trace, etc.
    #[arg(long)]
    pub kind: Option<String>,
    /// Component ID
    #[arg(long = "component")]
    pub component_id: Option<String>,
    /// Rig ID
    #[arg(long)]
    pub rig: Option<String>,
    /// Run status
    #[arg(long)]
    pub status: Option<String>,
}

#[derive(Serialize)]
pub struct RunsLatestRunOutput {
    pub command: &'static str,
    pub run: RunSummary,
}

#[derive(Serialize)]
pub struct RunsLatestFindingOutput {
    pub command: &'static str,
    pub run: RunSummary,
    pub finding: RecordedHomeboyFinding,
}

pub fn latest_run(args: RunsLatestRunArgs) -> CmdResult<RunsOutput> {
    let (_, run) = latest_run_context(args)?;

    Ok((
        RunsOutput::LatestRun(RunsLatestRunOutput {
            command: "runs.latest-run",
            run: run_summary(run),
        }),
        0,
    ))
}

pub(crate) fn latest_run_context(
    args: RunsLatestRunArgs,
) -> homeboy::core::Result<(ObservationStore, RunRecord)> {
    let store = ObservationStore::open_initialized()?;
    let run = require_latest_run(&store, run_filter_from_latest_args(args))?;
    Ok((store, run))
}

pub(crate) fn require_latest_run(
    store: &ObservationStore,
    filter: RunListFilter,
) -> homeboy::core::Result<RunRecord> {
    store.latest_run(filter)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "filter",
            "no observation run matched the provided filters",
            None,
            None,
        )
    })
}

pub(crate) fn run_filter_from_latest_args(args: RunsLatestRunArgs) -> RunListFilter {
    RunListFilter {
        kind: args.kind,
        component_id: args.component_id,
        status: args.status,
        rig_id: args.rig,
        limit: Some(1),
    }
}
