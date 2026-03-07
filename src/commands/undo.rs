use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::undo;

use super::CmdResult;

#[derive(Args)]
pub struct UndoArgs {
    #[command(subcommand)]
    pub command: Option<UndoCommand>,

    /// Restore a specific snapshot by ID (default: latest)
    #[arg(long)]
    pub id: Option<String>,
}

#[derive(Subcommand)]
pub enum UndoCommand {
    /// List available undo snapshots
    List,
    /// Delete a snapshot without restoring
    Delete {
        /// Snapshot ID to delete
        id: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum UndoOutput {
    #[serde(rename = "undo.restore")]
    Restore(undo::RestoreResult),

    #[serde(rename = "undo.list")]
    List {
        snapshots: Vec<undo::SnapshotSummary>,
    },

    #[serde(rename = "undo.delete")]
    Delete { id: String, deleted: bool },
}

pub fn run(args: UndoArgs, _global: &super::GlobalArgs) -> CmdResult<UndoOutput> {
    match args.command {
        Some(UndoCommand::List) => {
            let snapshots = undo::list_snapshots()?;
            if snapshots.is_empty() {
                homeboy::log_status!("undo", "No snapshots available");
            } else {
                homeboy::log_status!("undo", "{} snapshot(s) available:", snapshots.len());
                for snap in &snapshots {
                    homeboy::log_status!(
                        "undo",
                        "  {} — {} ({} file(s), {})",
                        snap.id,
                        snap.label,
                        snap.file_count,
                        snap.age
                    );
                }
            }
            Ok((UndoOutput::List { snapshots }, 0))
        }
        Some(UndoCommand::Delete { id }) => {
            undo::delete_snapshot(&id)?;
            Ok((UndoOutput::Delete { id, deleted: true }, 0))
        }
        None => {
            // Default: restore latest (or specific --id)
            let result = undo::restore(args.id.as_deref())?;
            let has_errors = !result.errors.is_empty();
            let exit_code = if has_errors { 1 } else { 0 };
            Ok((UndoOutput::Restore(result), exit_code))
        }
    }
}
