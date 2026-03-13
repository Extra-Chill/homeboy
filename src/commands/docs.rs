use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::Path;

use crate::docs;
use homeboy::codebase_map;
use homeboy::code_audit::docs_audit::AuditResult;
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

    /// Generate documentation files from JSON spec
    Generate {
        /// JSON spec (positional, supports @file and - for stdin)
        spec: Option<String>,

        /// Explicit JSON spec (takes precedence over positional)
        #[arg(long, value_name = "JSON")]
        json: Option<String>,

        /// Generate docs from audit output (pipe from `docs audit --features` or use @file)
        #[arg(long, value_name = "AUDIT_JSON")]
        from_audit: Option<String>,

        /// Show what would be generated without writing files
        #[arg(long)]
        dry_run: bool,
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

    #[serde(rename = "docs.generate")]
    Generate {
        files_created: Vec<String>,
        files_updated: Vec<String>,
        hints: Vec<String>,
    },
}

// ============================================================================
// Public API
// ============================================================================

/// Check if this invocation should return JSON (map or generate subcommand)
pub(crate) fn is_json_mode(args: &DocsArgs) -> bool {
    matches!(
        args.command,
        Some(DocsCommand::Map { .. }) | Some(DocsCommand::Generate { .. })
    )
}

/// Markdown output mode (topic display, list)
pub fn run_markdown(args: DocsArgs) -> CmdResult<String> {
    let topic = args.topic.as_deref().unwrap_or("index");

    if topic == "list" {
        let topics = docs::available_topics();
        return Ok((topics.join("\n"), 0));
    }

    let topic_vec = vec![topic.to_string()];
    let resolved = docs::resolve(&topic_vec)?;
    Ok((resolved.content, 0))
}

/// JSON output mode (map, generate subcommands)
pub fn run(args: DocsArgs, _global: &super::GlobalArgs) -> CmdResult<DocsOutput> {
    match args.command {
        Some(DocsCommand::Map {
            component_id,
            source_dirs,
            include_private,
            write,
            output_dir,
        }) => run_map(&component_id, source_dirs, include_private, write, &output_dir),
        Some(DocsCommand::Generate {
            spec,
            json,
            from_audit,
            dry_run,
        }) => {
            if let Some(ref audit_source) = from_audit {
                run_generate_from_audit(audit_source, dry_run)
            } else {
                let json_spec = json.as_deref().or(spec.as_deref());
                run_generate(json_spec)
            }
        }
        None => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "JSON output requires map or generate subcommand. Use `homeboy docs <topic>` for topic display.",
            None,
            Some(vec![
                "homeboy docs map <component-id>".to_string(),
                "homeboy docs generate --json '<spec>'".to_string(),
                "homeboy docs generate --from-audit @audit.json".to_string(),
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
            DocsOutput::Generate {
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

fn run_generate(json_spec: Option<&str>) -> CmdResult<DocsOutput> {
    let spec_str = json_spec.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "json",
            "Generate requires a JSON spec. Use --json or provide as positional argument.",
            None,
            Some(vec![
                r#"homeboy docs generate --json '{"output_dir":"docs","files":[{"path":"test.md","title":"Test"}]}'"#.to_string(),
            ]),
        )
    })?;

    let json_content = super::merge_json_sources(Some(spec_str), &[])?;
    let spec: homeboy::docs::GenerateSpec = serde_json::from_value(json_content).map_err(|e| {
        homeboy::Error::validation_invalid_json(e, Some("parse generate spec".to_string()), None)
    })?;

    let result = homeboy::docs::generate_from_spec(&spec)?;

    Ok((
        DocsOutput::Generate {
            files_created: result.files_created,
            files_updated: result.files_updated,
            hints: result.hints,
        },
        0,
    ))
}

fn run_generate_from_audit(source: &str, dry_run: bool) -> CmdResult<DocsOutput> {
    // Read audit JSON from @file, stdin (-), file path, or inline string.
    let effective_source = if !source.starts_with('{')
        && !source.starts_with('[')
        && source != "-"
        && !source.starts_with('@')
        && std::path::Path::new(source).exists()
    {
        format!("@{}", source)
    } else {
        source.to_string()
    };
    let json_content = super::merge_json_sources(Some(&effective_source), &[])?;

    // Parse audit result — handle both envelope and raw formats
    let audit: AuditResult = if let Some(data) = json_content.get("data") {
        serde_json::from_value(data.clone())
    } else {
        serde_json::from_value(json_content)
    }
    .map_err(|e| {
        homeboy::Error::validation_invalid_json(e, Some("parse audit result".to_string()), None)
    })?;

    let result = homeboy::docs::generate_from_audit(&audit, dry_run)?;

    Ok((
        DocsOutput::Generate {
            files_created: result.files_created,
            files_updated: result.files_updated,
            hints: result.hints,
        },
        0,
    ))
}
