//! discovery — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;

use super::conventions::Language;
use super::fingerprint::{FileFingerprint, fingerprint_file};
use super::walker::walk_source_files;

/// Result of auto-discovering file groups.
pub struct DiscoveryResult {
    /// Grouped files with conventions.
    pub groups: Vec<(String, String, Vec<FileFingerprint>)>,
    /// Total source files found by the walker.
    pub files_walked: usize,
    /// Files that were successfully fingerprinted by an extension.
    pub files_fingerprinted: usize,
}

/// Auto-discover file groups by scanning directories for clusters of similar files.
///
/// Returns groups of (group_name, glob_pattern, files) for directories that
/// contain 2+ files of the same language, plus counts of walked vs fingerprinted files.
pub fn auto_discover_groups(root: &Path) -> DiscoveryResult {
    let mut groups: Vec<(String, String, Vec<FileFingerprint>)> = Vec::new();

    // Walk directories, group files by parent dir + language
    let mut dir_files: HashMap<(String, Language), Vec<FileFingerprint>> = HashMap::new();
    let mut files_walked: usize = 0;
    let mut files_fingerprinted: usize = 0;

    if let Ok(walker) = walk_source_files(root) {
        for path in walker {
            files_walked += 1;
            if let Some(fp) = fingerprint_file(&path, root) {
                files_fingerprinted += 1;
                let parent = path
                    .parent()
                    .and_then(|p| p.strip_prefix(root).ok())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let key = (parent, fp.language.clone());
                dir_files.entry(key).or_default().push(fp);
            }
        }
    }

    for ((dir, _lang), fingerprints) in dir_files {
        if fingerprints.len() < 2 {
            continue;
        }

        let glob_pattern = if dir.is_empty() {
            "*".to_string()
        } else {
            format!("{}/*", dir)
        };

        // Generate a name from the directory
        let name = if dir.is_empty() {
            "Root Files".to_string()
        } else {
            dir.split('/')
                .last()
                .unwrap_or(&dir)
                .replace('-', " ")
                .replace('_', " ")
                .split_whitespace()
                .map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };

        groups.push((name, glob_pattern, fingerprints));
    }

    // Sort by group name for deterministic output
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    DiscoveryResult {
        groups,
        files_walked,
        files_fingerprinted,
    }
}

/// Discover cross-directory conventions by analyzing sibling subdirectories.
///
/// Groups discovered conventions by their grandparent directory, then checks
/// if sibling subdirectories share the same expected methods/registrations.
///
/// Example: if `inc/Abilities/Flow/` and `inc/Abilities/Job/` both expect
/// `execute`, `registerAbility`, `__construct` — that's a cross-directory
/// convention for `inc/Abilities/`.
pub fn discover_cross_directory(
    conventions: &[super::ConventionReport],
) -> Vec<super::DirectoryConvention> {
    // Group conventions by their parent directory (one level up from glob)
    let mut parent_groups: HashMap<String, Vec<&super::ConventionReport>> = HashMap::new();

    for conv in conventions {
        // Extract parent from glob: "inc/Abilities/Flow/*" → "inc/Abilities"
        let parts: Vec<&str> = conv.glob.trim_end_matches("/*").rsplitn(2, '/').collect();
        if parts.len() == 2 {
            let parent = parts[1].to_string();
            parent_groups.entry(parent).or_default().push(conv);
        }
    }

    let mut results = Vec::new();

    for (parent, child_convs) in &parent_groups {
        if child_convs.len() < 2 {
            continue; // Need at least 2 sibling dirs to detect a pattern
        }

        let total = child_convs.len();
        let threshold = (total as f32 * 0.6).ceil() as usize;

        // Count method frequency across sibling conventions
        let mut method_counts: HashMap<&str, usize> = HashMap::new();
        for conv in child_convs {
            for method in &conv.expected_methods {
                *method_counts.entry(method.as_str()).or_insert(0) += 1;
            }
        }

        let expected_methods: Vec<String> = method_counts
            .iter()
            .filter(|(_, count)| **count >= threshold)
            .map(|(name, _)| name.to_string())
            .collect();

        // Count registration frequency across sibling conventions
        let mut reg_counts: HashMap<&str, usize> = HashMap::new();
        for conv in child_convs {
            for reg in &conv.expected_registrations {
                *reg_counts.entry(reg.as_str()).or_insert(0) += 1;
            }
        }

        let expected_registrations: Vec<String> = reg_counts
            .iter()
            .filter(|(_, count)| **count >= threshold)
            .map(|(name, _)| name.to_string())
            .collect();

        if expected_methods.is_empty() && expected_registrations.is_empty() {
            continue; // No shared pattern across siblings
        }

        // Classify sibling directories
        let mut conforming_dirs = Vec::new();
        let mut outlier_dirs = Vec::new();

        for conv in child_convs {
            let dir_name = conv.glob.trim_end_matches("/*").to_string();

            let missing_methods: Vec<String> = expected_methods
                .iter()
                .filter(|m| !conv.expected_methods.contains(m))
                .cloned()
                .collect();

            let missing_registrations: Vec<String> = expected_registrations
                .iter()
                .filter(|r| !conv.expected_registrations.contains(r))
                .cloned()
                .collect();

            if missing_methods.is_empty() && missing_registrations.is_empty() {
                conforming_dirs.push(dir_name);
            } else {
                outlier_dirs.push(super::DirectoryOutlier {
                    dir: dir_name,
                    missing_methods,
                    missing_registrations,
                });
            }
        }

        let confidence = conforming_dirs.len() as f32 / total as f32;

        results.push(super::DirectoryConvention {
            parent: parent.clone(),
            expected_methods,
            expected_registrations,
            conforming_dirs,
            outlier_dirs,
            total_dirs: total,
            confidence,
        });
    }

    results.sort_by(|a, b| a.parent.cmp(&b.parent));
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_helpers::make_convention;

    #[test]
    fn cross_directory_detects_shared_methods() {
        let conventions = vec![
            make_convention("Flow", "inc/Abilities/Flow/*", &["execute", "__construct", "registerAbility"], &[]),
            make_convention("Job", "inc/Abilities/Job/*", &["execute", "__construct", "registerAbility"], &[]),
            make_convention("Data", "inc/Abilities/Data/*", &["execute", "__construct", "registerAbility"], &[]),
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.parent, "inc/Abilities");
        assert!(result.expected_methods.contains(&"execute".to_string()));
        assert!(result.expected_methods.contains(&"__construct".to_string()));
        assert!(result.expected_methods.contains(&"registerAbility".to_string()));
        assert_eq!(result.conforming_dirs.len(), 3);
        assert!(result.outlier_dirs.is_empty());
        assert_eq!(result.total_dirs, 3);
        assert!((result.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cross_directory_detects_outlier_missing_method() {
        let conventions = vec![
            make_convention("Flow", "inc/Abilities/Flow/*", &["execute", "__construct", "registerAbility"], &[]),
            make_convention("Job", "inc/Abilities/Job/*", &["execute", "__construct", "registerAbility"], &[]),
            make_convention("Data", "inc/Abilities/Data/*", &["execute", "__construct"], &[]), // missing registerAbility
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.conforming_dirs.len(), 2);
        assert_eq!(result.outlier_dirs.len(), 1);
        assert_eq!(result.outlier_dirs[0].dir, "inc/Abilities/Data");
        assert!(result.outlier_dirs[0].missing_methods.contains(&"registerAbility".to_string()));
    }

    #[test]
    fn cross_directory_needs_at_least_two_siblings() {
        // Only one subdirectory — no cross-directory convention possible
        let conventions = vec![
            make_convention("Flow", "inc/Abilities/Flow/*", &["execute", "__construct"], &[]),
        ];

        let results = discover_cross_directory(&conventions);
        assert!(results.is_empty());
    }

    #[test]
    fn cross_directory_skips_when_no_shared_methods() {
        // Sibling directories have completely different method sets
        let conventions = vec![
            make_convention("Flow", "inc/Extensions/Flow/*", &["run_flow", "validate_flow"], &[]),
            make_convention("Job", "inc/Extensions/Job/*", &["dispatch_job", "cancel_job"], &[]),
        ];

        let results = discover_cross_directory(&conventions);
        // No method appears in ≥60% of siblings (each appears in 1 of 2 = 50%)
        assert!(results.is_empty());
    }

    #[test]
    fn cross_directory_threshold_allows_partial_overlap() {
        // 3 of 4 siblings share "execute" (75% > 60% threshold) — should detect
        let conventions = vec![
            make_convention("A", "app/Services/A/*", &["execute", "validate"], &[]),
            make_convention("B", "app/Services/B/*", &["execute", "validate"], &[]),
            make_convention("C", "app/Services/C/*", &["execute", "validate"], &[]),
            make_convention("D", "app/Services/D/*", &["process"], &[]), // outlier
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert!(result.expected_methods.contains(&"execute".to_string()));
        assert!(result.expected_methods.contains(&"validate".to_string()));
        assert_eq!(result.conforming_dirs.len(), 3);
        assert_eq!(result.outlier_dirs.len(), 1);
        assert_eq!(result.outlier_dirs[0].dir, "app/Services/D");
    }

    #[test]
    fn cross_directory_includes_shared_registrations() {
        let conventions = vec![
            make_convention("Flow", "inc/Abilities/Flow/*", &["execute"], &["wp_abilities_api_init"]),
            make_convention("Job", "inc/Abilities/Job/*", &["execute"], &["wp_abilities_api_init"]),
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        assert!(results[0].expected_registrations.contains(&"wp_abilities_api_init".to_string()));
    }

    #[test]
    fn cross_directory_separate_parents_produce_separate_conventions() {
        let conventions = vec![
            make_convention("Flow", "inc/Abilities/Flow/*", &["execute", "register"], &[]),
            make_convention("Job", "inc/Abilities/Job/*", &["execute", "register"], &[]),
            make_convention("Auth", "inc/Middleware/Auth/*", &["handle", "boot"], &[]),
            make_convention("Cache", "inc/Middleware/Cache/*", &["handle", "boot"], &[]),
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 2);
        let parents: Vec<&str> = results.iter().map(|r| r.parent.as_str()).collect();
        assert!(parents.contains(&"inc/Abilities"));
        assert!(parents.contains(&"inc/Middleware"));
    }

    #[test]
    fn cross_directory_ignores_top_level_globs() {
        // Glob "steps/*" has no parent directory — rsplitn won't find 2 parts
        let conventions = vec![
            make_convention("Steps", "steps/*", &["execute"], &[]),
            make_convention("Jobs", "jobs/*", &["execute"], &[]),
        ];

        let results = discover_cross_directory(&conventions);
        assert!(results.is_empty()); // These aren't siblings under a common parent
    }
}
