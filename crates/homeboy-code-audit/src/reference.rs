//! Convention method-set construction and reference-path fingerprinting.
//!
//! Mechanically split out of `mod.rs`; behavior is preserved unchanged.

use std::collections::HashMap;
use std::path::Path;

use super::{conventions, fingerprint, walker};

pub(super) fn build_convention_method_set(
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

    methods
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
pub(super) fn fingerprint_reference_paths(
    reference_paths: &[String],
) -> Vec<fingerprint::FileFingerprint> {
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

        // Slice 2 of #1492: snapshot once, fingerprint from in-memory content
        // instead of re-reading each file inside `fingerprint_file`.
        let snapshot = walker::walk_source_files_snapshot(root);
        for (path, content) in snapshot.iter() {
            if let Some(fp) = fingerprint::fingerprint_content(path, root, content) {
                ref_fps.push(fp);
                total_files += 1;
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

/// Fingerprint this component's source files as dead-code references.
///
/// Dead-code findings are still emitted only for the owned convention
/// fingerprints. This reference-only pass keeps calls from singleton files,
/// index files, and module facades in the graph so exported functions are not
/// reported just because their consumers live outside a convention group.
pub(super) fn fingerprint_component_reference_files(
    root: &Path,
) -> Vec<fingerprint::FileFingerprint> {
    let snapshot = walker::walk_all_source_files_snapshot(root);
    let mut fingerprints = Vec::new();

    for (path, content) in snapshot.iter() {
        if let Some(fp) = fingerprint::fingerprint_content(path, root, content) {
            fingerprints.push(fp);
        }
    }

    fingerprints
}

/// Immutable dead-code inputs that can be shared by candidate and reference
/// projections of a changed-since audit.
#[derive(Debug, Clone, Default)]
pub(super) struct DeadCodeReferenceAnalysis {
    pub(super) external: Vec<fingerprint::FileFingerprint>,
    pub(super) component: Vec<fingerprint::FileFingerprint>,
}

impl DeadCodeReferenceAnalysis {
    pub(super) fn build(root: &Path, reference_paths: &[String]) -> Self {
        #[cfg(test)]
        REPOSITORY_REFERENCE_VISITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            external: fingerprint_reference_paths(reference_paths),
            component: fingerprint_component_reference_files(root),
        }
    }

    /// Replace only paths changed since the reference with fingerprints from the
    /// archived reference tree. All other source and external reference facts are
    /// immutable across the two projections and stay shared.
    pub(super) fn project_reference(
        &self,
        reference_root: &Path,
        changed_files: &[String],
    ) -> Self {
        let changed: std::collections::HashSet<&str> =
            changed_files.iter().map(String::as_str).collect();
        let mut component: HashMap<String, fingerprint::FileFingerprint> = self
            .component
            .iter()
            .filter(|fingerprint| !changed.contains(fingerprint.relative_path.as_str()))
            .map(|fingerprint| (fingerprint.relative_path.clone(), fingerprint.clone()))
            .collect();

        for path in changed_files {
            let source = reference_root.join(path);
            let Ok(content) = std::fs::read_to_string(&source) else {
                continue;
            };
            if let Some(fingerprint) =
                fingerprint::fingerprint_content(&source, reference_root, &content)
            {
                component.insert(fingerprint.relative_path.clone(), fingerprint);
            }
        }

        let mut component: Vec<_> = component.into_values().collect();
        component.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Self {
            external: self.external.clone(),
            component,
        }
    }
}

#[cfg(test)]
static REPOSITORY_REFERENCE_VISITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(super) fn repository_reference_visit_count() -> usize {
    REPOSITORY_REFERENCE_VISITS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
pub(super) fn reset_repository_reference_visit_count() {
    REPOSITORY_REFERENCE_VISITS.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_projection_reuses_candidate_repository_visit() {
        let candidate = tempfile::tempdir().expect("candidate directory");
        let reference = tempfile::tempdir().expect("reference directory");
        std::fs::write(candidate.path().join("changed.rs"), "fn current() {}")
            .expect("candidate source");
        std::fs::write(reference.path().join("changed.rs"), "fn baseline() {}")
            .expect("reference source");

        reset_repository_reference_visit_count();
        let analysis = DeadCodeReferenceAnalysis::build(candidate.path(), &[]);
        let _reference_projection =
            analysis.project_reference(reference.path(), &["changed.rs".to_string()]);

        assert_eq!(
            repository_reference_visit_count(),
            1,
            "changed-since reference projection must not repeat the repository-wide dead-code reference walk"
        );
    }
}
