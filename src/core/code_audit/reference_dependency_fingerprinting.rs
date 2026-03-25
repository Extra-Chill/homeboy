//! reference_dependency_fingerprinting — extracted from mod.rs.

use std::path::Path;
use crate::{component, is_zero, Result};


/// Build the unified convention method set used by duplication and parallel detectors.
///
/// Collects methods from three sources:
/// 1. Per-directory convention expected_methods
/// 2. Cross-directory conventions (methods shared across sibling directory conventions)
/// 3. Cross-file frequency (methods appearing in 3+ files)
/// 4. Naming pattern conventions (prefixes with 5+ unique names across 5+ files)
pub(crate) fn build_convention_method_set(
    discovered_conventions: &[conventions::Convention],
    all_fingerprints: &[&fingerprint::FileFingerprint],
) -> std::collections::HashSet<String> {
    use std::collections::HashMap;

    // 1. Per-directory convention methods
    let mut methods: std::collections::HashSet<String> = discovered_conventions
        .iter()
        .flat_map(|c| c.expected_methods.iter().cloned())
        .collect();

    // 2. Cross-directory: methods shared across 2+ sibling directory conventions
    {
        let mut method_by_parent: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for conv in discovered_conventions {
            let parts: Vec<&str> = conv.glob.split('/').collect();
            if parts.len() >= 3 {
                let parent = parts[..parts.len() - 2].join("/");
                let entry = method_by_parent.entry(parent).or_default();
                for method in &conv.expected_methods {
                    *entry.entry(method.clone()).or_insert(0) += 1;
                }
            }
        }
        for parent_methods in method_by_parent.values() {
            for (method, count) in parent_methods {
                if *count >= 2 {
                    methods.insert(method.clone());
                }
            }
        }
    }

    // 3. Cross-file frequency: methods appearing in 3+ files
    {
        let mut method_file_count: HashMap<&str, usize> = HashMap::new();
        for fp in all_fingerprints {
            let mut seen_in_file = std::collections::HashSet::new();
            for method in &fp.methods {
                if seen_in_file.insert(method.as_str()) {
                    *method_file_count.entry(method.as_str()).or_insert(0) += 1;
                }
            }
        }
        for (method, count) in &method_file_count {
            if *count >= 3 {
                methods.insert(method.to_string());
            }
        }
    }

    // 4. Naming pattern conventions: prefixes with 5+ unique names across 5+ files
    {
        fn extract_prefix(name: &str) -> Option<&str> {
            if let Some(pos) = name.find(|c: char| c.is_uppercase()) {
                if pos > 0 {
                    return Some(&name[..pos]);
                }
            }
            if let Some(pos) = name.find('_') {
                if pos > 0 {
                    return Some(&name[..pos]);
                }
            }
            None
        }

        let mut prefix_methods: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
        let mut prefix_files: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();

        for fp in all_fingerprints {
            for method in &fp.methods {
                if let Some(prefix) = extract_prefix(method) {
                    prefix_methods
                        .entry(prefix)
                        .or_default()
                        .insert(method.as_str());
                    prefix_files
                        .entry(prefix)
                        .or_default()
                        .insert(fp.relative_path.as_str());
                }
            }
        }

        for (prefix, prefix_method_set) in &prefix_methods {
            let file_count = prefix_files.get(prefix).map(|f| f.len()).unwrap_or(0);
            if prefix_method_set.len() >= 5 && file_count >= 5 {
                for method in prefix_method_set {
                    methods.insert(method.to_string());
                }
            }
        }
    }

/// Fingerprint external reference paths for cross-reference analysis.
///
/// Walks each reference path and fingerprints all source files found.
/// These fingerprints provide the call/import data that dead code detection
/// uses to determine whether a function is referenced externally (e.g. by
/// WordPress core calling a hook callback, or a parent plugin importing a class).
///
/// Reference fingerprints are NOT used for convention discovery, duplication
/// detection, or structural analysis — they only enrich the cross-reference set.
pub(crate) fn fingerprint_reference_paths(reference_paths: &[String]) -> Vec<fingerprint::FileFingerprint> {
    if reference_paths.is_empty() {
        return Vec::new();
    }

    let mut ref_fps = Vec::new();
    let mut total_files = 0;

    for ref_path in reference_paths {
        let root = Path::new(ref_path);
        if !root.is_dir() {
            continue;
        }

        if let Ok(walker_iter) = walker::walk_source_files(root) {
            for path in walker_iter {
                if let Some(fp) = fingerprint::fingerprint_file(&path, root) {
                    ref_fps.push(fp);
                    total_files += 1;
                }
            }
        }
    }

    if total_files > 0 {
        log_status!(
            "audit",
            "Reference dependencies: {} file(s) fingerprinted from {} path(s)",
            total_files,
            reference_paths.len()
        );
    }

    ref_fps
}
