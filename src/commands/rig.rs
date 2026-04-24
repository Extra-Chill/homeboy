use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::rig::{self, RigSpec};

use super::CmdResult;

#[derive(Args)]
pub struct RigArgs {
    #[command(subcommand)]
    command: RigCommand,
}

#[derive(Subcommand)]
enum RigCommand {
    /// List all declared rigs
    List,
    /// Show a rig spec
    Show {
        /// Rig ID
        rig_id: String,
    },
    /// Materialize a rig: run its `up` pipeline
    Up {
        /// Rig ID
        rig_id: String,
    },
    /// Run a rig's `check` pipeline and report health
    Check {
        /// Rig ID
        rig_id: String,
    },
    /// Tear down a rig: stop services and run its `down` pipeline
    Down {
        /// Rig ID
        rig_id: String,
    },
    /// Show current state of a rig: running services, last up/check
    Status {
        /// Rig ID
        rig_id: String,
    },
}

/// Output envelope for rig commands. `untagged` so each variant serializes
/// to its own shape — consumers discriminate on `command`.
#[derive(Serialize)]
#[serde(untagged)]
pub enum RigCommandOutput {
    List(RigListOutput),
    Show(RigShowOutput),
    Up(RigUpOutput),
    Check(RigCheckOutput),
    Down(RigDownOutput),
    Status(RigStatusOutput),
}

#[derive(Serialize)]
pub struct RigListOutput {
    pub command: &'static str,
    pub rigs: Vec<RigSummary>,
}

#[derive(Serialize)]
pub struct RigSummary {
    pub id: String,
    pub description: String,
    pub component_count: usize,
    pub service_count: usize,
    pub pipelines: Vec<String>,
}

#[derive(Serialize)]
pub struct RigShowOutput {
    pub command: &'static str,
    pub rig: RigSpec,
}

#[derive(Serialize)]
pub struct RigUpOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::UpReport,
}

#[derive(Serialize)]
pub struct RigCheckOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::CheckReport,
}

#[derive(Serialize)]
pub struct RigDownOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::DownReport,
}

#[derive(Serialize)]
pub struct RigStatusOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: rig::RigStatusReport,
}

pub fn run(args: RigArgs, _global: &super::GlobalArgs) -> CmdResult<RigCommandOutput> {
    match args.command {
        RigCommand::List => list(),
        RigCommand::Show { rig_id } => show(&rig_id),
        RigCommand::Up { rig_id } => up(&rig_id),
        RigCommand::Check { rig_id } => check(&rig_id),
        RigCommand::Down { rig_id } => down(&rig_id),
        RigCommand::Status { rig_id } => status(&rig_id),
    }
}

fn list() -> CmdResult<RigCommandOutput> {
    let rigs = rig::list()?;
    let summaries = rigs
        .into_iter()
        .map(|r| {
            let mut pipelines: Vec<String> = r.pipeline.keys().cloned().collect();
            pipelines.sort();
            RigSummary {
                id: r.id,
                description: r.description,
                component_count: r.components.len(),
                service_count: r.services.len(),
                pipelines,
            }
        })
        .collect();

    Ok((
        RigCommandOutput::List(RigListOutput {
            command: "rig.list",
            rigs: summaries,
        }),
        0,
    ))
}

fn show(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    Ok((
        RigCommandOutput::Show(RigShowOutput {
            command: "rig.show",
            rig,
        }),
        0,
    ))
}

fn up(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_up(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Up(RigUpOutput {
            command: "rig.up",
            report,
        }),
        exit_code,
    ))
}

fn check(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_check(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Check(RigCheckOutput {
            command: "rig.check",
            report,
        }),
        exit_code,
    ))
}

fn down(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_down(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Down(RigDownOutput {
            command: "rig.down",
            report,
        }),
        exit_code,
    ))
}

fn status(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_status(&rig)?;
    Ok((
        RigCommandOutput::Status(RigStatusOutput {
            command: "rig.status",
            report,
        }),
        0,
    ))
}
