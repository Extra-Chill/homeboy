use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::docs;
use homeboy::component;

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
    },

    /// Audit documentation for broken links and stale references
    Audit {
        /// Component to audit
        component_id: String,
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
pub struct AuditSummary {
    pub docs_audited: usize,
    pub issues_found: usize,
    pub stale_docs: usize,
    pub broken_links: usize,
}

#[derive(Serialize)]
pub struct AuditIssue {
    pub doc: String,
    pub issue_type: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
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
    Audit {
        component_id: String,
        summary: AuditSummary,
        issues: Vec<AuditIssue>,
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
        }) => run_scaffold(&component_id, &docs_dir),
        Some(DocsCommand::Audit { component_id }) => run_audit(&component_id),
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

fn run_scaffold(component_id: &str, docs_dir: &str) -> CmdResult<DocsOutput> {
    let comp = component::load(component_id)?;
    let source_path = Path::new(&comp.local_path);
    let docs_path = source_path.join(docs_dir);

    // Analyze source structure
    let source_directories = find_source_directories(source_path);

    // Find existing documentation
    let existing_docs = find_existing_docs(&docs_path);

    // Identify undocumented areas (source dirs without corresponding docs)
    let undocumented = identify_undocumented(&source_directories, &existing_docs);

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
// Audit (Link Validation + Staleness Detection)
// ============================================================================

fn run_audit(component_id: &str) -> CmdResult<DocsOutput> {
    let comp = component::load(component_id)?;
    let source_path = Path::new(&comp.local_path);
    let docs_path = source_path.join("docs");

    // Find all documentation files
    let doc_files = find_existing_docs(&docs_path);
    let docs_audited = doc_files.len();

    let mut issues = Vec::new();
    let mut stale_docs = 0usize;
    let mut broken_links = 0usize;

    // Audit each doc file
    for doc_file in &doc_files {
        let doc_path = docs_path.join(doc_file);
        let content = match fs::read_to_string(&doc_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Check for broken internal links
        let doc_issues = audit_doc_links(&content, doc_file, &docs_path, source_path);
        for issue in doc_issues {
            if issue.issue_type == "broken_link" {
                broken_links += 1;
            } else if issue.issue_type == "stale" {
                stale_docs += 1;
            }
            issues.push(issue);
        }

        // Check for path references that don't exist
        let path_issues = audit_path_references(&content, doc_file, source_path);
        for issue in path_issues {
            if issue.issue_type == "broken_path" {
                broken_links += 1;
            }
            issues.push(issue);
        }
    }

    // Generate hints
    let mut hints = Vec::new();
    if stale_docs > 0 {
        hints.push(format!("{} docs may need review", stale_docs));
    }
    if broken_links > 0 {
        hints.push(format!("{} broken links should be fixed", broken_links));
    }
    if issues.is_empty() {
        hints.push("All documentation links are valid".to_string());
    }

    Ok((
        DocsOutput::Audit {
            component_id: component_id.to_string(),
            summary: AuditSummary {
                docs_audited,
                issues_found: issues.len(),
                stale_docs,
                broken_links,
            },
            issues,
            hints,
        },
        0,
    ))
}

fn audit_doc_links(content: &str, doc_file: &str, docs_path: &Path, _source_path: &Path) -> Vec<AuditIssue> {
    let mut issues = Vec::new();

    // Regex to find markdown links: [text](path)
    let link_pattern = regex::Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap();

    for (line_num, line) in content.lines().enumerate() {
        for cap in link_pattern.captures_iter(line) {
            let link_path = cap.get(2).map(|m| m.as_str()).unwrap_or("");

            // Skip external links, anchors, and mailto
            if link_path.starts_with("http")
                || link_path.starts_with('#')
                || link_path.starts_with("mailto:")
            {
                continue;
            }

            // Resolve the link relative to the doc file's directory
            let doc_dir = Path::new(doc_file).parent().unwrap_or(Path::new(""));
            let resolved_path = if link_path.starts_with('/') {
                docs_path.join(&link_path[1..])
            } else {
                docs_path.join(doc_dir).join(link_path)
            };

            // Normalize path (handle .. and .)
            let normalized = normalize_path(&resolved_path);

            // Check if file exists (with or without .md extension)
            let exists = normalized.exists()
                || normalized.with_extension("md").exists()
                || normalized.join("index.md").exists();

            if !exists {
                issues.push(AuditIssue {
                    doc: doc_file.to_string(),
                    issue_type: "broken_link".to_string(),
                    detail: format!("Link to '{}' does not resolve", link_path),
                    line: Some(line_num + 1),
                });
            }
        }
    }

    issues
}

fn audit_path_references(content: &str, doc_file: &str, source_path: &Path) -> Vec<AuditIssue> {
    let mut issues = Vec::new();

    // Look for code-style path references like `src/foo/bar.rs` or `inc/core/example.php`
    let path_pattern = regex::Regex::new(r"`((?:src|inc|lib|app)/[a-zA-Z0-9_/.-]+\.[a-z]+)`").unwrap();

    for (line_num, line) in content.lines().enumerate() {
        for cap in path_pattern.captures_iter(line) {
            let path_ref = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let full_path = source_path.join(path_ref);

            if !full_path.exists() {
                issues.push(AuditIssue {
                    doc: doc_file.to_string(),
                    issue_type: "broken_path".to_string(),
                    detail: format!("Referenced file '{}' does not exist", path_ref),
                    line: Some(line_num + 1),
                });
            }
        }
    }

    issues
}

fn normalize_path(path: &Path) -> std::path::PathBuf {
    let mut result = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            _ => result.push(component),
        }
    }
    result
}

// ============================================================================
// Scaffold Helper Functions
// ============================================================================

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
