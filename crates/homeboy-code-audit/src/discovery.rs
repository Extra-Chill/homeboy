//! discovery — extracted from conventions.rs.

use std::collections::HashMap;
use std::path::Path;

use super::conventions::Language;
use super::fingerprint::{fingerprint_content, normalize_convention_tags, FileFingerprint};
use super::walker::{
    extension_provided_file_extensions, is_extension_provided_file, is_index_file, is_test_path,
};
use homeboy_audit_contract::AuditConfig;
use homeboy_engine_primitives::codebase_scan::CodebaseSnapshot;

type DiscoveryGroupKey = (String, Language, bool, Vec<String>);

/// Result of auto-discovering file groups.
pub struct DiscoveryResult {
    /// Grouped files with conventions.
    pub groups: Vec<(String, String, Vec<FileFingerprint>)>,
    /// Every extension-provided source file that fingerprinted, with NO
    /// convention-discovery filtering applied (#10558).
    ///
    /// `groups` is the convention corpus. Two filters exist purely to make
    /// convention *sibling* detection correct:
    ///
    ///   1. index files (`mod.rs`, `lib.rs`, `main.rs`, `index.*`,
    ///      `__init__.py`) are dropped by [`is_index_file`] because they
    ///      organize other files rather than being peers, and
    ///   2. groups with fewer than two members are dropped by
    ///      [`groups_from_dir_files`] because a convention needs peers to exist.
    ///
    /// Neither has any bearing on a whole-file term scan. When source policies
    /// borrowed the convention corpus, those two filters silently made 264 of
    /// this repository's 1819 `.rs` files (14.5%) — including every `mod.rs`,
    /// `lib.rs`, `main.rs`, and every `build.rs` sitting alone in a crate root —
    /// unscannable by ANY source policy, regardless of configuration. Module
    /// roots are exactly where re-exports, feature wiring, and cross-layer glue
    /// accumulate, so that blind spot was pointed at the most policy-relevant
    /// files in the tree.
    ///
    /// This field is that unfiltered corpus. `entry::source_policy_findings_for_path`
    /// already scanned this shape (it builds from
    /// `walker::walk_all_source_files_snapshot`); the engine now agrees with it.
    pub policy_fingerprints: Vec<FileFingerprint>,
    /// Total source files found by the walker.
    pub files_walked: usize,
    /// Files that were successfully fingerprinted by an extension.
    pub files_fingerprinted: usize,
}

/// Auto-discover file groups by scanning directories for clusters of similar files.
///
/// Returns groups of (group_name, glob_pattern, files) for directories that
/// contain 2+ files of the same language, plus counts of walked vs fingerprinted files.
///
/// Also returns [`DiscoveryResult::policy_fingerprints`]: the same walk WITHOUT
/// the two convention-only filters, so path-scanning detectors (source policies)
/// get an honest whole-tree corpus instead of borrowing the convention one.
///
/// Consumes a caller-provided source snapshot. Fingerprinting goes through
/// [`fingerprint_content`] so the snapshot's already-loaded content is reused —
/// no second `read_to_string`. Each file is fingerprinted exactly once and the
/// result is shared between both corpora.
pub(crate) fn auto_discover_groups_from_snapshot(
    root: &Path,
    audit_config: &AuditConfig,
    snapshot: &CodebaseSnapshot,
) -> DiscoveryResult {
    // Walk directories, group files by (parent dir, language, is_test, opaque convention tags).
    // Test files are separated from production files so conventions from
    // production code don't get applied to test files and vice versa.
    // This prevents false positives like test files being flagged for
    // missing production methods (set_up, tear_down are optional hooks).
    let mut dir_files: HashMap<DiscoveryGroupKey, Vec<FileFingerprint>> = HashMap::new();
    let mut policy_fingerprints: Vec<FileFingerprint> = Vec::new();
    let mut files_walked: usize = 0;
    let mut files_fingerprinted: usize = 0;
    let source_extensions = extension_provided_file_extensions();

    for (path, content) in snapshot.iter() {
        if !is_extension_provided_file(path, &source_extensions) {
            continue;
        }
        // Convention discovery excludes index files; the policy corpus does not.
        // `files_walked` / `files_fingerprinted` keep counting the convention
        // corpus so the "no extension can fingerprint these files" warning below
        // keeps its existing meaning.
        let convention_eligible = !is_index_file(path);
        if convention_eligible {
            files_walked += 1;
        }
        let Some(mut fp) = fingerprint_content(path, root, content) else {
            continue;
        };
        fp.convention_tags = convention_tags_for(&fp, audit_config);
        policy_fingerprints.push(policy_view(&fp));

        if !convention_eligible {
            continue;
        }
        files_fingerprinted += 1;
        let parent = path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let file_is_test = is_test_path(&fp.relative_path);
        let key = (
            parent,
            fp.language.clone(),
            file_is_test,
            fp.convention_tags.clone(),
        );
        dir_files.entry(key).or_default().push(fp);
    }

    policy_fingerprints.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    DiscoveryResult {
        groups: groups_from_dir_files(dir_files),
        policy_fingerprints,
        files_walked,
        files_fingerprinted,
    }
}

/// The slice of a fingerprint the source-policy corpus needs: path, language,
/// and raw content.
///
/// Source policies are whole-file term scans — `source_policy::run`,
/// `validate_source_roots`, and `validate_configured_paths` read exactly these
/// three fields and nothing else. Copying only them keeps the second corpus at
/// roughly the cost of the file text instead of duplicating every extracted
/// fact vector (method hashes, call sites, aggregate facts, …) for the whole
/// tree.
///
/// If a detector that needs richer facts is ever moved onto the policy corpus,
/// widen this — or give it the convention corpus — rather than letting it read
/// empty fact vectors.
fn policy_view(fp: &FileFingerprint) -> FileFingerprint {
    FileFingerprint {
        relative_path: fp.relative_path.clone(),
        language: fp.language,
        content: fp.content.clone(),
        convention_tags: fp.convention_tags.clone(),
        ..FileFingerprint::default()
    }
}

fn convention_tags_for(fp: &FileFingerprint, audit_config: &AuditConfig) -> Vec<String> {
    let normalized_path = fp.relative_path.replace('\\', "/");
    let mut tags = fp.convention_tags.clone();
    for rule in &audit_config.convention_tag_globs {
        if rule
            .globs
            .iter()
            .any(|pattern| glob_match::glob_match(pattern, &normalized_path))
        {
            tags.push(rule.tag.clone());
        }
    }
    normalize_convention_tags(tags)
}

fn groups_from_dir_files(
    dir_files: HashMap<DiscoveryGroupKey, Vec<FileFingerprint>>,
) -> Vec<(String, String, Vec<FileFingerprint>)> {
    let mut groups: Vec<(String, String, Vec<FileFingerprint>)> = Vec::new();

    for ((dir, _lang, is_test, convention_tags), fingerprints) in dir_files {
        if fingerprints.len() < 2 {
            continue;
        }

        let glob_pattern = if dir.is_empty() {
            "*".to_string()
        } else {
            format!("{}/*", dir)
        };

        // Generate a name from the directory, with test suffix for test groups
        let base_name = if dir.is_empty() {
            "Root Files".to_string()
        } else {
            dir.split('/')
                .next_back()
                .unwrap_or(&dir)
                .replace(['-', '_'], " ")
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

        let mut name = if is_test {
            format!("{} (Tests)", base_name)
        } else {
            base_name
        };
        if !convention_tags.is_empty() {
            name = format!("{} [{}]", name, convention_tags.join(", "));
        }

        groups.push((name, glob_pattern, fingerprints));
    }

    // Sort by group name for deterministic output
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    groups
}

/// Discover cross-directory conventions by analyzing sibling subdirectories.
///
/// Groups discovered conventions by their grandparent directory, then checks
/// if sibling subdirectories share the same expected methods/registrations.
///
/// Example: if `inc/Abilities/Flow/` and `inc/Abilities/Job/` both expect
/// `execute`, `registerAbility`, `__construct` — that's a cross-directory
/// convention for `inc/Abilities/`.
pub(crate) fn discover_cross_directory(
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
    use super::super::test_helpers::make_convention;
    use super::*;

    fn tagged_fingerprint(path: &str, tags: &[&str]) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Unknown,
            methods: vec!["run".to_string()],
            convention_tags: tags.iter().map(|tag| tag.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn opaque_convention_tags_keep_directory_groups_separate() {
        let mut dir_files: HashMap<DiscoveryGroupKey, Vec<FileFingerprint>> = HashMap::new();

        let alpha = vec!["extension:alpha".to_string()];
        let beta = vec!["extension:beta".to_string()];
        dir_files.insert(
            (
                "src/items".to_string(),
                Language::Unknown,
                false,
                alpha.clone(),
            ),
            vec![
                tagged_fingerprint("src/items/a.one", &["extension:alpha"]),
                tagged_fingerprint("src/items/b.one", &["extension:alpha"]),
            ],
        );
        dir_files.insert(
            (
                "src/items".to_string(),
                Language::Unknown,
                false,
                beta.clone(),
            ),
            vec![
                tagged_fingerprint("src/items/a.two", &["extension:beta"]),
                tagged_fingerprint("src/items/b.two", &["extension:beta"]),
            ],
        );

        let groups = groups_from_dir_files(dir_files);

        assert_eq!(groups.len(), 2);
        assert!(groups
            .iter()
            .any(|(name, _, files)| name == "Items [extension:alpha]" && files.len() == 2));
        assert!(groups
            .iter()
            .any(|(name, _, files)| name == "Items [extension:beta]" && files.len() == 2));
    }

    #[test]
    fn component_convention_tag_globs_add_opaque_grouping_tags() {
        let fp = FileFingerprint {
            relative_path: "src/generated/item.fixture".to_string(),
            convention_tags: vec!["extension:seed".to_string()],
            ..Default::default()
        };
        let audit_config = AuditConfig {
            convention_tag_globs: vec![homeboy_audit_contract::ConventionTagGlob {
                tag: "component:generated".to_string(),
                globs: vec!["src/generated/*".to_string()],
            }],
            ..Default::default()
        };

        let tags = convention_tags_for(&fp, &audit_config);

        assert_eq!(
            tags,
            vec![
                "component:generated".to_string(),
                "extension:seed".to_string()
            ]
        );
    }

    #[test]
    fn cross_directory_detects_shared_methods() {
        let conventions = vec![
            make_convention(
                "Flow",
                "inc/Abilities/Flow/*",
                &["execute", "__construct", "registerAbility"],
                &[],
            ),
            make_convention(
                "Job",
                "inc/Abilities/Job/*",
                &["execute", "__construct", "registerAbility"],
                &[],
            ),
            make_convention(
                "Data",
                "inc/Abilities/Data/*",
                &["execute", "__construct", "registerAbility"],
                &[],
            ),
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.parent, "inc/Abilities");
        assert!(result.expected_methods.contains(&"execute".to_string()));
        assert!(result.expected_methods.contains(&"__construct".to_string()));
        assert!(result
            .expected_methods
            .contains(&"registerAbility".to_string()));
        assert_eq!(result.conforming_dirs.len(), 3);
        assert!(result.outlier_dirs.is_empty());
        assert_eq!(result.total_dirs, 3);
        assert!((result.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cross_directory_detects_outlier_missing_method() {
        let conventions = vec![
            make_convention(
                "Flow",
                "inc/Abilities/Flow/*",
                &["execute", "__construct", "registerAbility"],
                &[],
            ),
            make_convention(
                "Job",
                "inc/Abilities/Job/*",
                &["execute", "__construct", "registerAbility"],
                &[],
            ),
            make_convention(
                "Data",
                "inc/Abilities/Data/*",
                &["execute", "__construct"],
                &[],
            ), // missing registerAbility
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result.conforming_dirs.len(), 2);
        assert_eq!(result.outlier_dirs.len(), 1);
        assert_eq!(result.outlier_dirs[0].dir, "inc/Abilities/Data");
        assert!(result.outlier_dirs[0]
            .missing_methods
            .contains(&"registerAbility".to_string()));
    }

    #[test]
    fn cross_directory_needs_at_least_two_siblings() {
        // Only one subdirectory — no cross-directory convention possible
        let conventions = vec![make_convention(
            "Flow",
            "inc/Abilities/Flow/*",
            &["execute", "__construct"],
            &[],
        )];

        let results = discover_cross_directory(&conventions);
        assert!(results.is_empty());
    }

    #[test]
    fn cross_directory_skips_when_no_shared_methods() {
        // Sibling directories have completely different method sets
        let conventions = vec![
            make_convention(
                "Flow",
                "inc/Extensions/Flow/*",
                &["run_flow", "validate_flow"],
                &[],
            ),
            make_convention(
                "Job",
                "inc/Extensions/Job/*",
                &["dispatch_job", "cancel_job"],
                &[],
            ),
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
            make_convention(
                "Flow",
                "inc/Abilities/Flow/*",
                &["execute"],
                &["wp_abilities_api_init"],
            ),
            make_convention(
                "Job",
                "inc/Abilities/Job/*",
                &["execute"],
                &["wp_abilities_api_init"],
            ),
        ];

        let results = discover_cross_directory(&conventions);

        assert_eq!(results.len(), 1);
        assert!(results[0]
            .expected_registrations
            .contains(&"wp_abilities_api_init".to_string()));
    }

    #[test]
    fn cross_directory_separate_parents_produce_separate_conventions() {
        let conventions = vec![
            make_convention(
                "Flow",
                "inc/Abilities/Flow/*",
                &["execute", "register"],
                &[],
            ),
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
