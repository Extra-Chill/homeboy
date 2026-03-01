use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::docs;
use homeboy::component;
use homeboy::docs_audit::{self, AuditResult, DetectedFeature};
use homeboy::extension;

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
        /// Component ID or direct filesystem path to audit
        component_id: String,

        /// Docs directory relative to component/project root (overrides config, default: docs)
        #[arg(long)]
        docs_dir: Option<String>,

        /// Include full list of all detected features in output
        #[arg(long)]
        features: bool,
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
        Some(DocsCommand::Audit { component_id, docs_dir, features }) => run_audit(&component_id, docs_dir.as_deref(), features),
        Some(DocsCommand::Generate { spec, json, from_audit, dry_run }) => {
            if let Some(ref audit_source) = from_audit {
                run_generate_from_audit(audit_source, dry_run)
            } else {
                let json_spec = json.as_deref().or(spec.as_deref());
                run_generate(json_spec)
            }
        }
        None => Err(homeboy::Error::validation_invalid_argument(
            "command",
            "JSON output requires scaffold, audit, or generate subcommand. Use `homeboy docs <topic>` for topic display.",
            None,
            Some(vec![
                "homeboy docs scaffold <component-id>".to_string(),
                "homeboy docs audit <component-id>".to_string(),
                "homeboy docs generate --json '<spec>'".to_string(),
                "homeboy docs generate --from-audit @audit.json".to_string(),
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
        let extensions = source_extensions
            .clone()
            .unwrap_or_else(default_source_extensions);
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

fn run_audit(component_id: &str, docs_dir: Option<&str>, features: bool) -> CmdResult<DocsOutput> {
    // If the argument looks like a filesystem path, audit it directly
    // without requiring component registration
    let result = if std::path::Path::new(component_id).is_dir() {
        docs_audit::audit_path(component_id, docs_dir, features)?
    } else {
        docs_audit::audit_component(component_id, docs_dir, features)?
    };
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
        "extensions",
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
        } else {
            // Build title line
            let title_line = if let Some(ref title) = file_spec.title {
                format!("# {}", title)
            } else {
                let name = file_spec
                    .path
                    .trim_end_matches(".md")
                    .split('/')
                    .next_back()
                    .unwrap_or(&file_spec.path);
                format!("# {}", title_from_name(name))
            };

            // Infer section headings from sibling docs in the same directory
            let filename = file_spec
                .path
                .split('/')
                .next_back()
                .unwrap_or(&file_spec.path);
            let sibling_dir = if let Some(parent) = file_path.parent() {
                parent.to_path_buf()
            } else {
                output_path.to_path_buf()
            };
            let sections = infer_sections_from_siblings(&sibling_dir, filename);

            if let Some(headings) = sections {
                let mut parts = vec![title_line, String::new()];
                for heading in headings {
                    parts.push(format!("## {}", heading));
                    parts.push(String::new());
                }
                parts.join("\n")
            } else {
                format!("{}\n", title_line)
            }
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

// ============================================================================
// Generate from Audit
// ============================================================================

fn run_generate_from_audit(source: &str, dry_run: bool) -> CmdResult<DocsOutput> {
    // Read audit JSON from @file, stdin (-), or inline string
    let json_content = super::merge_json_sources(Some(source), &[])?;

    // Parse audit result — handle both envelope and raw formats
    let audit: AuditResult = if let Some(data) = json_content.get("data") {
        serde_json::from_value(data.clone())
    } else {
        serde_json::from_value(json_content)
    }
    .map_err(|e| {
        homeboy::Error::validation_invalid_json(
            e,
            Some("parse audit result".to_string()),
            None,
        )
    })?;

    if audit.detected_features.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "from-audit",
            "Audit result has no detected_features. Run `docs audit --features` to include them.",
            None,
            Some(vec![
                "homeboy docs audit docsync --features > audit.json".to_string(),
                "homeboy docs generate --from-audit @audit.json".to_string(),
            ]),
        ));
    }

    // Load extension config to get labels and doc targets
    let comp = component::load(&audit.component_id).ok();
    let (feature_labels, doc_targets) = collect_extension_doc_config(comp.as_ref());

    // Group features by label
    let groups = group_features_by_label(&audit.detected_features, &feature_labels);

    // Resolve docs directory
    let docs_dir = comp
        .as_ref()
        .and_then(|c| c.docs_dir.as_deref())
        .unwrap_or("docs");
    let source_path = comp
        .as_ref()
        .map(|c| Path::new(&c.local_path).to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let docs_path = source_path.join(docs_dir);

    let mut files_created = Vec::new();
    let mut files_updated = Vec::new();
    let mut hints = Vec::new();

    // For each group that has a doc_target, render into that file
    for (label, features) in &groups {
        let target = match doc_targets.get(label.as_str()) {
            Some(t) => t,
            None => {
                hints.push(format!(
                    "Skipped '{}' ({} features) — no doc_target configured in extension",
                    label,
                    features.len()
                ));
                continue;
            }
        };

        let file_path = docs_path.join(&target.file);
        let default_heading = format!("## {}", label);
        let heading = target
            .heading
            .as_deref()
            .unwrap_or(&default_heading);
        let template = target.template.as_deref().unwrap_or("- `{name}` ({source_file}:{line})");

        // Render the section content
        let mut section_lines: Vec<String> = Vec::new();
        section_lines.push(heading.to_string());
        section_lines.push(String::new());

        for feature in features {
            let line = template
                .replace("{name}", &feature.name)
                .replace("{source_file}", &feature.source_file)
                .replace("{line}", &feature.line.to_string())
                .replace(
                    "{documented}",
                    if feature.documented { "yes" } else { "**undocumented**" },
                );
            section_lines.push(line);
        }
        section_lines.push(String::new());

        let section_content = section_lines.join("\n");

        // Check if the file already exists and has this heading
        let existed = file_path.exists();
        let final_content = if existed {
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            replace_or_append_section(&existing, heading, &section_content)
        } else {
            // New file — add title from label
            let title = format!("# {}\n\n", label);
            format!("{}{}", title, section_content)
        };

        if !dry_run {
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
            fs::write(&file_path, &final_content).map_err(|e| {
                homeboy::Error::internal_io(
                    e.to_string(),
                    Some(format!("write {}", file_path.display())),
                )
            })?;
        }

        let relative = format!("{}/{}", docs_dir, target.file);
        if existed {
            files_updated.push(relative);
        } else {
            files_created.push(relative);
        }
    }

    if dry_run {
        hints.insert(0, "Dry run — no files written".to_string());
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

/// Collect feature_labels and doc_targets from all linked extensions.
fn collect_extension_doc_config(
    comp: Option<&component::Component>,
) -> (HashMap<String, String>, HashMap<String, extension::DocTarget>) {
    let mut labels = HashMap::new();
    let mut targets = HashMap::new();

    if let Some(comp) = comp {
        if let Some(ref extensions) = comp.extensions {
            for extension_id in extensions.keys() {
                if let Ok(manifest) = extension::load_extension(extension_id) {
                    for (key, label) in manifest.audit_feature_labels() {
                        labels.insert(key.clone(), label.clone());
                    }
                    for (label, target) in manifest.audit_doc_targets() {
                        targets.insert(label.clone(), target.clone());
                    }
                }
            }
        }
    }

    (labels, targets)
}

/// Group detected features by their label (resolved from pattern → label mapping).
///
/// The label is determined by finding which key in `feature_labels` is a substring
/// of the feature's pattern string. Features with no matching label are grouped
/// under their raw pattern.
fn group_features_by_label<'a>(
    features: &'a [DetectedFeature],
    feature_labels: &HashMap<String, String>,
) -> Vec<(String, Vec<&'a DetectedFeature>)> {
    let mut groups: HashMap<String, Vec<&'a DetectedFeature>> = HashMap::new();

    for feature in features {
        // Find the label for this feature's pattern
        let label = feature_labels
            .iter()
            .find(|(key, _)| feature.pattern.contains(key.as_str()))
            .map(|(_, label)| label.clone())
            .unwrap_or_else(|| feature.pattern.clone());

        groups.entry(label).or_default().push(feature);
    }

    // Sort groups by label for consistent output
    let mut sorted: Vec<(String, Vec<&DetectedFeature>)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted
}

/// Replace an existing section in a doc file, or append it.
///
/// A "section" starts with the heading line and ends at the next heading of equal
/// or higher level, or end of file.
fn replace_or_append_section(existing: &str, heading: &str, new_section: &str) -> String {
    let heading_level = heading.chars().take_while(|c| *c == '#').count();
    let lines: Vec<&str> = existing.lines().collect();

    // Find the heading line
    let start = lines.iter().position(|line| line.trim() == heading);

    if let Some(start_idx) = start {
        // Find the end of this section (next heading of same or higher level, or EOF)
        let end_idx = lines[start_idx + 1..]
            .iter()
            .position(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with('#') {
                    let level = trimmed.chars().take_while(|c| *c == '#').count();
                    level <= heading_level
                } else {
                    false
                }
            })
            .map(|i| start_idx + 1 + i)
            .unwrap_or(lines.len());

        // Replace the section
        let mut result: Vec<&str> = Vec::new();
        result.extend_from_slice(&lines[..start_idx]);
        // Insert new section content (already includes heading)
        let new_lines: Vec<&str> = new_section.lines().collect();
        result.extend(new_lines);
        if end_idx < lines.len() {
            result.extend_from_slice(&lines[end_idx..]);
        }
        result.join("\n")
    } else {
        // Append the section
        let mut result = existing.to_string();
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push('\n');
        result.push_str(new_section);
        result
    }
}

// ============================================================================
// Section Inference
// ============================================================================

/// Infer common section headings from sibling markdown files in the same directory.
///
/// Reads all `.md` files in `dir` (excluding `exclude_filename`), extracts `## ` headings
/// from each, and returns the ordered list of headings that appear in at least 3 files
/// or 50% of siblings (whichever threshold is lower).
///
/// Returns `None` if fewer than 3 siblings exist or no common headings are found.
fn infer_sections_from_siblings(dir: &Path, exclude_filename: &str) -> Option<Vec<String>> {
    if !dir.is_dir() {
        return None;
    }

    let entries = fs::read_dir(dir).ok()?;

    let mut sibling_headings: Vec<Vec<String>> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Only .md files, skip the file being generated
        if !name.ends_with(".md") || name == exclude_filename || !path.is_file() {
            continue;
        }

        let content = fs::read_to_string(&path).ok();
        if let Some(text) = content {
            let headings: Vec<String> = text
                .lines()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    if trimmed.starts_with("## ") && !trimmed.starts_with("### ") {
                        Some(trimmed.trim_start_matches("## ").trim().to_string())
                    } else {
                        None
                    }
                })
                .collect();

            if !headings.is_empty() {
                sibling_headings.push(headings);
            }
        }
    }

    let sibling_count = sibling_headings.len();
    if sibling_count < 3 {
        return None;
    }

    // Count how many siblings contain each heading
    let mut heading_counts: HashMap<String, usize> = HashMap::new();

    for headings in &sibling_headings {
        let unique: std::collections::HashSet<&String> = headings.iter().collect();
        for heading in unique {
            *heading_counts.entry(heading.clone()).or_insert(0) += 1;
        }
    }

    // Threshold: heading must appear in at least 3 files or 50% of siblings
    let threshold = std::cmp::min(3, (sibling_count + 1) / 2);

    let common_set: std::collections::HashSet<&str> = heading_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.as_str())
        .collect();

    if common_set.is_empty() {
        return None;
    }

    // Determine ordering by median position across siblings.
    // For each common heading, collect its index in every file that has it,
    // then use the median index as the sort key.
    let mut median_positions: HashMap<&str, usize> = HashMap::new();
    for heading in &common_set {
        let mut positions: Vec<usize> = Vec::new();
        for headings in &sibling_headings {
            if let Some(pos) = headings.iter().position(|h| h == heading) {
                positions.push(pos);
            }
        }
        positions.sort();
        let median = positions[positions.len() / 2];
        median_positions.insert(heading, median);
    }

    let mut common_headings: Vec<String> = common_set.iter().map(|s| s.to_string()).collect();
    common_headings.sort_by_key(|h| median_positions.get(h.as_str()).copied().unwrap_or(usize::MAX));

    if common_headings.is_empty() {
        None
    } else {
        Some(common_headings)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("Failed to create temp dir")
    }

    fn write_md(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).expect("Failed to write test file");
    }

    #[test]
    fn test_infer_sections_returns_none_when_fewer_than_3_siblings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n## Usage\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n## Usage\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_none(), "Should return None with only 2 siblings");
    }

    #[test]
    fn test_infer_sections_finds_common_headings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n");
        write_md(dir, "b.md", "# B\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n");
        write_md(dir, "c.md", "# C\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some(), "Should find common headings");
        let headings = result.unwrap();
        assert_eq!(headings, vec!["Configuration", "Parameters", "Error Handling"]);
    }

    #[test]
    fn test_infer_sections_excludes_target_file() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n## Usage\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n## Usage\n");
        write_md(dir, "c.md", "# C\n\n## Config\n\n## Usage\n");
        // This file matches the exclude name — should not be counted
        write_md(dir, "new.md", "# New\n\n## Totally Different\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
        let headings = result.unwrap();
        assert!(headings.contains(&"Config".to_string()));
        assert!(headings.contains(&"Usage".to_string()));
        assert!(!headings.contains(&"Totally Different".to_string()));
    }

    #[test]
    fn test_infer_sections_filters_uncommon_headings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n## Usage\n\n## Special A\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n## Usage\n\n## Special B\n");
        write_md(dir, "c.md", "# C\n\n## Config\n\n## Usage\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
        let headings = result.unwrap();
        assert_eq!(headings, vec!["Config", "Usage"]);
    }

    #[test]
    fn test_infer_sections_returns_none_for_nonexistent_dir() {
        let result = infer_sections_from_siblings(Path::new("/nonexistent/path"), "new.md");
        assert!(result.is_none());
    }

    #[test]
    fn test_infer_sections_skips_non_md_files() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n");
        write_md(dir, "b.md", "# B\n\n## Config\n");
        write_md(dir, "c.md", "# C\n\n## Config\n");
        fs::write(dir.join("readme.txt"), "## Not Markdown\n").unwrap();

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
    }

    #[test]
    fn test_infer_sections_ignores_h3_headings() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Config\n\n### Sub Detail\n");
        write_md(dir, "b.md", "# B\n\n## Config\n\n### Sub Detail\n");
        write_md(dir, "c.md", "# C\n\n## Config\n\n### Sub Detail\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some());
        let headings = result.unwrap();
        assert_eq!(headings, vec!["Config"]);
        assert!(!headings.contains(&"Sub Detail".to_string()));
    }

    #[test]
    fn test_infer_sections_returns_none_when_no_common_pattern() {
        let tmp = create_temp_dir();
        let dir = tmp.path();

        write_md(dir, "a.md", "# A\n\n## Alpha\n");
        write_md(dir, "b.md", "# B\n\n## Beta\n");
        write_md(dir, "c.md", "# C\n\n## Gamma\n");

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_none(), "No heading appears in 3+ files");
    }

    #[test]
    fn test_title_from_name_kebab_case() {
        assert_eq!(title_from_name("google-analytics"), "Google Analytics");
    }

    #[test]
    fn test_title_from_name_snake_case() {
        assert_eq!(title_from_name("page_speed"), "Page Speed");
    }
}
