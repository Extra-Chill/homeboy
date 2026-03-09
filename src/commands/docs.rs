use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::docs;
use homeboy::code_audit::docs_audit::{self, AuditResult, DetectedFeature};
use homeboy::component;
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
    /// Audit documentation for broken links and stale references
    Audit {
        /// Component ID or direct filesystem path to audit
        component_id: String,

        /// Override component local_path for this audit run
        #[arg(long)]
        path: Option<String>,

        /// Docs directory relative to component/project root (overrides config, default: docs)
        #[arg(long)]
        docs_dir: Option<String>,

        /// Save current audit state as baseline for future comparisons
        #[arg(long)]
        baseline: bool,

        /// Skip baseline comparison even if a baseline exists
        #[arg(long)]
        ignore_baseline: bool,

        /// Include full list of all detected features in output
        #[arg(long)]
        features: bool,
    },

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

/// A module in the codebase map — a group of related files.
#[derive(Serialize)]
pub struct MapModule {
    /// Human-readable module name (e.g., "REST API Controllers")
    pub name: String,
    /// Directory path relative to component root
    pub path: String,
    /// Number of source files
    pub file_count: usize,
    /// Classes/types found in this module
    pub classes: Vec<MapClass>,
    /// Methods shared across most files (convention pattern)
    pub shared_methods: Vec<String>,
}

/// A class entry in the codebase map.
#[derive(Serialize)]
pub struct MapClass {
    /// Class/type name
    pub name: String,
    /// File path relative to component root
    pub file: String,
    /// Parent class name, if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// Interfaces and traits
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    /// Namespace
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// Public methods
    pub public_methods: Vec<String>,
    /// Protected methods (only if include_private)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub protected_methods: Vec<String>,
    /// Public/protected properties
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<String>,
    /// Hook references (actions and filters)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<homeboy::extension::HookRef>,
}

/// The class hierarchy: parent → children mapping.
#[derive(Serialize)]
pub struct HierarchyEntry {
    pub parent: String,
    pub children: Vec<String>,
}

/// Summary of hooks in the codebase.
#[derive(Serialize)]
pub struct HookSummary {
    pub total_actions: usize,
    pub total_filters: usize,
    /// Top hook prefixes (e.g., "woocommerce_" → 847)
    pub top_prefixes: Vec<(String, usize)>,
}

/// Full codebase map output.
#[derive(Serialize)]
pub struct CodebaseMap {
    pub component: String,
    pub modules: Vec<MapModule>,
    pub class_hierarchy: Vec<HierarchyEntry>,
    pub hook_summary: HookSummary,
    pub total_files: usize,
    pub total_classes: usize,
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum DocsOutput {
    #[serde(rename = "docs.audit")]
    Audit(AuditResult),

    #[serde(rename = "docs.audit.baseline")]
    AuditBaselineSaved {
        component_id: String,
        path: String,
        broken_references: usize,
        docs_scanned: usize,
    },

    #[serde(rename = "docs.audit.compared")]
    AuditCompared {
        #[serde(flatten)]
        result: AuditResult,
        baseline_comparison: homeboy::code_audit::docs_audit::baseline::BaselineComparison,
    },

    #[serde(rename = "docs.map")]
    Map(CodebaseMap),

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

/// Check if this invocation should return JSON (audit, map, or generate subcommand)
pub(crate) fn is_json_mode(args: &DocsArgs) -> bool {
    matches!(
        args.command,
        Some(DocsCommand::Audit { .. })
            | Some(DocsCommand::Map { .. })
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

/// JSON output mode (audit, map, generate subcommands)
pub fn run(args: DocsArgs, _global: &super::GlobalArgs) -> CmdResult<DocsOutput> {
    match args.command {
        Some(DocsCommand::Audit {
            component_id,
            path,
            docs_dir,
            baseline,
            ignore_baseline,
            features,
        }) => run_audit(
            &component_id,
            path.as_deref(),
            docs_dir.as_deref(),
            features,
            baseline,
            ignore_baseline,
        ),
        Some(DocsCommand::Map { component_id, source_dirs, include_private, write, output_dir }) => run_map(&component_id, source_dirs, include_private, write, &output_dir),
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
            "JSON output requires audit, map, or generate subcommand. Use `homeboy docs <topic>` for topic display.",
            None,
            Some(vec![
                "homeboy docs audit <component-id>".to_string(),
                "homeboy docs map <component-id>".to_string(),
                "homeboy docs generate --json '<spec>'".to_string(),
                "homeboy docs generate --from-audit @audit.json".to_string(),
                "homeboy docs commands/deploy".to_string(),
            ]),
        )),
    }
}

// ============================================================================
// Map (Machine-Optimized Codebase Map)
// ============================================================================

fn run_map(
    component_id: &str,
    explicit_source_dirs: Option<Vec<String>>,
    include_private: bool,
    write: bool,
    output_dir: &str,
) -> CmdResult<DocsOutput> {
    use homeboy::code_audit::fingerprint::FileFingerprint;

    let comp = component::load(component_id)?;
    let root = Path::new(&comp.local_path);

    // Determine which directories to scan
    let source_dirs = if let Some(dirs) = explicit_source_dirs {
        dirs
    } else {
        // Auto-detect: conventional + extension-based fallback
        let conventional = find_source_directories(root);
        if conventional.is_empty() {
            let extensions = default_source_extensions();
            find_source_directories_by_extension(root, &extensions)
        } else {
            conventional
        }
    };

    // Fingerprint all source files
    let mut all_fingerprints: Vec<FileFingerprint> = Vec::new();
    for dir in &source_dirs {
        let dir_path = root.join(dir);
        if !dir_path.is_dir() {
            continue;
        }
        collect_fingerprints_recursive(&dir_path, root, &mut all_fingerprints);
    }

    // Group fingerprints by parent directory
    let mut dir_groups: HashMap<String, Vec<&FileFingerprint>> = HashMap::new();
    for fp in &all_fingerprints {
        let parent = Path::new(&fp.relative_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        dir_groups.entry(parent).or_default().push(fp);
    }

    // Build modules from directory groups
    let mut modules: Vec<MapModule> = Vec::new();
    let mut all_classes: Vec<&FileFingerprint> = Vec::new();

    let mut sorted_dirs: Vec<_> = dir_groups.keys().cloned().collect();
    sorted_dirs.sort();

    for dir in &sorted_dirs {
        let fps = &dir_groups[dir];
        if fps.is_empty() {
            continue;
        }

        // Build class entries
        let mut classes: Vec<MapClass> = Vec::new();
        for fp in fps {
            let type_name = match &fp.type_name {
                Some(name) => name.clone(),
                None => continue, // Skip files without a class/type
            };

            let public_methods: Vec<String> = fp
                .methods
                .iter()
                .filter(|m| fp.visibility.get(*m).map(|v| v == "public").unwrap_or(true))
                .cloned()
                .collect();

            let protected_methods: Vec<String> = if include_private {
                fp.methods
                    .iter()
                    .filter(|m| {
                        fp.visibility
                            .get(*m)
                            .map(|v| v == "protected")
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            classes.push(MapClass {
                name: type_name,
                file: fp.relative_path.clone(),
                extends: fp.extends.clone(),
                implements: fp.implements.clone(),
                namespace: fp.namespace.clone(),
                public_methods,
                protected_methods,
                properties: fp.properties.clone(),
                hooks: fp.hooks.clone(),
            });

            all_classes.push(fp);
        }

        if classes.is_empty() {
            continue;
        }

        // Compute shared methods (methods appearing in >50% of files)
        let method_counts: HashMap<&str, usize> = {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for fp in fps {
                for method in &fp.methods {
                    if fp
                        .visibility
                        .get(method)
                        .map(|v| v == "public")
                        .unwrap_or(true)
                    {
                        *counts.entry(method.as_str()).or_default() += 1;
                    }
                }
            }
            counts
        };
        let threshold = (fps.len() as f64 * 0.5).ceil() as usize;
        let noise_methods = [
            "__construct",
            "__destruct",
            "__toString",
            "__clone",
            "__get",
            "__set",
            "__isset",
            "__unset",
            "__sleep",
            "__wakeup",
            "__invoke",
            "__debugInfo",
            "getInstance",
            "instance",
        ];
        let mut shared: Vec<String> = method_counts
            .iter()
            .filter(|(_, &count)| count >= threshold && count > 1)
            .filter(|(&name, _)| !noise_methods.contains(&name))
            .map(|(&name, _)| name.to_string())
            .collect();
        shared.sort();

        // Derive a human-readable module name from the directory.
        // For generic segments (V1, V2, src, lib, includes), prepend parent.
        let module_name = derive_module_name(dir);

        modules.push(MapModule {
            name: module_name,
            path: dir.clone(),
            file_count: fps.len(),
            classes,
            shared_methods: shared,
        });
    }

    // Build class hierarchy (parent → children)
    let mut hierarchy_map: HashMap<String, Vec<String>> = HashMap::new();
    for fp in &all_fingerprints {
        if let (Some(ref type_name), Some(ref parent)) = (&fp.type_name, &fp.extends) {
            hierarchy_map
                .entry(parent.clone())
                .or_default()
                .push(type_name.clone());
        }
    }
    let mut class_hierarchy: Vec<HierarchyEntry> = hierarchy_map
        .into_iter()
        .map(|(parent, mut children)| {
            children.sort();
            children.dedup();
            HierarchyEntry { parent, children }
        })
        .collect();
    class_hierarchy.sort_by(|a, b| b.children.len().cmp(&a.children.len()));

    // Build hook summary
    let mut action_count = 0usize;
    let mut filter_count = 0usize;
    let mut prefix_counts: HashMap<String, usize> = HashMap::new();
    for fp in &all_fingerprints {
        for hook in &fp.hooks {
            match hook.hook_type.as_str() {
                "action" => action_count += 1,
                "filter" => filter_count += 1,
                _ => {}
            }
            // Extract prefix (up to first _)
            let prefix = hook
                .name
                .find('_')
                .map(|i| &hook.name[..=i])
                .unwrap_or(&hook.name);
            *prefix_counts.entry(prefix.to_string()).or_default() += 1;
        }
    }
    let mut top_prefixes: Vec<(String, usize)> = prefix_counts.into_iter().collect();
    top_prefixes.sort_by(|a, b| b.1.cmp(&a.1));
    top_prefixes.truncate(10);

    let total_files = all_fingerprints.len();
    let total_classes = all_fingerprints
        .iter()
        .filter(|fp| fp.type_name.is_some())
        .count();

    let map = CodebaseMap {
        component: component_id.to_string(),
        modules,
        class_hierarchy,
        hook_summary: HookSummary {
            total_actions: action_count,
            total_filters: filter_count,
            top_prefixes,
        },
        total_files,
        total_classes,
    };

    // --write: render markdown files to disk
    if write {
        let comp = component::load(component_id)?;
        let base = Path::new(&comp.local_path).join(output_dir);
        let files = render_map_to_markdown(&map, &base)?;
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

// ============================================================================
// Markdown Rendering (mechanical doc generation from map data)
// ============================================================================

/// Render a CodebaseMap into markdown files on disk. Returns list of created file paths.
fn render_map_to_markdown(
    map: &CodebaseMap,
    output_dir: &Path,
) -> Result<Vec<String>, homeboy::Error> {
    let mut created = Vec::new();

    // Create output dir
    fs::create_dir_all(output_dir).map_err(|e| {
        homeboy::Error::internal_io(
            e.to_string(),
            Some(format!("create {}", output_dir.display())),
        )
    })?;

    // Build cross-reference indices
    let class_index = build_class_module_index(&map.modules);
    let children_index: HashMap<String, usize> = map
        .class_hierarchy
        .iter()
        .map(|e| (e.parent.clone(), e.children.len()))
        .collect();

    // 1. Write index.md — overview with module listing and hierarchy
    let index = render_index(map);
    let index_path = output_dir.join("index.md");
    write_file(&index_path, &index)?;
    created.push(index_path.to_string_lossy().to_string());

    // 2. Write a doc file per module (with splitting for large modules)
    for module in &map.modules {
        let safe_name = module.path.replace('/', "-");

        if module.classes.len() > MODULE_SPLIT_THRESHOLD {
            // Split large modules: write a summary page + sub-pages
            let summary = render_module_summary(module, &safe_name);
            let summary_path = output_dir.join(format!("{}.md", safe_name));
            write_file(&summary_path, &summary)?;
            created.push(summary_path.to_string_lossy().to_string());

            // Split classes into chunks
            let chunks = split_classes_by_prefix(&module.classes);
            for (suffix, chunk_classes) in &chunks {
                let chunk_name = format!("{}-{}", safe_name, suffix);
                let content = render_module_chunk(module, chunk_classes, suffix, &children_index);
                let chunk_path = output_dir.join(format!("{}.md", chunk_name));
                write_file(&chunk_path, &content)?;
                created.push(chunk_path.to_string_lossy().to_string());
            }
        } else {
            let filename = format!("{}.md", safe_name);
            let content = render_module(module, &children_index);
            let mod_path = output_dir.join(&filename);
            write_file(&mod_path, &content)?;
            created.push(mod_path.to_string_lossy().to_string());
        }
    }

    // 3. Write hierarchy.md with cross-references to module docs
    let hier = render_hierarchy(&map.class_hierarchy, &class_index);
    let hier_path = output_dir.join("hierarchy.md");
    write_file(&hier_path, &hier)?;
    created.push(hier_path.to_string_lossy().to_string());

    // 4. Write hooks.md
    let hooks = render_hooks_summary(&map.hook_summary);
    let hooks_path = output_dir.join("hooks.md");
    write_file(&hooks_path, &hooks)?;
    created.push(hooks_path.to_string_lossy().to_string());

    Ok(created)
}

fn write_file(path: &Path, content: &str) -> Result<(), homeboy::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some(format!("create {}", parent.display())))
        })?;
    }
    fs::write(path, content).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some(format!("write {}", path.display())))
    })
}

fn render_index(map: &CodebaseMap) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", map.component));
    out.push_str(&format!(
        "{} files, {} classes, {} modules\n\n",
        map.total_files,
        map.total_classes,
        map.modules.len()
    ));
    out.push_str(&format!(
        "Hooks: {} actions, {} filters\n\n",
        map.hook_summary.total_actions, map.hook_summary.total_filters
    ));

    out.push_str("## Modules\n\n");
    out.push_str("| Module | Path | Files | Classes | Shared Methods |\n");
    out.push_str("|--------|------|------:|--------:|----------------|\n");
    for module in &map.modules {
        let shared = if module.shared_methods.is_empty() {
            "—".to_string()
        } else {
            module.shared_methods.join(", ")
        };
        out.push_str(&format!(
            "| [{}](./{}.md) | `{}` | {} | {} | {} |\n",
            module.name,
            module.path.replace('/', "-"),
            module.path,
            module.file_count,
            module.classes.len(),
            shared
        ));
    }

    out.push_str("\n## Top Class Hierarchies\n\n");
    for entry in map.class_hierarchy.iter().take(20) {
        out.push_str(&format!(
            "- **{}** → {} children: {}\n",
            entry.parent,
            entry.children.len(),
            entry
                .children
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out
}

fn render_module(module: &MapModule, children_index: &HashMap<String, usize>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — {}\n\n", module.name, module.path));
    out.push_str(&format!(
        "{} files, {} classes\n\n",
        module.file_count,
        module.classes.len()
    ));

    if !module.shared_methods.is_empty() {
        out.push_str(&format!(
            "**Shared interface:** {}\n\n",
            module.shared_methods.join(", ")
        ));
    }

    for class in &module.classes {
        render_class(&mut out, class, children_index);
    }

    out
}

/// Render a summary page for large modules that get split.
fn render_module_summary(module: &MapModule, safe_name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — {}\n\n", module.name, module.path));
    out.push_str(&format!(
        "{} files, {} classes (split into sub-pages)\n\n",
        module.file_count,
        module.classes.len()
    ));

    if !module.shared_methods.is_empty() {
        out.push_str(&format!(
            "**Shared interface:** {}\n\n",
            module.shared_methods.join(", ")
        ));
    }

    // List classes grouped by their prefix-based split
    let chunks = split_classes_by_prefix(&module.classes);
    out.push_str("## Sub-pages\n\n");
    for (suffix, chunk_classes) in &chunks {
        out.push_str(&format!(
            "- [{}](./{}-{}.md) — {} classes\n",
            suffix,
            safe_name,
            suffix,
            chunk_classes.len()
        ));
    }

    out.push_str("\n## All Classes\n\n");
    for class in &module.classes {
        let extras = match &class.extends {
            Some(parent) => format!(" (extends {})", parent),
            None => String::new(),
        };
        out.push_str(&format!(
            "- **{}**{} — {} public methods\n",
            class.name,
            extras,
            class.public_methods.len()
        ));
    }

    out
}

/// Render a chunk of classes from a split module.
fn render_module_chunk(
    module: &MapModule,
    classes: &[&MapClass],
    suffix: &str,
    children_index: &HashMap<String, usize>,
) -> String {
    let mut out = String::new();
    let safe_name = module.path.replace('/', "-");
    out.push_str(&format!(
        "# {} — {} ({})\n\n",
        module.name, module.path, suffix
    ));
    out.push_str(&format!(
        "{} classes ([back to module summary](./{}.md))\n\n",
        classes.len(),
        safe_name
    ));

    for class in classes {
        render_class(&mut out, class, children_index);
    }

    out
}

/// Split classes by common prefix for large module splitting.
/// Groups by next meaningful word after shared prefix (e.g., WC_REST_Product → Product).
/// Falls back to alphabetical by first unique char when grouping produces bad results.
fn split_classes_by_prefix(classes: &[MapClass]) -> Vec<(String, Vec<&MapClass>)> {
    // Find the most common prefix (majority-based, not strict common prefix)
    let common = majority_prefix(classes);

    // Group by next meaningful word after common prefix
    let mut groups: HashMap<String, Vec<&MapClass>> = HashMap::new();
    for class in classes {
        let remainder = if class.name.starts_with(&common) {
            &class.name[common.len()..]
        } else {
            &class.name
        };
        // Take first word (up to next underscore)
        let key = remainder
            .find('_')
            .map(|i| &remainder[..i])
            .unwrap_or(remainder);
        let key = if key.is_empty() { "Core" } else { key };
        groups.entry(key.to_string()).or_default().push(class);
    }

    // Validate: reject if too many tiny groups (>15), one huge group, or only one group
    let needs_fallback = groups.len() > 15
        || groups.len() <= 1
        || groups
            .values()
            .any(|g| g.len() > MODULE_SPLIT_THRESHOLD * 2);

    if needs_fallback {
        // Alphabetical by first char AFTER majority prefix
        let mut alpha_groups: HashMap<String, Vec<&MapClass>> = HashMap::new();
        for class in classes {
            let remainder = if class.name.starts_with(&common) {
                &class.name[common.len()..]
            } else {
                &class.name
            };
            let first = remainder
                .chars()
                .next()
                .unwrap_or('_')
                .to_uppercase()
                .to_string();
            alpha_groups.entry(first).or_default().push(class);
        }

        // If still just one group, try more chars
        if alpha_groups.len() <= 1 {
            alpha_groups.clear();
            for class in classes {
                let remainder = if class.name.starts_with(&common) {
                    &class.name[common.len()..]
                } else {
                    &class.name
                };
                let key: String = remainder.chars().take(3).collect();
                let key = if key.is_empty() {
                    "Other".to_string()
                } else {
                    key
                };
                alpha_groups.entry(key).or_default().push(class);
            }
        }

        let mut sorted: Vec<_> = alpha_groups.into_iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        return sorted;
    }

    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sorted
}

/// Find the most common underscore-delimited prefix among class names.
/// Uses a frequency approach: the prefix shared by the majority (>50%) of classes.
fn majority_prefix(classes: &[MapClass]) -> String {
    if classes.is_empty() {
        return String::new();
    }

    // Count prefix frequencies at each underscore boundary
    let mut prefix_counts: HashMap<&str, usize> = HashMap::new();
    for class in classes {
        let name = &class.name;
        // Find all underscore positions and count each prefix
        for (i, _) in name.match_indices('_') {
            let prefix = &name[..=i]; // include the underscore
            *prefix_counts.entry(prefix).or_default() += 1;
        }
    }

    // Find the longest prefix shared by >50% of classes
    let threshold = (classes.len() as f64 * 0.5).ceil() as usize;
    let mut best = String::new();
    for (prefix, count) in &prefix_counts {
        if *count >= threshold && prefix.len() > best.len() {
            best = prefix.to_string();
        }
    }

    best
}

/// Render a single class entry (shared between normal and chunk rendering).
fn render_class(out: &mut String, class: &MapClass, children_index: &HashMap<String, usize>) {
    out.push_str(&format!("## {}\n\n", class.name));
    out.push_str(&format!("**File:** `{}`\n", class.file));

    if let Some(ref parent) = class.extends {
        out.push_str(&format!("**Extends:** {}\n", parent));
    }
    if !class.implements.is_empty() {
        out.push_str(&format!(
            "**Implements:** {}\n",
            class.implements.join(", ")
        ));
    }
    if let Some(ref ns) = class.namespace {
        out.push_str(&format!("**Namespace:** `{}`\n", ns));
    }

    // Cross-reference: note if this class has children in the hierarchy
    if let Some(&count) = children_index.get(&class.name) {
        out.push_str(&format!(
            "**Children:** {} subclasses ([see hierarchy](./hierarchy.md))\n",
            count
        ));
    }

    out.push('\n');

    // Properties
    if !class.properties.is_empty() {
        out.push_str("### Properties\n\n");
        for prop in &class.properties {
            out.push_str(&format!("- `{}`\n", prop));
        }
        out.push('\n');
    }

    // Public methods — group getters, setters, booleans, other
    if !class.public_methods.is_empty() {
        let getters: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| m.starts_with("get_") || m.starts_with("get"))
            .filter(|m| !m.starts_with("get_") || m.len() > 4)
            .collect();
        let setters: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| m.starts_with("set_") || m.starts_with("set"))
            .filter(|m| !m.starts_with("set_") || m.len() > 4)
            .collect();
        let booleans: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| m.starts_with("is_") || m.starts_with("has_") || m.starts_with("can_"))
            .collect();
        let other: Vec<_> = class
            .public_methods
            .iter()
            .filter(|m| {
                !m.starts_with("get_")
                    && !m.starts_with("get")
                    && !m.starts_with("set_")
                    && !m.starts_with("set")
                    && !m.starts_with("is_")
                    && !m.starts_with("has_")
                    && !m.starts_with("can_")
            })
            .collect();

        out.push_str(&format!(
            "### Public Methods ({})\n\n",
            class.public_methods.len()
        ));

        if !getters.is_empty() {
            out.push_str(&format!(
                "**Getters ({}):** {}\n\n",
                getters.len(),
                getters
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !setters.is_empty() {
            out.push_str(&format!(
                "**Setters ({}):** {}\n\n",
                setters.len(),
                setters
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !booleans.is_empty() {
            out.push_str(&format!(
                "**Checks ({}):** {}\n\n",
                booleans.len(),
                booleans
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !other.is_empty() {
            out.push_str(&format!(
                "**Other ({}):** {}\n\n",
                other.len(),
                other
                    .iter()
                    .map(|m| format!("`{}`", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    // Protected methods
    if !class.protected_methods.is_empty() {
        out.push_str(&format!(
            "### Protected Methods ({})\n\n{}\n\n",
            class.protected_methods.len(),
            class
                .protected_methods
                .iter()
                .map(|m| format!("`{}`", m))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Hooks — mark dynamic hooks
    if !class.hooks.is_empty() {
        let actions: Vec<_> = class
            .hooks
            .iter()
            .filter(|h| h.hook_type == "action")
            .collect();
        let filters: Vec<_> = class
            .hooks
            .iter()
            .filter(|h| h.hook_type == "filter")
            .collect();

        out.push_str(&format!("### Hooks ({})\n\n", class.hooks.len()));
        if !actions.is_empty() {
            out.push_str(&format!(
                "**Actions ({}):** {}\n\n",
                actions.len(),
                actions
                    .iter()
                    .map(|h| format_hook_name(&h.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !filters.is_empty() {
            out.push_str(&format!(
                "**Filters ({}):** {}\n\n",
                filters.len(),
                filters
                    .iter()
                    .map(|h| format_hook_name(&h.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    out.push_str("---\n\n");
}

/// Format a hook name, noting dynamic hooks (those ending with a separator or containing variables).
fn format_hook_name(name: &str) -> String {
    let is_dynamic = name.ends_with('_')
        || name.ends_with('-')
        || name.ends_with('.')
        || name.contains('{')
        || name.contains('$');
    if is_dynamic {
        format!("`{}*` *(dynamic)*", name)
    } else {
        format!("`{}`", name)
    }
}

fn render_hierarchy(hierarchy: &[HierarchyEntry], class_index: &HashMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("# Class Hierarchy\n\n");
    for entry in hierarchy {
        // Link parent to its module doc if known
        let parent_display = if let Some(filename) = class_index.get(&entry.parent) {
            format!("[{}](./{})", entry.parent, filename)
        } else {
            entry.parent.clone()
        };
        out.push_str(&format!(
            "## {} ({} children)\n\n",
            parent_display,
            entry.children.len()
        ));
        for child in &entry.children {
            if let Some(filename) = class_index.get(child) {
                out.push_str(&format!("- [{}](./{})\n", child, filename));
            } else {
                out.push_str(&format!("- {}\n", child));
            }
        }
        out.push('\n');
    }
    out
}

fn render_hooks_summary(summary: &HookSummary) -> String {
    let mut out = String::new();
    out.push_str("# Hooks Summary\n\n");
    out.push_str(&format!(
        "**{} actions, {} filters** ({} total)\n\n",
        summary.total_actions,
        summary.total_filters,
        summary.total_actions + summary.total_filters
    ));
    out.push_str("## Top Prefixes\n\n");
    out.push_str("| Prefix | Count |\n");
    out.push_str("|--------|------:|\n");
    for (prefix, count) in &summary.top_prefixes {
        out.push_str(&format!("| {} | {} |\n", prefix, count));
    }
    out
}

/// Derive a human-readable module name from a directory path.
/// For generic last segments (V1, V2, Version1, src, lib, includes),
/// we prepend the parent segment to give context.
fn derive_module_name(dir: &str) -> String {
    let segments: Vec<&str> = dir.split('/').collect();
    if segments.is_empty() {
        return dir.to_string();
    }

    let last = *segments.last().unwrap();

    // Segments that are too generic on their own
    let generic = [
        "V1",
        "V2",
        "V3",
        "V4",
        "v1",
        "v2",
        "v3",
        "v4",
        "Version1",
        "Version2",
        "Version3",
        "Version4",
        "src",
        "lib",
        "includes",
        "inc",
        "app",
        "Controllers",
        "Models",
        "Views",
        "Routes",
        "Schemas",
        "Utilities",
        "Helpers",
        "Abstract",
        "Interfaces",
    ];

    if segments.len() >= 2 && generic.contains(&last) {
        let parent = segments[segments.len() - 2];
        format!("{} {}", parent, last)
    } else {
        last.to_string()
    }
}

/// Build a lookup from class name → module doc filename for cross-references.
fn build_class_module_index(modules: &[MapModule]) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for module in modules {
        let safe_name = module.path.replace('/', "-");
        let filename = format!("{}.md", safe_name);
        for class in &module.classes {
            index.insert(class.name.clone(), filename.clone());
        }
    }
    index
}

/// Maximum classes in a single module doc before we split it.
const MODULE_SPLIT_THRESHOLD: usize = 30;

/// Recursively collect fingerprints from a directory.
fn collect_fingerprints_recursive(
    dir: &Path,
    root: &Path,
    fingerprints: &mut Vec<homeboy::code_audit::fingerprint::FileFingerprint>,
) {
    use homeboy::code_audit::fingerprint;
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden, vendor, node_modules, tests
        if name.starts_with('.')
            || name == "vendor"
            || name == "node_modules"
            || name == "tests"
            || name == "test"
            || name == "__pycache__"
            || name == "target"
            || name == "build"
            || name == "dist"
        {
            continue;
        }

        if path.is_dir() {
            collect_fingerprints_recursive(&path, root, fingerprints);
        } else if path.is_file() {
            if let Some(fp) = fingerprint::fingerprint_file(&path, root) {
                fingerprints.push(fp);
            }
        }
    }
}

// ============================================================================
// Audit (Claim-Based Documentation Verification)
// ============================================================================

fn run_audit(
    component_id: &str,
    path_override: Option<&str>,
    docs_dir: Option<&str>,
    features: bool,
    baseline: bool,
    ignore_baseline: bool,
) -> CmdResult<DocsOutput> {
    // Resolve effective source path with --path parity to `homeboy audit`
    let (resolved_id, source_path) = if std::path::Path::new(component_id).is_dir() {
        let effective = path_override.unwrap_or(component_id);
        let path = std::path::PathBuf::from(effective);
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        (label, path)
    } else if let Some(path) = path_override {
        (component_id.to_string(), std::path::PathBuf::from(path))
    } else {
        let comp = homeboy::component::load(component_id)?;
        (
            component_id.to_string(),
            std::path::PathBuf::from(&comp.local_path),
        )
    };

    let source_path_str = source_path.to_string_lossy().to_string();

    // Audit directly by path so --path semantics are consistent
    let mut result = docs_audit::audit_path(&source_path_str, docs_dir, features)?;
    result.component_id = resolved_id;

    // --baseline: save current state
    if baseline {
        let saved = docs_audit::baseline::save_baseline(&source_path, &result)?;

        eprintln!(
            "[docs audit] Baseline saved to {} ({} broken reference(s), {} docs scanned)",
            saved.display(),
            result.summary.broken_references,
            result.summary.docs_scanned,
        );

        return Ok((
            DocsOutput::AuditBaselineSaved {
                component_id: result.component_id,
                path: saved.to_string_lossy().to_string(),
                broken_references: result.summary.broken_references,
                docs_scanned: result.summary.docs_scanned,
            },
            0,
        ));
    }

    // Default: compare against baseline if one exists
    if !ignore_baseline {
        if let Some(existing_baseline) = docs_audit::baseline::load_baseline(&source_path) {
            let comparison = docs_audit::baseline::compare(&result, &existing_baseline);

            let exit_code = if comparison.drift_increased { 1 } else { 0 };

            if comparison.drift_increased {
                eprintln!(
                    "[docs audit] DRIFT INCREASED: {} new broken reference(s) since baseline",
                    comparison.new_items.len()
                );
            } else if !comparison.resolved_fingerprints.is_empty() {
                eprintln!(
                    "[docs audit] Drift reduced: {} broken reference(s) resolved since baseline",
                    comparison.resolved_fingerprints.len()
                );
            } else {
                eprintln!("[docs audit] No change from baseline");
            }

            return Ok((
                DocsOutput::AuditCompared {
                    result,
                    baseline_comparison: comparison,
                },
                exit_code,
            ));
        }
    }

    Ok((DocsOutput::Audit(result), 0))
}

// ============================================================================
// Source Directory Detection Helpers (shared by map)
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
    // Read audit JSON from @file, stdin (-), file path, or inline string.
    // Auto-detect bare file paths: if it doesn't look like JSON or stdin
    // and a file exists at that path, treat it as @file.
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
        let heading = target.heading.as_deref().unwrap_or(&default_heading);
        let template = target
            .template
            .as_deref()
            .unwrap_or("- `{name}` ({source_file}:{line})");

        // Render the section content
        let mut section_lines: Vec<String> = Vec::new();
        section_lines.push(heading.to_string());
        section_lines.push(String::new());

        for feature in features {
            let desc = feature.description.as_deref().unwrap_or("");
            let has_fields = template.contains("{fields}") && feature.fields.is_some();
            let line = template
                .replace("{name}", &feature.name)
                .replace("{source_file}", &feature.source_file)
                .replace("{line}", &feature.line.to_string())
                .replace("{description}", desc)
                .replace("{fields}", "") // fields rendered separately below
                .replace(
                    "{documented}",
                    if feature.documented {
                        "yes"
                    } else {
                        "**undocumented**"
                    },
                );

            // Push each line of the template (handles \n in template strings)
            for tpl_line in line.lines() {
                // Skip blank lines that result from empty placeholders
                if tpl_line.trim().is_empty() {
                    continue;
                }
                section_lines.push(tpl_line.to_string());
            }

            // Render fields as sub-items if template requested them
            if has_fields {
                section_lines.push(String::new());
                for field in feature.fields.as_ref().unwrap() {
                    let field_desc = field.description.as_deref().unwrap_or("");
                    if field_desc.is_empty() {
                        section_lines.push(format!("- `{}`", field.name));
                    } else {
                        section_lines.push(format!("- `{}` — {}", field.name, field_desc));
                    }
                }
            }

            section_lines.push(String::new());
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

    // Deduplicate file lists (a file may be appended to multiple times)
    let mut seen = std::collections::HashSet::new();
    files_created.retain(|f| seen.insert(f.clone()));
    seen.clear();
    files_updated.retain(|f| seen.insert(f.clone()));
    // A file that was created shouldn't also appear in updated
    files_updated.retain(|f| !files_created.contains(f));

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
) -> (
    HashMap<String, String>,
    HashMap<String, extension::DocTarget>,
) {
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
    let threshold = std::cmp::min(3, sibling_count.div_ceil(2));

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
    common_headings.sort_by_key(|h| {
        median_positions
            .get(h.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });

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

        write_md(
            dir,
            "a.md",
            "# A\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n",
        );
        write_md(
            dir,
            "b.md",
            "# B\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n",
        );
        write_md(
            dir,
            "c.md",
            "# C\n\n## Configuration\n\n## Parameters\n\n## Error Handling\n",
        );

        let result = infer_sections_from_siblings(dir, "new.md");
        assert!(result.is_some(), "Should find common headings");
        let headings = result.unwrap();
        assert_eq!(
            headings,
            vec!["Configuration", "Parameters", "Error Handling"]
        );
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

        write_md(
            dir,
            "a.md",
            "# A\n\n## Config\n\n## Usage\n\n## Special A\n",
        );
        write_md(
            dir,
            "b.md",
            "# B\n\n## Config\n\n## Usage\n\n## Special B\n",
        );
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
