use clap::Args;
use serde::Serialize;

use homeboy::core::observation::{FindingListFilter, ObservationStore, RecordedHomeboyFinding};
use homeboy::core::Error;

use crate::commands::{
    runs::{
        latest::latest_run_context, latest::RunsLatestFindingOutput, latest::RunsLatestRunArgs,
        run_summary, RunsOutput,
    },
    CmdResult,
};

#[derive(Args, Clone, Default)]
pub struct RunsFindingsArgs {
    /// Observation run ID
    pub run_id: String,
    /// Finding tool, for example lint
    #[arg(long)]
    pub tool: Option<String>,
    /// Finding file path
    #[arg(long)]
    pub file: Option<String>,
    /// Finding fingerprint
    #[arg(long)]
    pub fingerprint: Option<String>,
    /// Maximum findings to return
    #[arg(long, default_value_t = 100)]
    pub limit: i64,
}

#[derive(Args, Clone, Default)]
pub struct RunsLatestFindingArgs {
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
    /// Finding tool, for example lint
    #[arg(long)]
    pub tool: Option<String>,
    /// Finding file path
    #[arg(long)]
    pub file: Option<String>,
}

#[derive(Serialize)]
pub struct RunsFindingsOutput {
    pub command: &'static str,
    pub run_id: String,
    pub findings: Vec<RecordedHomeboyFinding>,
}

#[derive(Serialize)]
pub struct RunsFindingOutput {
    pub command: &'static str,
    pub finding: RecordedHomeboyFinding,
}

pub fn findings(args: RunsFindingsArgs) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    require_run(&store, &args.run_id)?;
    let findings = store
        .list_findings(FindingListFilter {
            run_id: Some(args.run_id.clone()),
            tool: args.tool,
            file: args.file,
            fingerprint: args.fingerprint,
            limit: Some(args.limit),
        })?
        .into_iter()
        .map(RecordedHomeboyFinding::from)
        .collect();

    Ok((
        RunsOutput::Findings(RunsFindingsOutput {
            command: "runs.findings",
            run_id: args.run_id,
            findings,
        }),
        0,
    ))
}

pub fn finding(finding_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let finding = store.get_finding(finding_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "finding_id",
            format!("finding not found: {finding_id}"),
            Some(finding_id.to_string()),
            None,
        )
    })?;

    Ok((
        RunsOutput::Finding(RunsFindingOutput {
            command: "runs.finding",
            finding: RecordedHomeboyFinding::from(finding),
        }),
        0,
    ))
}

pub fn latest_finding(args: RunsLatestFindingArgs) -> CmdResult<RunsOutput> {
    let (store, run) = latest_run_context(RunsLatestRunArgs {
        kind: args.kind,
        component_id: args.component_id,
        rig: args.rig,
        status: args.status,
    })?;
    let finding = store
        .latest_finding(FindingListFilter {
            run_id: Some(run.id.clone()),
            tool: args.tool,
            file: args.file,
            fingerprint: None,
            limit: Some(1),
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "filter",
                format!(
                    "no finding matched the provided filters in latest run {}",
                    run.id
                ),
                Some(run.id.clone()),
                None,
            )
        })?;

    Ok((
        RunsOutput::LatestFinding(RunsLatestFindingOutput {
            command: "runs.latest-finding",
            run: run_summary(run),
            finding: RecordedHomeboyFinding::from(finding),
        }),
        0,
    ))
}

fn require_run(store: &ObservationStore, run_id: &str) -> homeboy::core::Result<()> {
    if store.get_run(run_id)?.is_some() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "run_id",
        format!("run record not found: {run_id}"),
        Some(run_id.to_string()),
        None,
    ))
}
