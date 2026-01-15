use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::docs;

use super::CmdResult;

#[derive(Args)]
pub struct DocsArgs {
    #[command(subcommand)]
    pub command: Option<DocsCommand>,

    /// Topic path (e.g., 'commands/deploy') or 'list' to show available topics
    #[arg(trailing_var_arg = true)]
    pub topic: Vec<String>,
}

#[derive(Subcommand)]
pub enum DocsCommand {
    /// Analyze codebase and report documentation status (read-only)
    Scaffold {
        /// Source directory to analyze (default: current directory)
        #[arg(long)]
        source: Option<String>,

        /// Docs directory to check for existing documentation (default: docs)
        #[arg(long, default_value = "docs")]
        docs_dir: String,
    },

    /// Generate documentation files from JSON spec
    Generate {
        /// JSON spec (positional, supports @file and - for stdin)
        spec: Option<String>,

        /// Explicit JSON spec (takes precedence over positional)
        #[arg(long, value_name = "JSON")]
        json: Option<String>,
    },
}

// ============================================================================
// Output Types
// ============================================================================

#[derive(Serialize)]
pub struct ScaffoldAnalysis {
    pub source_directories: Vec<String>,
    pub existing_docs: Vec<String>,
    pub undocumented: Vec<String>,
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum DocsOutput {
    #[serde(rename = "docs.scaffold")]
    Scaffold {
        analysis: ScaffoldAnalysis,
        instructions: String,
        hints: Vec<String>,
    },

    #[serde(rename = "docs.generate")]
    Generate {
        files_created: Vec<String>,
        files_updated: Vec<String>,
        hints: Vec<String>,
    },
}

// ============================================================================
// Input Types (for generate)
// ============================================================================

#[derive(Deserialize)]
pub struct GenerateSpec {
    pub output_dir: String,
    pub files: Vec<GenerateFileSpec>,
}

#[derive(Deserialize)]
pub struct GenerateFileSpec {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

// ============================================================================
// Public API
// ============================================================================

/// Check if this invocation should return JSON (scaffold or generate subcommand)
pub fn is_json_mode(args: &DocsArgs) -> bool {
    matches!(
        args.command,
        Some(DocsCommand::Scaffold { .. }) | Some(DocsCommand::Generate { .. })
    )
}

/// Markdown output mode (topic display, list)
pub fn run_markdown(args: DocsArgs) -> CmdResult<String> {
    if args.topic.len() == 1 && args.topic[0] == "list" {
        let topics = docs::available_topics();
        return Ok((topics.join("\n"), 0));
    }

    let resolved = docs::resolve(&args.topic)?;
    Ok((resolved.content, 0))
}

/// JSON output mode (scaffold, generate subcommands)
pub fn run(args: DocsArgs, _global: &super::GlobalArgs) -> CmdResult<DocsOutput> {
    match args.command {
        Some(DocsCommand::Scaffold { source, docs_dir }) => {
            run_scaffold(source.as_deref(), &docs_dir)
        }
        Some(DocsCommand::Generate { spec, json }) => {
            let json_spec = json.as_deref().or(spec.as_deref());
            run_generate(json_spec)
        }
        None => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "JSON output requires scaffold or generate subcommand. Use `homeboy docs <topic>` for topic display.",
            None,
            Some(vec![
                "homeboy docs scaffold".to_string(),
                "homeboy docs generate --json '<spec>'".to_string(),
                "homeboy docs commands/deploy".to_string(),
            ]),
        )),
    }
}

// ============================================================================
// Scaffold (Analysis Only)
// ============================================================================

fn run_scaffold(source_dir: Option<&str>, docs_dir: &str) -> CmdResult<DocsOutput> {
    let source = source_dir.unwrap_or(".");
    let source_path = Path::new(source);
    let docs_path = Path::new(docs_dir);

    // Analyze source structure
    let source_directories = find_source_directories(source_path);

    // Find existing documentation
    let existing_docs = find_existing_docs(docs_path);

    // Identify undocumented areas (source dirs without corresponding docs)
    let undocumented = identify_undocumented(&source_directories, &existing_docs);

    // Generate hints
    let mut hints = Vec::new();
    hints.push(format!("Found {} source directories", source_directories.len()));
    if !existing_docs.is_empty() {
        hints.push(format!("{} docs already exist", existing_docs.len()));
    }
    if !undocumented.is_empty() {
        hints.push(format!(
            "{} directories may need documentation",
            undocumented.len()
        ));
    }

    Ok((
        DocsOutput::Scaffold {
            analysis: ScaffoldAnalysis {
                source_directories,
                existing_docs,
                undocumented,
            },
            instructions: "Run `homeboy docs documentation/generation` for writing guidelines"
                .to_string(),
            hints,
        },
        0,
    ))
}

fn find_source_directories(source_path: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    let source_dir_names = ["src", "lib", "inc", "app", "components", "modules", "crates"];

    for dir_name in &source_dir_names {
        let dir_path = source_path.join(dir_name);
        if dir_path.is_dir() {
            dirs.push(dir_name.to_string());
            // Also collect immediate subdirectories
            if let Ok(entries) = fs::read_dir(&dir_path) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.starts_with('.') {
                            dirs.push(format!("{}/{}", dir_name, name));
                        }
                    }
                }
            }
        }
    }

    dirs.sort();
    dirs
}

fn find_existing_docs(docs_path: &Path) -> Vec<String> {
    let mut docs = Vec::new();

    if !docs_path.exists() {
        return docs;
    }

    fn scan_docs(dir: &Path, prefix: &str, docs: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if name.starts_with('.') {
                    continue;
                }

                if path.is_file() && name.ends_with(".md") {
                    let relative = if prefix.is_empty() {
                        name
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    docs.push(relative);
                } else if path.is_dir() {
                    let new_prefix = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    scan_docs(&path, &new_prefix, docs);
                }
            }
        }
    }

    scan_docs(docs_path, "", &mut docs);
    docs.sort();
    docs
}

fn identify_undocumented(source_dirs: &[String], existing_docs: &[String]) -> Vec<String> {
    // Simple heuristic: source dirs without a matching doc file
    source_dirs
        .iter()
        .filter(|src_dir| {
            // Check if any doc contains this directory name
            let dir_name = src_dir.split('/').last().unwrap_or(src_dir);
            !existing_docs
                .iter()
                .any(|doc| doc.contains(dir_name) || doc.replace(".md", "").contains(dir_name))
        })
        .cloned()
        .collect()
}

// ============================================================================
// Generate (Bulk File Creation)
// ============================================================================

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

    // Handle stdin and @file patterns
    let json_content = super::merge_json_sources(Some(spec_str), &[])?;
    let spec: GenerateSpec = serde_json::from_value(json_content).map_err(|e| {
        homeboy::Error::validation_invalid_json(e, Some("parse generate spec".to_string()))
    })?;

    let output_path = Path::new(&spec.output_dir);

    // Create output directory if needed
    if !output_path.exists() {
        fs::create_dir_all(output_path).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some(format!("create {}", spec.output_dir)))
        })?;
    }

    let mut files_created = Vec::new();
    let mut files_updated = Vec::new();

    for file_spec in &spec.files {
        let file_path = output_path.join(&file_spec.path);

        // Create parent directories
        if let Some(parent) = file_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    homeboy::Error::internal_io(
                        e.to_string(),
                        Some(format!("create {}", parent.display())),
                    )
                })?;
            }
        }

        // Determine content
        let content = if let Some(ref c) = file_spec.content {
            c.clone()
        } else if let Some(ref title) = file_spec.title {
            format!("# {}\n", title)
        } else {
            // Use filename as title
            let name = file_spec
                .path
                .trim_end_matches(".md")
                .split('/')
                .last()
                .unwrap_or(&file_spec.path);
            format!("# {}\n", title_from_name(name))
        };

        // Track if updating or creating
        let existed = file_path.exists();

        // Write file
        fs::write(&file_path, &content).map_err(|e| {
            homeboy::Error::internal_io(
                e.to_string(),
                Some(format!("write {}", file_path.display())),
            )
        })?;

        let relative_path = file_path.to_string_lossy().to_string();
        if existed {
            files_updated.push(relative_path);
        } else {
            files_created.push(relative_path);
        }
    }

    // Generate hints
    let mut hints = Vec::new();
    if !files_created.is_empty() {
        hints.push(format!("Created {} files", files_created.len()));
    }
    if !files_updated.is_empty() {
        hints.push(format!("Updated {} files", files_updated.len()));
    }

    Ok((
        DocsOutput::Generate {
            files_created,
            files_updated,
            hints,
        },
        0,
    ))
}

fn title_from_name(name: &str) -> String {
    // Convert kebab-case or snake_case to Title Case
    name.split(|c| c == '-' || c == '_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
