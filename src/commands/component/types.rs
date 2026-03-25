//! types — extracted from component.rs.

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use homeboy::project::{self, Project};
use homeboy::EntityCrudOutput;
use super::super::{CmdResult, DynamicSetArgs};
use std::path::Path;
use homeboy::component::{self, Component};


#[derive(Args)]
pub struct ComponentArgs {
    #[command(subcommand)]
    command: ComponentCommand,
}

#[derive(Subcommand)]
pub(crate) enum ComponentCommand {
    /// Initialize portable component config for a repo
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Absolute path to local source directory (writes homeboy.json there)
        #[arg(long)]
        local_path: Option<String>,
        /// Remote path relative to project basePath
        #[arg(long)]
        remote_path: Option<String>,
        /// Build artifact path relative to localPath
        #[arg(long)]
        build_artifact: Option<String>,
        /// Version targets in the form "file" or "file::pattern" (repeatable). For complex patterns, use --version-targets @file.json to avoid shell escaping
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,
        /// Version targets as JSON array (supports @file.json and - for stdin)
        #[arg(
            long = "version-targets",
            value_name = "JSON",
            conflicts_with = "version_targets"
        )]
        version_targets_json: Option<String>,
        /// Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,
        /// Extension(s) this component uses (e.g., "wordpress"). Repeatable.
        #[arg(long = "extension", value_name = "EXTENSION")]
        extensions: Vec<String>,
        /// Attach component to a project after creation
        #[arg(long)]
        project: Option<String>,
    },
    /// Display component configuration
    Show {
        /// Component ID
        id: String,
    },
    /// Update component configuration fields
    ///
    /// Supports dedicated flags for common fields (e.g., --local-path, --build-command)
    /// as well as --json for arbitrary updates. When combining --json with dynamic
    /// trailing flags, use '--' separator.
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,

        /// Absolute path to local source directory
        #[arg(long)]
        local_path: Option<String>,
        /// Remote path relative to project basePath
        #[arg(long)]
        remote_path: Option<String>,
        /// Build artifact path relative to localPath
        #[arg(long)]
        build_artifact: Option<String>,
        /// Extract command to run after upload (e.g., "unzip -o {artifact} && rm {artifact}")
        #[arg(long)]
        extract_command: Option<String>,
        /// Path to changelog file relative to localPath
        #[arg(long)]
        changelog_target: Option<String>,

        /// Version targets in the form "file" or "file::pattern" (repeatable).
        /// Same format as `component create --version-target`.
        #[arg(long = "version-target", value_name = "TARGET")]
        version_targets: Vec<String>,

        /// Extension(s) this component uses (e.g., "wordpress"). Repeatable.
        #[arg(long = "extension", value_name = "EXTENSION")]
        extensions: Vec<String>,
    },
    /// Delete a component configuration
    Delete {
        /// Component ID
        id: String,
    },
    /// Rename a component (changes ID directly)
    Rename {
        /// Current component ID
        id: String,
        /// New component ID (should match repository directory name)
        new_id: String,
    },
    /// List all available components
    List,
    /// List projects using this component
    Projects {
        /// Component ID
        id: String,
    },
    /// Show which components are shared across projects
    Shared {
        /// Specific component ID to check (optional, shows all if omitted)
        id: Option<String>,
    },
    /// Add a version target to a component
    AddVersionTarget {
        /// Component ID
        id: String,
        /// Target file path relative to component root
        file: String,
        /// Regex pattern with capture group for version
        pattern: String,
    },
}

/// Entity-specific fields for component commands.
#[derive(Debug, Default, Serialize)]
pub struct ComponentExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projects: Option<Vec<Project>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<std::collections::HashMap<String, Vec<String>>>,
}

pub type ComponentOutput = EntityCrudOutput<Value, ComponentExtra>;
