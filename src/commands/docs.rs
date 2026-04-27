use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::Path;

use crate::help_topics;
use homeboy::code_audit::codebase_map;
use homeboy::component;

use super::CmdResult;

// ============================================================================
// CLI Args
// ============================================================================

#[derive(Args)]
pub struct DocsArgs {
    #[command(subcommand)]
    pub command: Option<DocsCommand>,

    /// Topic path (e.g., 'commands/deploy') or 'list' to show available topics
    #[arg(value_name = "TOPIC")]
    pub topic: Option<String>,
}

#[derive(Subcommand)]
pub enum DocsCommand {
    /// Generate a machine-optimized codebase map for AI documentation
    Map {
        /// Component to analyze
        component_id: String,

        /// Source directories to analyze (comma-separated). Overrides auto-detection.
        #[arg(long, value_delimiter = ',')]
        source_dirs: Option<Vec<String>>,

        /// Include private methods and internals (default: public API surface only)
        #[arg(long)]
        include_private: bool,

        /// Write markdown documentation files to disk (default: JSON to stdout)
        #[arg(long)]
        write: bool,

        /// Output directory for markdown files (default: docs)
        #[arg(long, default_value = "docs")]
        output_dir: String,
    },
}

// ============================================================================
// Output Types
// ============================================================================

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum DocsOutput {
    #[serde(rename = "docs.map")]
    Map(codebase_map::CodebaseMap),

    #[serde(rename = "docs.map.write")]
    MapWrite {
        files_created: Vec<String>,
        files_updated: Vec<String>,
        hints: Vec<String>,
    },
}

// ============================================================================
// Public API
// ============================================================================

/// Check if this invocation should return JSON (map subcommand)
pub fn is_json_mode(args: &DocsArgs) -> bool {
    matches!(args.command, Some(DocsCommand::Map { .. }))
}

/// Markdown output mode (topic display, list)
pub fn run_markdown(args: DocsArgs) -> CmdResult<String> {
    let topic = args.topic.as_deref().unwrap_or("index");

    if topic == "list" {
        let topics = help_topics::available_topics();
        return Ok((topics.join("\n"), 0));
    }

    let topic_vec = vec![topic.to_string()];
    let resolved = help_topics::resolve(&topic_vec)?;
    Ok((resolved.content, 0))
}

/// JSON output mode (map subcommand)
pub fn run(args: DocsArgs, _global: &super::GlobalArgs) -> CmdResult<DocsOutput> {
    match args.command {
        Some(DocsCommand::Map {
            component_id,
            source_dirs,
            include_private,
            write,
            output_dir,
        }) => run_map(&component_id, source_dirs, include_private, write, &output_dir),
        None => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "JSON output requires the map subcommand. Use `homeboy docs <topic>` for topic display.",
            None,
            Some(vec![
                "homeboy docs map <component-id>".to_string(),
                "homeboy docs commands/deploy".to_string(),
            ]),
        )),
    }
}

// ============================================================================
// Command handlers — thin wrappers around core
// ============================================================================

fn run_map(
    component_id: &str,
    source_dirs: Option<Vec<String>>,
    include_private: bool,
    write: bool,
    output_dir: &str,
) -> CmdResult<DocsOutput> {
    let config = codebase_map::MapConfig {
        component_id,
        source_dirs,
        include_private,
    };

    let map = codebase_map::build_map(&config)?;

    if write {
        let comp = component::load(component_id)?;
        let base = Path::new(&comp.local_path).join(output_dir);
        let files = codebase_map::render_map_to_markdown(&map, &base)?;
        return Ok((
            DocsOutput::MapWrite {
                files_created: files,
                files_updated: vec![],
                hints: vec![format!(
                    "Generated docs from {} classes across {} modules",
                    map.total_classes,
                    map.modules.len()
                )],
            },
            0,
        ));
    }

    Ok((DocsOutput::Map(map), 0))
}
