//! types — extracted from extension.rs.

use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::extension::{
    self, extension_ready_status, is_extension_linked, load_extension, run_setup, ExtensionSummary,
    UpdateEntry,
};
use homeboy::project::{self, Project};
use crate::commands::CmdResult;


#[derive(Args)]
pub struct ExtensionArgs {
    #[command(subcommand)]
    command: ExtensionCommand,
}

#[derive(Subcommand)]
pub(crate) enum ExtensionCommand {
    /// Show available extensions with compatibility status
    List {
        /// Project ID to filter compatible extensions
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Show detailed information about a extension
    Show {
        /// Extension ID
        extension_id: String,
    },
    /// Execute a extension
    Run {
        /// Extension ID
        extension_id: String,
        /// Project ID (defaults to active project)
        #[arg(short, long)]
        project: Option<String>,
        /// Component ID (required when ambiguous)
        #[arg(short, long)]
        component: Option<String>,
        /// Input values as key=value pairs
        #[arg(short, long, value_parser = super::parse_key_val)]
        input: Vec<(String, String)>,
        /// Run only specific steps (comma-separated, e.g. --step phpunit,phpcs)
        #[arg(long)]
        step: Option<String>,
        /// Skip specific steps (comma-separated, e.g. --skip phpstan,lint)
        #[arg(long)]
        skip: Option<String>,
        /// Arguments to pass to the extension (for CLI extensions)
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Stream output directly to terminal (default: auto-detect based on TTY)
        #[arg(long)]
        stream: bool,
        /// Disable streaming and capture output (default: auto-detect based on TTY)
        #[arg(long)]
        no_stream: bool,
    },
    /// Run the extension's setup command (if defined)
    Setup {
        /// Extension ID
        extension_id: String,
    },
    /// Install a extension from a git URL or local path
    Install {
        /// Git URL or local path to extension directory
        source: String,
        /// Override extension id
        #[arg(long)]
        id: Option<String>,
    },
    /// Update an installed extension (git pull)
    Update {
        /// Extension ID (omit with --all to update everything)
        extension_id: Option<String>,
        /// Update all installed extensions
        #[arg(long)]
        all: bool,
        /// Force update even with uncommitted changes
        #[arg(long)]
        force: bool,
    },
    /// Uninstall a extension
    Uninstall {
        /// Extension ID
        extension_id: String,
    },
    /// Execute a extension action (API call or builtin)
    Action {
        /// Extension ID
        extension_id: String,
        /// Action ID
        action_id: String,
        /// Project ID (required for API actions)
        #[arg(short, long)]
        project: Option<String>,
        /// JSON array of selected data rows
        #[arg(long)]
        data: Option<String>,
    },
    /// Run a tool from a extension's vendor directory
    Exec {
        /// Extension ID
        extension_id: String,
        /// Component ID (sets working directory to component path)
        #[arg(short, long)]
        component: Option<String>,
        /// Command and arguments to run
        #[arg(trailing_var_arg = true, required = true)]
        args: Vec<String>,
    },
    /// Update extension manifest fields
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Extension ID (optional if provided in JSON body)
        extension_id: Option<String>,
        /// JSON object to merge into manifest (supports @file and - for stdin)
        #[arg(long, value_name = "JSON")]
        json: String,
        /// Replace these fields instead of merging arrays
        #[arg(long, value_name = "FIELD")]
        replace: Vec<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "command")]
#[allow(clippy::large_enum_variant)]
pub enum ExtensionOutput {
    #[serde(rename = "extension.list")]
    List {
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        extensions: Vec<ExtensionSummary>,
    },
    #[serde(rename = "extension.show")]
    Show { extension: ExtensionDetail },
    #[serde(rename = "extension.run")]
    Run {
        extension_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", flatten)]
        output: Option<homeboy::engine::command::CapturedOutput>,
    },
    #[serde(rename = "extension.setup")]
    Setup { extension_id: String },
    #[serde(rename = "extension.install")]
    Install {
        extension_id: String,
        source: String,
        path: String,
        linked: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_revision: Option<String>,
    },
    #[serde(rename = "extension.update")]
    Update {
        extension_id: String,
        url: String,
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_version: Option<String>,
    },
    #[serde(rename = "extension.update_all")]
    UpdateAll {
        updated: Vec<UpdateEntry>,
        skipped: Vec<String>,
    },
    #[serde(rename = "extension.uninstall")]
    Uninstall {
        extension_id: String,
        path: String,
        was_linked: bool,
    },
    #[serde(rename = "extension.action")]
    Action {
        extension_id: String,
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        response: serde_json::Value,
    },
    #[serde(rename = "extension.set")]
    Set {
        extension_id: String,
        updated_fields: Vec<String>,
    },
    #[serde(rename = "extension.exec")]
    Exec {
        extension_id: String,
        #[serde(skip_serializing_if = "Option::is_none", flatten)]
        output: Option<homeboy::engine::command::CapturedOutput>,
    },
    #[serde(rename = "extension.set")]
    SetBatch { batch: homeboy::BatchResult },
}

#[derive(Serialize)]
pub struct ExtensionDetail {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub runtime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_setup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_ready_check: Option<bool>,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_detail: Option<String>,
    pub linked: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDetail>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<homeboy::extension::InputConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<homeboy::extension::SettingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<RequiresDetail>,
}

#[derive(Serialize)]
pub struct CliDetail {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
}

#[derive(Serialize)]
pub struct ActionDetail {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: homeboy::extension::ActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<homeboy::extension::HttpMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Serialize)]
pub struct RequiresDetail {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}
