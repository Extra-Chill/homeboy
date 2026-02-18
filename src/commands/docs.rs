use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::docs;
use homeboy::component;
use homeboy::docs_audit::{self, AuditResult};

use super::CmdResult;

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
    /// Analyze codebase and report documentation status (read-only)
    Scaffold {
        /// Component to analyze
        component_id: String,

        /// Docs directory to check for existing documentation (default: docs)
        #[arg(long, default_value = "docs")]
        docs_dir: String,

        /// Source directories to analyze (comma-separated, or repeat flag). Overrides auto-detection.
        #[arg(long, value_delimiter = ',')]
        source_dirs: Option<Vec<String>>,

        /// File extensions to detect as source code (default: php,rs,js,ts,py,go,java,rb,swift,kt)
        #[arg(long, value_delimiter = ',')]
        source_extensions: Option<Vec<String>>,

        /// Include all directories containing source files (extension-based detection)
        #[arg(long)]
        detect_by_extension: bool,
    },

    /// Audit documentation for broken links and stale references
    Audit {
        /// Component to audit
        component_id: String,

        /// Docs directory relative to component root (overrides component config, default: docs)
        #[arg(long)]
        docs_dir: Option<String>,
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
    pub component_id: String,
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

    #[serde(rename = "docs.audit")]
    Audit(AuditResult),

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

/// Check if this invocation should return JSON (scaffold, audit, or generate subcommand)
pub fn is_json_mode(args: &DocsArgs) -> bool {
    matches!(
        args.command,
        Some(DocsCommand::Scaffold { .. })
            | Some(DocsCommand::Audit { .. })
            | Some(DocsCommand::Generate { .. })
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

/// JSON output mode (scaffold, audit, generate subcommands)
pub fn run(args: DocsArgs, _global: &super::GlobalArgs) -> CmdResult<DocsOutput> {
    match args.command {
        Some(DocsCommand::Scaffold {
            component_id,
            docs_dir,
            source_dirs,
            source_extensions,
            detect_by_extension,
        }) => run_scaffold(
            &component_id,
            &docs_dir,
            source_dirs,
            source_extensions,
            detect_by_extension,
        ),
        Some(DocsCommand::Audit { component_id, docs_dir }) => run_audit(&component_id, docs_dir.as_deref()),
        Some(DocsCommand::Generate { spec, json }) => {
            let json_spec = json.as_deref().or(spec.as_deref());
            run_generate(json_spec)
        }
        None => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "JSON output requires scaffold, audit, or generate subcommand. Use `homeboy docs <topic>` for topic display.",
            None,
            Some(vec![
                "homeboy docs scaffold <component-id>".to_string(),
                "homeboy docs audit <component-id>".to_string(),
                "homeboy docs generate --json '<spec>'".to_string(),
                "homeboy docs commands/deploy".to_string(),
            ]),
        )),
    }
}

// ============================================================================
// Scaffold (Analysis Only)
// ============================================================================

fn run_scaffold(
    component_id: &str,
    docs_dir: &str,
    explicit_source_dirs: Option<Vec<String>>,
    source_extensions: Option<Vec<String>>,
    detect_by_extension: bool,
) -> CmdResult<DocsOutput> {
    let comp = component::load(component_id)?;
    let source_path = Path::new(&comp.local_path);
    let docs_path = source_path.join(docs_dir);

    // Analyze source structure
    let source_directories = if let Some(dirs) = explicit_source_dirs {
        // User provided explicit directories
        dirs
    } else if detect_by_extension {
        // Extension-based detection
        let extensions = source_extensions.clone().unwrap_or_else(default_source_extensions);
        find_source_directories_by_extension(source_path, &extensions)
    } else if let Some(extensions) = source_extensions {
        // Custom extensions provided - use extension-based detection automatically
        find_source_directories_by_extension(source_path, &extensions)
    } else {
        // Try conventional directories first
        let conventional = find_source_directories(source_path);
        if conventional.is_empty() {
            // Fallback to extension-based detection with defaults
            let extensions = default_source_extensions();
            find_source_directories_by_extension(source_path, &extensions)
        } else {
            conventional
        }
    };

    // Find existing documentation
    let existing_docs = find_existing_docs(&docs_path);

    // Identify undocumented areas (source dirs without corresponding docs)
    let undocumented = identify_undocumented(&source_directories, &existing_docs, &docs_path);

    // Generate hints
    let mut hints = Vec::new();
    hints.push(format!(
        "Found {} source directories",
        source_directories.len()
    ));
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
                component_id: component_id.to_string(),
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

// ============================================================================
// Audit (Claim-Based Documentation Verification)
// ============================================================================

fn run_audit(component_id: &str, docs_dir: Option<&str>) -> CmdResult<DocsOutput> {
    let result = docs_audit::audit_component(component_id, docs_dir)?;
    Ok((DocsOutput::Audit(result), 0))
}

// ============================================================================
// Scaffold Helper Functions
// ============================================================================

fn default_source_extensions() -> Vec<String> {
    vec![
        "php".to_string(),
        "rs".to_string(),
        "js".to_string(),
        "ts".to_string(),
        "jsx".to_string(),
        "tsx".to_string(),
        "py".to_string(),
        "go".to_string(),
        "java".to_string(),
        "rb".to_string(),
        "swift".to_string(),
        "kt".to_string(),
    ]
}

fn find_source_directories(source_path: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    let source_dir_names = [
        "src",
        "lib",
        "inc",
        "app",
        "components",
        "modules",
        "crates",
    ];

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

/// Find source directories by scanning for files with matching extensions.
/// Returns directories that contain at least one source file (non-recursive for root,
/// recursive one level for subdirectories).
fn find_source_directories_by_extension(source_path: &Path, extensions: &[String]) -> Vec<String> {
    let mut dirs = Vec::new();

    // Check if root contains source files
    if directory_contains_source_files(source_path, extensions) {
        dirs.push(".".to_string());
    }

    // Scan immediate subdirectories
    if let Ok(entries) = fs::read_dir(source_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden directories, common non-source directories
            if name.starts_with('.')
                || name == "node_modules"
                || name == "vendor"
                || name == "docs"
                || name == "tests"
                || name == "test"
                || name == "__pycache__"
                || name == "target"
                || name == "build"
                || name == "dist"
            {
                continue;
            }

            if path.is_dir() && directory_contains_source_files(&path, extensions) {
                dirs.push(name.clone());

                // Also collect immediate subdirectories of this directory
                if let Ok(sub_entries) = fs::read_dir(&path) {
                    for sub_entry in sub_entries.flatten() {
                        let sub_path = sub_entry.path();
                        let sub_name = sub_entry.file_name().to_string_lossy().to_string();

                        if !sub_name.starts_with('.')
                            && sub_path.is_dir()
                            && directory_contains_source_files(&sub_path, extensions)
                        {
                            dirs.push(format!("{}/{}", name, sub_name));
                        }
                    }
                }
            }
        }
    }

    dirs.sort();
    dirs
}

/// Check if a directory contains any files with the given extensions.
fn directory_contains_source_files(dir: &Path, extensions: &[String]) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if extensions.iter().any(|e| e.to_lowercase() == ext_str) {
                        return true;
                    }
                }
            }
        }
    }
    false
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

fn identify_undocumented(
    source_dirs: &[String],
    existing_docs: &[String],
    docs_path: &Path,
) -> Vec<String> {
    // Build a set of doc content references by scanning doc files for source dir mentions
    let mut referenced_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();

    for doc_file in existing_docs {
        let doc_full_path = docs_path.join(doc_file);
        if let Ok(content) = fs::read_to_string(&doc_full_path) {
            for src_dir in source_dirs {
                // Check if the doc references this source directory by path or name
                let dir_name = src_dir.split('/').next_back().unwrap_or(src_dir);
                if content.contains(src_dir)
                    || content.contains(&format!("`{}`", src_dir))
                    || content.contains(&format!("`{}/", src_dir))
                    || content.contains(&format!("{}/", src_dir))
                {
                    referenced_dirs.insert(src_dir.clone());
                }
                // Also check if the dir name appears meaningfully (as path segment)
                if content.contains(&format!("{}/", dir_name))
                    || content.contains(&format!("`{}`", dir_name))
                {
                    referenced_dirs.insert(src_dir.clone());
                }
            }
        }
    }

    source_dirs
        .iter()
        .filter(|src_dir| {
            // Check both: doc filename matching AND content references
            let dir_name = src_dir.split('/').next_back().unwrap_or(src_dir);
            let has_matching_doc = existing_docs
                .iter()
                .any(|doc| doc.contains(dir_name) || doc.replace(".md", "").contains(dir_name));
            let is_referenced = referenced_dirs.contains(*src_dir);
            !has_matching_doc && !is_referenced
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
        homeboy::Error::validation_invalid_json(e, Some("parse generate spec".to_string()), None)
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
                .next_back()
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
    name.split(['-', '_'])
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
