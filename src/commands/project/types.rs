//! types — extracted from project.rs.

use clap::{Args, Subcommand, ValueEnum};
use homeboy::project::{self};
use std::path::Path;
use super::super::CmdResult;


#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Subcommand)]
pub(crate) enum ProjectCommand {
    /// List all configured projects
    List,
    /// Show project configuration
    Show {
        /// Project ID
        project_id: String,
    },
    /// Create a new project
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Project ID (CLI mode)
        id: Option<String>,
        /// Public site domain (CLI mode)
        domain: Option<String>,
        /// Optional server ID
        #[arg(long)]
        server_id: Option<String>,
        /// Optional remote base path
        #[arg(long)]
        base_path: Option<String>,
        /// Optional table prefix
        #[arg(long)]
        table_prefix: Option<String>,
    },
    /// Update project configuration fields
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: super::DynamicSetArgs,
    },
    /// Remove items from project configuration arrays
    Remove {
        /// Project ID (optional if provided in JSON body)
        project_id: Option<String>,
        /// JSON spec (positional, supports @file and - for stdin)
        spec: Option<String>,
        /// Explicit JSON spec (takes precedence over positional)
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },
    /// Rename a project (changes ID)
    Rename {
        /// Current project ID
        project_id: String,
        /// New project ID
        new_id: String,
    },
    /// Manage project components
    Components {
        #[command(subcommand)]
        command: ProjectComponentsCommand,
    },
    /// Manage pinned files and logs
    Pin {
        #[command(subcommand)]
        command: ProjectPinCommand,
    },
    /// Delete a project configuration
    Delete {
        /// Project ID
        project_id: String,
    },
    /// Initialize a project directory (migrate from flat file to directory layout)
    Init {
        /// Project ID
        project_id: String,
    },
    /// Show live server health and component versions for a project
    Status {
        /// Project ID
        project_id: String,

        /// Show only server health metrics, skip component versions
        #[arg(long)]
        health_only: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProjectComponentsCommand {
    /// List associated components
    List {
        /// Project ID
        project_id: String,
    },
    /// Replace project components with the provided list
    Set {
        /// Project ID
        project_id: String,
        /// JSON array of attachments: [{"id":"foo","local_path":"/repo"}]
        #[arg(long)]
        json: String,
    },
    /// Attach a repo path for a project component discovered via homeboy.json
    AttachPath {
        /// Project ID
        project_id: String,
        /// Local repo path containing homeboy.json
        local_path: String,
    },
    /// Remove one or more components
    Remove {
        /// Project ID
        project_id: String,
        /// Component IDs
        component_ids: Vec<String>,
    },
    /// Remove all components
    Clear {
        /// Project ID
        project_id: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProjectPinCommand {
    /// List pinned items
    List {
        /// Project ID
        project_id: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
    },
    /// Pin a file or log
    Add {
        /// Project ID
        project_id: String,
        /// Path to pin (relative to basePath or absolute)
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
        /// Optional display label
        #[arg(long)]
        label: Option<String>,
        /// Number of lines to tail (logs only)
        #[arg(long, default_value = "100")]
        tail: u32,
    },
    /// Unpin a file or log
    Remove {
        /// Project ID
        project_id: String,
        /// Path to unpin
        path: String,
        /// Item type: file or log
        #[arg(long, value_enum)]
        r#type: ProjectPinType,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum ProjectPinType {
    File,
    Log,
}

pub type ProjectOutput = homeboy::project::ProjectReportOutput;
