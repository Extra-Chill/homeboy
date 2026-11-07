use clap::{Args, Subcommand};
use homeboy::core::engine::temp::{self, ScratchAllocation};
use serde::Serialize;
use std::time::Duration;

use super::CmdResult;

#[derive(Args)]
#[command(
    after_help = "Put verification, replay, proof and recovery bytes here instead of an ad hoc path.\nAllocations live under the runtime-temp root, appear in `homeboy cleanup retained-storage`,\nand are removed by automatic retention once their TTL passes unpinned.\n\nOn a runner host: homeboy runner exec <runner> -- homeboy scratch allocate ..."
)]
pub struct ScratchArgs {
    #[command(subcommand)]
    command: ScratchCommand,
}

#[derive(Subcommand)]
enum ScratchCommand {
    /// Allocate an owned scratch directory and print its path
    Allocate {
        /// Issue, run or session the bytes belong to
        #[arg(long)]
        owner: String,
        /// What the bytes are for
        #[arg(long)]
        purpose: String,
        /// How long to keep it before automatic retention may remove it
        #[arg(long, default_value = "3d", value_parser = crate::commands::utils::watch::parse_duration_arg)]
        ttl: Duration,
    },
    /// List scratch allocations on this host with their sizes
    List,
    /// Keep an allocation past its TTL while a tracker or run references it
    Pin {
        id: String,
        #[arg(long)]
        reason: String,
    },
    /// Return a pinned allocation to its TTL
    Unpin { id: String },
    /// Delete an allocation now
    Release { id: String },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ScratchOutput {
    One {
        command: &'static str,
        allocation: ScratchAllocation,
    },
    List {
        command: &'static str,
        allocations: Vec<ScratchAllocation>,
        total_bytes: u64,
    },
}

pub fn run(args: ScratchArgs) -> CmdResult<ScratchOutput> {
    let one = |command, allocation| {
        Ok((
            ScratchOutput::One {
                command,
                allocation,
            },
            0,
        ))
    };
    match args.command {
        ScratchCommand::Allocate {
            owner,
            purpose,
            ttl,
        } => one(
            "scratch.allocate",
            temp::allocate_scratch(&owner, &purpose, ttl)?,
        ),
        ScratchCommand::List => {
            let allocations = temp::list_scratch(true)?;
            let total_bytes = allocations.iter().filter_map(|row| row.size_bytes).sum();
            Ok((
                ScratchOutput::List {
                    command: "scratch.list",
                    allocations,
                    total_bytes,
                },
                0,
            ))
        }
        ScratchCommand::Pin { id, reason } => one("scratch.pin", temp::pin_scratch(&id, &reason)?),
        ScratchCommand::Unpin { id } => one("scratch.unpin", temp::unpin_scratch(&id)?),
        ScratchCommand::Release { id } => one("scratch.release", temp::release_scratch(&id)?),
    }
}
