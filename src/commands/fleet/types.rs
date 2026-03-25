//! types — extracted from fleet.rs.

use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::fleet::{self, Fleet, FleetComponentDrift, FleetStatusResult};
use homeboy::project::Project;
use homeboy::EntityCrudOutput;
use super::super::{CmdResult, DynamicSetArgs};


#[derive(Args)]
pub struct FleetArgs {
    #[command(subcommand)]
    command: FleetCommand,
}

#[derive(Subcommand)]
pub(crate) enum FleetCommand {
    /// Create a new fleet
    Create {
        /// Fleet ID
        id: String,

        /// Project IDs to include (comma-separated or repeated)
        #[arg(long, short = 'p', value_delimiter = ',')]
        projects: Option<Vec<String>>,

        /// Description of the fleet
        #[arg(long, short = 'd')]
        description: Option<String>,
    },
    /// Display fleet configuration
    Show {
        /// Fleet ID
        id: String,
    },
    /// Update fleet configuration
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
    },
    /// Delete a fleet
    Delete {
        /// Fleet ID
        id: String,
    },
    /// List all fleets
    List,
    /// Add a project to a fleet
    Add {
        /// Fleet ID
        id: String,

        /// Project ID to add
        #[arg(long, short = 'p')]
        project: String,
    },
    /// Remove a project from a fleet
    Remove {
        /// Fleet ID
        id: String,

        /// Project ID to remove
        #[arg(long, short = 'p')]
        project: String,
    },
    /// Show projects in a fleet
    Projects {
        /// Fleet ID
        id: String,
    },
    /// Show component usage across a fleet
    Components {
        /// Fleet ID
        id: String,
    },
    /// Show live component versions and server health across a fleet (via SSH)
    Status {
        /// Fleet ID
        id: String,

        /// Use locally cached versions instead of live SSH check
        #[arg(long)]
        cached: bool,

        /// Show only server health metrics, skip component versions
        #[arg(long)]
        health_only: bool,
    },
    /// Check component drift across a fleet (compares local vs remote)
    Check {
        /// Fleet ID
        id: String,

        /// Only show components that need updates
        #[arg(long)]
        outdated: bool,
    },
    /// Run a command across all projects in a fleet via SSH
    Exec {
        /// Fleet ID
        id: String,

        /// Command to execute on each project's server
        #[arg(num_args = 0.., trailing_var_arg = true)]
        command: Vec<String>,

        /// Show what would execute without running anything
        #[arg(long)]
        check: bool,

        /// Override the SSH user for this execution (instead of each server's configured user)
        #[arg(long)]
        user: Option<String>,

        /// Reserved for future parallel mode. Currently all execution is serial.
        #[arg(long, hide = true)]
        serial: bool,
    },
}

/// Entity-specific fields for fleet commands.
#[derive(Debug, Default, Serialize)]
pub struct FleetExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<FleetStatusResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<Vec<homeboy::fleet::FleetProjectCheck>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<homeboy::fleet::FleetCheckSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec: Option<Vec<homeboy::fleet::FleetExecProjectResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exec_summary: Option<homeboy::fleet::FleetExecSummary>,
}

pub type FleetOutput = EntityCrudOutput<Fleet, FleetExtra>;
