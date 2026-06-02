use clap::Subcommand;
use homeboy::core::rig;

use super::output::{RigSourcesOutput, RigSourcesReport};
use super::RigCommandOutput;
use crate::commands::CmdResult;

#[derive(Subcommand)]
pub(super) enum RigSourcesCommand {
    /// List installed rig source packages
    List,
    /// Remove rigs installed from a source package
    Remove {
        /// Source URL/path, package path, or package ID from `rig sources list`
        source: String,
    },
    /// Refresh rigs installed from recorded source package paths
    Refresh {
        /// Source URL/path, package path, or package ID from `rig sources list`.
        /// Omit to refresh every installed git-backed source package.
        source: Option<String>,
    },
}

pub(super) fn run(command: RigSourcesCommand) -> CmdResult<RigCommandOutput> {
    match command {
        RigSourcesCommand::List => list(),
        RigSourcesCommand::Remove { source } => remove(&source),
        RigSourcesCommand::Refresh { source } => refresh(source.as_deref()),
    }
}

fn list() -> CmdResult<RigCommandOutput> {
    Ok((
        RigCommandOutput::Sources(RigSourcesOutput {
            command: "rig.sources.list",
            report: RigSourcesReport::List(rig::list_sources()?),
        }),
        0,
    ))
}

fn remove(source: &str) -> CmdResult<RigCommandOutput> {
    Ok((
        RigCommandOutput::Sources(RigSourcesOutput {
            command: "rig.sources.remove",
            report: RigSourcesReport::Remove(rig::remove_source(source)?),
        }),
        0,
    ))
}

fn refresh(source: Option<&str>) -> CmdResult<RigCommandOutput> {
    Ok((
        RigCommandOutput::Sources(RigSourcesOutput {
            command: "rig.sources.refresh",
            report: RigSourcesReport::Refresh(rig::update_source(source)?),
        }),
        0,
    ))
}
