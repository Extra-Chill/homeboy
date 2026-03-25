//! helpers — extracted from codebase_map.rs.

use std::collections::HashMap;
use std::path::Path;
use serde::Serialize;
use crate::code_audit::fingerprint::{self, FileFingerprint};
use crate::engine::codebase_scan::{self, ExtensionFilter, ScanConfig};
use crate::{component, extension, Error};
use super::MapClass;
use super::MODULE_SPLIT_THRESHOLD;
use super::MapModule;


/// Derive a human-readable module name from a directory path.
/// For generic last segments (V1, V2, src, lib, includes), prepend the parent.
pub(crate) fn derive_module_name(dir: &str) -> String {
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
pub(crate) fn build_class_module_index(modules: &[MapModule]) -> HashMap<String, String> {
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
pub(crate) fn split_classes_by_prefix(classes: &[MapClass]) -> Vec<(String, Vec<&MapClass>)> {
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
pub(crate) fn majority_prefix(classes: &[MapClass]) -> String {
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
