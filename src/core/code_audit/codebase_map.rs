//! Codebase map — structural analysis that builds a navigable map of modules,
//! classes, hooks, and class hierarchies from source code fingerprints.
//!
//! The map is a read-only reference: it scans an entire component's source tree
//! using [`code_audit::fingerprint`] and groups results by directory into modules.
//! Output is either a [`CodebaseMap`] JSON structure or rendered markdown files.

mod markdown_rendering;
mod types;

pub use markdown_rendering::*;
pub use types::*;


use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::code_audit::fingerprint::{self, FileFingerprint};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::{component, extension, Error};

// ============================================================================
// Types
// ============================================================================

// ============================================================================
// Map builder
// ============================================================================

// ============================================================================
// Markdown rendering
// ============================================================================

fn render_hierarchy(hierarchy: &[HierarchyEntry], class_index: &HashMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("# Class Hierarchy\n\n");
    for entry in hierarchy {
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

// ============================================================================
// Helpers
// ============================================================================

/// Derive a human-readable module name from a directory path.
/// For generic last segments (V1, V2, src, lib, includes), prepend the parent.
fn derive_module_name(dir: &str) -> String {
    let segments: Vec<&str> = dir.split('/').collect();
    if segments.is_empty() {
        return dir.to_string();
    }

    let last = *segments.last().unwrap();

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

/// Split classes by common prefix for large module splitting.
fn split_classes_by_prefix(classes: &[MapClass]) -> Vec<(String, Vec<&MapClass>)> {
    let common = majority_prefix(classes);

    let mut groups: HashMap<String, Vec<&MapClass>> = HashMap::new();
    for class in classes {
        let remainder = if class.name.starts_with(&common) {
            &class.name[common.len()..]
        } else {
            &class.name
        };
        let key = remainder
            .find('_')
            .map(|i| &remainder[..i])
            .unwrap_or(remainder);
        let key = if key.is_empty() { "Core" } else { key };
        groups.entry(key.to_string()).or_default().push(class);
    }

    let needs_fallback = groups.len() > 15
        || groups.len() <= 1
        || groups
            .values()
            .any(|g| g.len() > MODULE_SPLIT_THRESHOLD * 2);

    if needs_fallback {
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
fn majority_prefix(classes: &[MapClass]) -> String {
    if classes.is_empty() {
        return String::new();
    }

    let mut prefix_counts: HashMap<&str, usize> = HashMap::new();
    for class in classes {
        let name = &class.name;
        for (i, _) in name.match_indices('_') {
            let prefix = &name[..=i];
            *prefix_counts.entry(prefix).or_default() += 1;
        }
    }

    let threshold = (classes.len() as f64 * 0.5).ceil() as usize;
    let mut best = String::new();
    for (prefix, count) in &prefix_counts {
        if *count >= threshold && prefix.len() > best.len() {
            best = prefix.to_string();
        }
    }

    best
}

// ============================================================================
// Source directory detection
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

fn find_source_directories_by_extension(source_path: &Path, extensions: &[String]) -> Vec<String> {
    let mut dirs = Vec::new();

    if directory_contains_source_files(source_path, extensions) {
        dirs.push(".".to_string());
    }

    if let Ok(entries) = fs::read_dir(source_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

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
