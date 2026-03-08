//! Convention discovery — detect structural patterns across similar files.
//!
//! Scans files matched by glob patterns, extracts structural fingerprints
//! (method names, registration calls, naming patterns), then groups them
//! to discover conventions and outliers.

use std::collections::HashMap;
use std::path::Path;

use super::fingerprint::FileFingerprint;
use super::import_matching::has_import;
use super::naming::{detect_naming_suffix, suffix_matches};
use super::signatures::{compute_signature_skeleton, tokenize_signature};

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Php,
    Rust,
    JavaScript,
    TypeScript,
    #[default]
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "php" => Language::Php,
            "rs" => Language::Rust,
            "js" | "jsx" | "mjs" => Language::JavaScript,
            "ts" | "tsx" => Language::TypeScript,
            _ => Language::Unknown,
        }
    }
}

/// A discovered convention: a pattern that most files in a group follow.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Convention {
    /// Human-readable name (auto-generated or from config).
    pub name: String,
    /// The glob pattern that groups these files.
    pub glob: String,
    /// The expected methods/functions that define the convention.
    pub expected_methods: Vec<String>,
    /// The expected registration calls.
    pub expected_registrations: Vec<String>,
    /// The expected interfaces/traits that files should implement.
    pub expected_interfaces: Vec<String>,
    /// The expected namespace pattern (if consistent across files).
    pub expected_namespace: Option<String>,
    /// The expected import/use statements.
    pub expected_imports: Vec<String>,
    /// Files that follow the convention.
    pub conforming: Vec<String>,
    /// Files that deviate from the convention.
    pub outliers: Vec<Outlier>,
    /// How many files were analyzed.
    pub total_files: usize,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f32,
}

/// A file that deviates from a convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Outlier {
    /// Relative file path.
    pub file: String,
    /// Whether this outlier appears to be helper/utility drift rather than a real member.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub noisy: bool,
    /// What's missing or different.
    pub deviations: Vec<Deviation>,
}

/// A specific deviation from the convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Deviation {
    /// What kind of deviation.
    pub kind: AuditFinding,
    /// Human-readable description.
    pub description: String,
    /// Suggested fix.
    pub suggestion: String,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AuditFinding {
    MissingMethod,
    ExtraMethod,
    MissingRegistration,
    DifferentRegistration,
    MissingInterface,
    NamingMismatch,
    SignatureMismatch,
    NamespaceMismatch,
    MissingImport,
    /// File exceeds line count threshold.
    GodFile,
    /// File has too many top-level items.
    HighItemCount,
    /// Directory has too many source files in a flat namespace.
    DirectorySprawl,
    /// Function body is duplicated across files.
    DuplicateFunction,
    /// Function has identical structure but different identifiers/literals.
    NearDuplicate,
    /// Function parameter is declared but never used in the function body.
    UnusedParameter,
    /// Developer has marked code with a dead code suppression attribute.
    DeadCodeMarker,
    /// Public function/method is never imported or called by any other file.
    UnreferencedExport,
    /// Private/internal function is never called within the same file.
    OrphanedInternal,
    /// Source file has no corresponding test file.
    MissingTestFile,
    /// Source method/function has no corresponding test method.
    MissingTestMethod,
    /// Test file or test method has no corresponding source file/method.
    OrphanedTest,
    /// Comment starts with TODO/FIXME/HACK/XXX marker.
    TodoMarker,
    /// Comment starts with stale or legacy phrasing.
    LegacyComment,
    /// File violates a configured architecture/layer ownership rule.
    LayerOwnershipViolation,
    /// Inline test modules are present in source files instead of centralized tests.
    InlineTestModule,
    /// Test files are placed under source directories instead of the central tests tree.
    ScatteredTestFile,
    /// Duplicated code block found within the same method/function body.
    IntraMethodDuplicate,
    /// Two functions in different files follow the same call pattern —
    /// they invoke a parallel sequence of helpers, suggesting the shared
    /// workflow should be abstracted into a single parameterized function.
    ParallelImplementation,
}

impl AuditFinding {
    /// All known variant names in snake_case, for CLI help and error messages.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "missing_method",
            "extra_method",
            "missing_registration",
            "different_registration",
            "missing_interface",
            "naming_mismatch",
            "signature_mismatch",
            "namespace_mismatch",
            "missing_import",
            "god_file",
            "high_item_count",
            "directory_sprawl",
            "duplicate_function",
            "near_duplicate",
            "unused_parameter",
            "dead_code_marker",
            "unreferenced_export",
            "orphaned_internal",
            "missing_test_file",
            "missing_test_method",
            "orphaned_test",
            "todo_marker",
            "legacy_comment",
            "layer_ownership_violation",
            "inline_test_module",
            "scattered_test_file",
            "intra_method_duplicate",
            "parallel_implementation",
        ]
    }

}

impl std::str::FromStr for AuditFinding {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        let json = format!("\"{}\"", normalized);
        serde_json::from_str(&json).map_err(|_| {
            format!(
                "unknown finding kind '{}'. Valid kinds: {}",
                value,
                Self::all_names().join(", ")
            )
        })
    }
}

// ============================================================================
// Import Matching
// ============================================================================

// ============================================================================
// Fingerprinting — Extension-powered
// ============================================================================

// ============================================================================
// Convention Discovery
// ============================================================================

/// Discover conventions from a set of fingerprints that share a common grouping.
///
/// The algorithm:
/// 1. Find methods that appear in ≥ 60% of files (the "convention")
/// 2. Find files that are missing any of those methods (the "outliers")
pub fn discover_conventions(
    group_name: &str,
    glob_pattern: &str,
    fingerprints: &[FileFingerprint],
) -> Option<Convention> {
    if fingerprints.len() < 2 {
        return None; // Need at least 2 files to detect a pattern
    }

    let total = fingerprints.len();
    let threshold = (total as f32 * 0.6).ceil() as usize;

    // Count method frequency
    let mut method_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for method in &fp.methods {
            *method_counts.entry(method.clone()).or_insert(0) += 1;
        }
    }

    // Methods appearing in ≥ threshold files are "expected"
    let expected_methods: Vec<String> = method_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.clone())
        .collect();

    if expected_methods.is_empty() {
        return None; // No convention found
    }

    // Count registration frequency
    let mut reg_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for reg in &fp.registrations {
            *reg_counts.entry(reg.clone()).or_insert(0) += 1;
        }
    }

    let expected_registrations: Vec<String> = reg_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.clone())
        .collect();

    // Count interface/trait frequency
    let mut interface_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for iface in &fp.implements {
            *interface_counts.entry(iface.clone()).or_insert(0) += 1;
        }
    }

    let expected_interfaces: Vec<String> = interface_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.clone())
        .collect();

    // Discover namespace convention (most common namespace)
    let mut ns_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        if let Some(ns) = &fp.namespace {
            *ns_counts.entry(ns.clone()).or_insert(0) += 1;
        }
    }
    let expected_namespace = ns_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .max_by_key(|(_, count)| *count)
        .map(|(ns, _)| ns.clone());

    // Discover import conventions (imports appearing in ≥ threshold files)
    let mut import_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for imp in &fp.imports {
            *import_counts.entry(imp.clone()).or_insert(0) += 1;
        }
    }
    let expected_imports: Vec<String> = import_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.clone())
        .collect();

    // Use primary type_name (one per file) for suffix detection so multi-type
    // files don't dilute the convention signal. The full type_names list is only
    // used below for the per-file conformance check.
    let primary_type_names: Vec<String> = fingerprints
        .iter()
        .filter_map(|fp| fp.type_name.clone())
        .collect();

    let naming_suffix = detect_naming_suffix(&primary_type_names);

    // Classify files
    let mut conforming = Vec::new();
    let mut outliers = Vec::new();

    for fp in fingerprints {
        // A file is "helper-like" only if NONE of its types match the convention suffix.
        // This prevents false positives where the primary type_name doesn't match but
        // the file contains another type that does (e.g., VersionOutput + VersionArgs).
        let helper_like = naming_suffix.as_ref().is_some_and(|suffix| {
            let names_to_check: Vec<&str> = if !fp.type_names.is_empty() {
                fp.type_names.iter().map(|s| s.as_str()).collect()
            } else {
                fp.type_name.as_deref().into_iter().collect()
            };
            !names_to_check.is_empty()
                && names_to_check
                    .iter()
                    .all(|name| !suffix_matches(name, suffix))
        });

        let mut deviations = Vec::new();

        if helper_like {
            let suffix = naming_suffix.as_deref().unwrap_or("member");
            deviations.push(Deviation {
                kind: AuditFinding::NamingMismatch,
                description: format!(
                    "Helper-like name does not match convention suffix '{}': {}",
                    suffix,
                    fp.type_name
                        .clone()
                        .unwrap_or_else(|| fp.relative_path.clone())
                ),
                suggestion: format!(
                    "Treat this as a utility/helper or rename it to match the '{}' convention",
                    suffix
                ),
            });
        }

        // Check missing methods
        for expected in &expected_methods {
            if helper_like {
                continue;
            }
            if !fp.methods.contains(expected) {
                deviations.push(Deviation {
                    kind: AuditFinding::MissingMethod,
                    description: format!("Missing method: {}", expected),
                    suggestion: format!(
                        "Add {}() to match the convention in {}",
                        expected, group_name
                    ),
                });
            }
        }

        // Check missing registrations
        for expected in &expected_registrations {
            if helper_like {
                continue;
            }
            if !fp.registrations.contains(expected) {
                deviations.push(Deviation {
                    kind: AuditFinding::MissingRegistration,
                    description: format!("Missing registration: {}", expected),
                    suggestion: format!(
                        "Add {} call to match the convention in {}",
                        expected, group_name
                    ),
                });
            }
        }

        // Check missing interfaces/traits
        for expected in &expected_interfaces {
            if helper_like {
                continue;
            }
            if !fp.implements.contains(expected) {
                deviations.push(Deviation {
                    kind: AuditFinding::MissingInterface,
                    description: format!("Missing interface: {}", expected),
                    suggestion: format!(
                        "Implement {} to match the convention in {}",
                        expected, group_name
                    ),
                });
            }
        }

        // Check namespace mismatch
        if let Some(expected_ns) = &expected_namespace {
            if let Some(actual_ns) = &fp.namespace {
                if actual_ns != expected_ns {
                    deviations.push(Deviation {
                        kind: AuditFinding::NamespaceMismatch,
                        description: format!(
                            "Namespace mismatch: expected `{}`, found `{}`",
                            expected_ns, actual_ns
                        ),
                        suggestion: format!("Change namespace to `{}`", expected_ns),
                    });
                }
            }
            // Missing namespace when others have one is also a deviation
            if fp.namespace.is_none() {
                deviations.push(Deviation {
                    kind: AuditFinding::NamespaceMismatch,
                    description: format!(
                        "Missing namespace declaration (expected `{}`)",
                        expected_ns
                    ),
                    suggestion: format!("Add `namespace {};`", expected_ns),
                });
            }
        }

        // Check missing imports (aware of grouped imports, path equivalence, and usage)
        for expected_imp in &expected_imports {
            if !has_import(expected_imp, &fp.imports, &fp.content) {
                deviations.push(Deviation {
                    kind: AuditFinding::MissingImport,
                    description: format!("Missing import: {}", expected_imp),
                    suggestion: format!(
                        "Add `use {};` to match the convention in {}",
                        expected_imp, group_name
                    ),
                });
            }
        }

        if deviations.is_empty() {
            conforming.push(fp.relative_path.clone());
        } else {
            outliers.push(Outlier {
                file: fp.relative_path.clone(),
                noisy: helper_like,
                deviations,
            });
        }
    }

    let conforming_count = conforming.len();
    let confidence = conforming_count as f32 / total as f32;

    log_status!(
        "audit",
        "Convention '{}': {}/{} files conform (confidence: {:.0}%)",
        group_name,
        conforming_count,
        total,
        confidence * 100.0
    );

    Some(Convention {
        name: group_name.to_string(),
        glob: glob_pattern.to_string(),
        expected_methods,
        expected_registrations,
        expected_interfaces,
        expected_namespace,
        expected_imports,
        conforming,
        outliers,
        total_files: total,
        confidence,
    })
}

// ============================================================================
// Signature Consistency
// ============================================================================

/// Check method signatures across all files in a convention for consistency.
///
/// Uses structural comparison: signatures are tokenized and compared
/// position-by-position. Positions where tokens vary across files are treated
/// as "type parameters" (expected to differ). Only structural differences
/// (different token count, different constant tokens) are flagged.
pub fn check_signature_consistency(conventions: &mut [Convention], root: &Path) {
    for conv in conventions.iter_mut() {
        if conv.expected_methods.is_empty() {
            continue;
        }

        // Detect language from the glob pattern
        let lang = if conv.glob.ends_with(".php") || conv.glob.ends_with("/*") {
            // Check first conforming file extension
            conv.conforming
                .first()
                .and_then(|f| f.rsplit('.').next())
                .map(Language::from_extension)
                .unwrap_or(Language::Unknown)
        } else {
            Language::Unknown
        };

        if lang == Language::Unknown {
            continue;
        }

        // Collect signatures for each method across ALL files (conforming + outliers)
        let all_files: Vec<String> = conv
            .conforming
            .iter()
            .chain(conv.outliers.iter().map(|o| &o.file))
            .cloned()
            .collect();

        // method_name -> [(file, raw_signature)]
        let mut method_sigs: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for file in &all_files {
            let full_path = root.join(file);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let sigs = super::fixer::extract_signatures(&content, &lang);
            for sig in &sigs {
                if conv.expected_methods.contains(&sig.name) {
                    method_sigs
                        .entry(sig.name.clone())
                        .or_default()
                        .push((file.clone(), sig.signature.clone()));
                }
            }
        }

        // For each method, compute the structural skeleton and find mismatches
        let mut new_outlier_deviations: HashMap<String, Vec<Deviation>> = HashMap::new();

        for (method, file_sigs) in &method_sigs {
            if file_sigs.len() < 2 {
                continue;
            }

            let tokenized: Vec<Vec<String>> = file_sigs
                .iter()
                .map(|(_, sig)| tokenize_signature(sig))
                .collect();

            match compute_signature_skeleton(&tokenized) {
                Some(skeleton) => {
                    // Skeleton computed — all signatures have the same structure.
                    // Check each file against the skeleton's constant positions.
                    for (i, (file, sig)) in file_sigs.iter().enumerate() {
                        let tokens = &tokenized[i];
                        let mut mismatches = Vec::new();
                        for (j, expected) in skeleton.iter().enumerate() {
                            if let Some(expected_token) = expected {
                                if j < tokens.len() && &tokens[j] != expected_token {
                                    mismatches.push((expected_token.clone(), tokens[j].clone()));
                                }
                            }
                        }
                        if !mismatches.is_empty() {
                            // This file's constant tokens differ — real mismatch
                            let canonical_sig = skeleton
                                .iter()
                                .map(|s| s.as_deref().unwrap_or("<_>"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            new_outlier_deviations
                                .entry(file.clone())
                                .or_default()
                                .push(Deviation {
                                    kind: AuditFinding::SignatureMismatch,
                                    description: format!(
                                        "Signature mismatch for {}: expected structure `{}`, found `{}`",
                                        method, canonical_sig, sig
                                    ),
                                    suggestion: format!(
                                        "Update {}() to match the structural pattern: `{}`",
                                        method, canonical_sig
                                    ),
                                });
                        }
                    }
                }
                None => {
                    // Different token counts — structural mismatch.
                    // Find the majority token count and flag files that differ.
                    let mut len_counts: HashMap<usize, usize> = HashMap::new();
                    for t in &tokenized {
                        *len_counts.entry(t.len()).or_insert(0) += 1;
                    }
                    let majority_len = len_counts
                        .iter()
                        .max_by_key(|(_, count)| *count)
                        .map(|(len, _)| *len)
                        .unwrap_or(0);

                    // Build canonical from majority-length sigs
                    let majority_sigs: Vec<&Vec<String>> = tokenized
                        .iter()
                        .filter(|t| t.len() == majority_len)
                        .collect();

                    let canonical_display = if let Some(first) = majority_sigs.first() {
                        first.join(" ")
                    } else {
                        continue;
                    };

                    for (i, (file, sig)) in file_sigs.iter().enumerate() {
                        if tokenized[i].len() != majority_len {
                            new_outlier_deviations
                                .entry(file.clone())
                                .or_default()
                                .push(Deviation {
                                    kind: AuditFinding::SignatureMismatch,
                                    description: format!(
                                        "Signature mismatch for {}: different structure — expected {} tokens, found {}. Example: `{}`",
                                        method, majority_len, tokenized[i].len(), sig
                                    ),
                                    suggestion: format!(
                                        "Update {}() to match the structural pattern: `{}`",
                                        method, canonical_display
                                    ),
                                });
                        }
                    }
                }
            }
        }

        if new_outlier_deviations.is_empty() {
            continue;
        }

        // Move conforming files with mismatches to outliers
        let mut moved_files = Vec::new();
        for file in &conv.conforming {
            if let Some(devs) = new_outlier_deviations.remove(file) {
                moved_files.push(file.clone());
                conv.outliers.push(Outlier {
                    file: file.clone(),
                    noisy: false,
                    deviations: devs,
                });
            }
        }
        conv.conforming.retain(|f| !moved_files.contains(f));

        // Add deviations to existing outliers
        for outlier in &mut conv.outliers {
            if let Some(devs) = new_outlier_deviations.remove(&outlier.file) {
                outlier.deviations.extend(devs);
            }
        }

        // Recalculate confidence
        conv.confidence = conv.conforming.len() as f32 / conv.total_files as f32;
    }
}

// ============================================================================
// Auto-Discovery
// ============================================================================

// ============================================================================
// Cross-Directory Discovery
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_convention_from_fingerprints() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "steps/ai-chat.php".to_string(),
                language: Language::Php,
                methods: vec![
                    "register".to_string(),
                    "validate".to_string(),
                    "execute".to_string(),
                ],
                type_name: Some("AiChat".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "steps/webhook.php".to_string(),
                language: Language::Php,
                methods: vec![
                    "register".to_string(),
                    "validate".to_string(),
                    "execute".to_string(),
                ],
                type_name: Some("Webhook".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "steps/agent-ping.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string(), "execute".to_string()],
                type_name: Some("AgentPing".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Step Types", "steps/*.php", &fingerprints).unwrap();

        assert_eq!(convention.name, "Step Types");
        assert!(convention
            .expected_methods
            .contains(&"register".to_string()));
        assert!(convention.expected_methods.contains(&"execute".to_string()));
        assert_eq!(convention.conforming.len(), 2);
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "steps/agent-ping.php");
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| d.description.contains("validate")));
    }

    #[test]
    fn convention_needs_minimum_two_files() {
        let fingerprints = vec![FileFingerprint {
            relative_path: "single.php".to_string(),
            language: Language::Php,
            methods: vec!["run".to_string()],
            ..Default::default()
        }];

        assert!(discover_conventions("Single", "*.php", &fingerprints).is_none());
    }

    #[test]
    fn language_from_extension() {
        assert_eq!(Language::from_extension("php"), Language::Php);
        assert_eq!(Language::from_extension("rs"), Language::Rust);
        assert_eq!(Language::from_extension("ts"), Language::TypeScript);
        assert_eq!(Language::from_extension("jsx"), Language::JavaScript);
        assert_eq!(Language::from_extension("txt"), Language::Unknown);
    }

    #[test]
    fn discover_interface_convention() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/create.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("CreateAbility".to_string()),
                implements: vec!["AbilityInterface".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/update.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                implements: vec!["AbilityInterface".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/helpers.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("Helpers".to_string()),
                ..Default::default()
            },
        ];

        let convention =
            discover_conventions("Abilities", "abilities/*.php", &fingerprints).unwrap();

        // Should detect AbilityInterface as expected
        assert!(convention
            .expected_interfaces
            .contains(&"AbilityInterface".to_string()));

        // helpers.php should be a noisy outlier due to naming mismatch
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "abilities/helpers.php");
        assert!(
            convention.outliers[0].noisy,
            "Helper-like file should be marked noisy"
        );
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| matches!(d.kind, AuditFinding::NamingMismatch)));
    }

    #[test]
    fn helper_like_outlier_collapses_to_naming_mismatch() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("CreateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowHelpers.php".to_string(),
                language: Language::Php,
                methods: vec!["formatFlow".to_string()],
                type_name: Some("FlowHelpers".to_string()),
                ..Default::default()
            },
        ];

        let convention =
            discover_conventions("Abilities", "abilities/*.php", &fingerprints).unwrap();

        assert_eq!(convention.outliers.len(), 1);
        assert!(convention.outliers[0].noisy);
        assert_eq!(convention.outliers[0].deviations.len(), 1);
        assert!(matches!(
            convention.outliers[0].deviations[0].kind,
            AuditFinding::NamingMismatch
        ));
    }

    #[test]
    fn no_interface_convention_when_none_shared() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "a.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                implements: vec!["FooInterface".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "b.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                implements: vec!["BarInterface".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "c.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Mixed", "*.php", &fingerprints).unwrap();

        // No interface appears in ≥60% of files
        assert!(convention.expected_interfaces.is_empty());
    }

    // ========================================================================
    // Signature consistency tests
    // ========================================================================

    #[test]
    fn signature_check_detects_mismatch() {
        let dir = std::env::temp_dir().join("homeboy_sig_mismatch_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("steps")).unwrap();

        // Two conforming files with matching signatures
        std::fs::write(
            dir.join("steps/AiChat.php"),
            r#"<?php
class AiChat {
    public function execute($config, $context) { return []; }
    public function register(): void {}
}
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("steps/Webhook.php"),
            r#"<?php
class Webhook {
    public function execute($config, $context) { return []; }
    public function register(): void {}
}
"#,
        )
        .unwrap();

        // One file with structurally different signature (different param count)
        std::fs::write(
            dir.join("steps/AgentPing.php"),
            r#"<?php
class AgentPing {
    public function execute($config) { return []; }
    public function register(): void {}
}
"#,
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Steps".to_string(),
            glob: "steps/*".to_string(),
            expected_methods: vec!["execute".to_string(), "register".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "steps/AiChat.php".to_string(),
                "steps/Webhook.php".to_string(),
                "steps/AgentPing.php".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        // AgentPing should be moved to outliers
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        assert_eq!(conv.outliers[0].file, "steps/AgentPing.php");
        assert!(conv.outliers[0].deviations.iter().any(|d| {
            d.kind == AuditFinding::SignatureMismatch && d.description.contains("execute")
        }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_check_adds_to_existing_outliers() {
        let dir = std::env::temp_dir().join("homeboy_sig_existing_outlier_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("steps")).unwrap();

        std::fs::write(
            dir.join("steps/AiChat.php"),
            "<?php\nclass AiChat {\n    public function execute($config, $context) { return []; }\n    public function register(): void {}\n}\n",
        ).unwrap();

        std::fs::write(
            dir.join("steps/Webhook.php"),
            "<?php\nclass Webhook {\n    public function execute($config, $context) { return []; }\n    public function register(): void {}\n}\n",
        ).unwrap();

        // File already an outlier (missing register) AND has structurally different execute (1 param vs 2)
        std::fs::write(
            dir.join("steps/Bad.php"),
            "<?php\nclass Bad {\n    public function execute($config) { return []; }\n}\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Steps".to_string(),
            glob: "steps/*".to_string(),
            expected_methods: vec!["execute".to_string(), "register".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "steps/AiChat.php".to_string(),
                "steps/Webhook.php".to_string(),
            ],
            outliers: vec![Outlier {
                file: "steps/Bad.php".to_string(),
                noisy: false,
                deviations: vec![Deviation {
                    kind: AuditFinding::MissingMethod,
                    description: "Missing method: register".to_string(),
                    suggestion: "Add register()".to_string(),
                }],
            }],
            total_files: 3,
            confidence: 0.67,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        // Should have BOTH the original MissingMethod AND the new SignatureMismatch
        assert!(conv.outliers[0].deviations.len() >= 2);
        assert!(conv.outliers[0]
            .deviations
            .iter()
            .any(|d| d.kind == AuditFinding::MissingMethod));
        assert!(conv.outliers[0]
            .deviations
            .iter()
            .any(|d| d.kind == AuditFinding::SignatureMismatch));
    }

    #[test]
    fn signature_check_no_change_when_all_match() {
        let dir = std::env::temp_dir().join("homeboy_sig_all_match_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("steps")).unwrap();

        std::fs::write(
            dir.join("steps/A.php"),
            "<?php\nclass A {\n    public function execute(array $config): array { return []; }\n}\n",
        ).unwrap();

        std::fs::write(
            dir.join("steps/B.php"),
            "<?php\nclass B {\n    public function execute(array $config): array { return []; }\n}\n",
        ).unwrap();

        let mut conventions = vec![Convention {
            name: "Steps".to_string(),
            glob: "steps/*".to_string(),
            expected_methods: vec!["execute".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["steps/A.php".to_string(), "steps/B.php".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert!(conv.outliers.is_empty());
        assert!((conv.confidence - 1.0).abs() < f32::EPSILON);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_check_skips_unknown_language() {
        let dir = std::env::temp_dir().join("homeboy_sig_unknown_lang_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();

        std::fs::write(dir.join("data/a.txt"), "some text\n").unwrap();
        std::fs::write(dir.join("data/b.txt"), "some text\n").unwrap();

        let mut conventions = vec![Convention {
            name: "Data".to_string(),
            glob: "data/*".to_string(),
            expected_methods: vec!["process".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["data/a.txt".to_string(), "data/b.txt".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        // Should not change anything for unknown language
        assert_eq!(conventions[0].conforming.len(), 2);
        assert!(conventions[0].outliers.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signature_check_majority_wins() {
        // 2 files have one signature (2 params), 1 file has another (1 param) — the 2-file version is canonical
        let dir = std::env::temp_dir().join("homeboy_sig_majority_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("steps")).unwrap();

        std::fs::write(
            dir.join("steps/A.php"),
            "<?php\nclass A {\n    public function run($input, $context) { return true; }\n}\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("steps/B.php"),
            "<?php\nclass B {\n    public function run($input, $context) { return true; }\n}\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("steps/C.php"),
            "<?php\nclass C {\n    public function run($input) { return true; }\n}\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Steps".to_string(),
            glob: "steps/*".to_string(),
            expected_methods: vec!["run".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "steps/A.php".to_string(),
                "steps/B.php".to_string(),
                "steps/C.php".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        assert_eq!(conv.outliers[0].file, "steps/C.php");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn return_type_difference_not_a_mismatch() {
        // Files with and without return types should NOT produce a SignatureMismatch.
        // This was the bug reported in #571.
        let dir = std::env::temp_dir().join("homeboy_sig_return_type_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("api")).unwrap();

        std::fs::write(
            dir.join("api/Users.php"),
            "<?php\nclass Users {\n    public function register(): void {}\n    public function check($request) {}\n}\n",
        ).unwrap();

        std::fs::write(
            dir.join("api/Posts.php"),
            "<?php\nclass Posts {\n    public function register() {}\n    public function check($request) {}\n}\n",
        ).unwrap();

        let mut conventions = vec![Convention {
            name: "Api".to_string(),
            glob: "api/*".to_string(),
            expected_methods: vec!["register".to_string(), "check".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["api/Users.php".to_string(), "api/Posts.php".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        // Both files should remain conforming — return type is not structural
        assert_eq!(
            conv.conforming.len(),
            2,
            "Return type difference should not cause mismatch"
        );
        assert!(
            conv.outliers.is_empty(),
            "No outliers expected for return type differences"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn namespace_mismatch_detected_in_convention() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateFlow.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("CreateFlow".to_string()),
                namespace: Some("DataMachine\\Abilities\\Flow".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateFlow.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("UpdateFlow".to_string()),
                namespace: Some("DataMachine\\Abilities\\Flow".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/DeleteFlow.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("DeleteFlow".to_string()),
                namespace: Some("DataMachine\\Flow".to_string()), // WRONG namespace
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Flow", "abilities/*", &fingerprints).unwrap();

        assert_eq!(
            convention.expected_namespace,
            Some("DataMachine\\Abilities\\Flow".to_string())
        );
        assert_eq!(convention.conforming.len(), 2);
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "abilities/DeleteFlow.php");
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| { d.kind == AuditFinding::NamespaceMismatch }));
    }

    #[test]
    fn missing_import_detected_in_convention() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/A.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                imports: vec!["DataMachine\\Core\\Base".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/B.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                imports: vec!["DataMachine\\Core\\Base".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/C.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                // File uses Base but doesn't import it
                content: "class C extends Base {\n    public function execute() {}\n}".to_string(),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Abilities", "abilities/*", &fingerprints).unwrap();

        assert!(convention
            .expected_imports
            .contains(&"DataMachine\\Core\\Base".to_string()));
        assert_eq!(convention.outliers.len(), 1);
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| { d.kind == AuditFinding::MissingImport }));
    }

    #[test]
    fn missing_namespace_detected() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "steps/A.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                namespace: Some("App\\Steps".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "steps/B.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                namespace: Some("App\\Steps".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "steps/C.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                // Missing namespace entirely
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Steps", "steps/*", &fingerprints).unwrap();

        assert_eq!(
            convention.expected_namespace,
            Some("App\\Steps".to_string())
        );
        assert_eq!(convention.outliers.len(), 1);
        assert!(convention.outliers[0].deviations.iter().any(|d| {
            d.kind == AuditFinding::NamespaceMismatch
                && d.description.contains("Missing namespace")
        }));
    }

    // ========================================================================
    // has_import tests
    // ========================================================================

    // ========================================================================
    // type_names tests (issue #554)
    // ========================================================================

    #[test]
    fn no_naming_mismatch_when_type_names_includes_matching_type() {
        // Reproduces issue #554: version.rs has type_name=VersionOutput (first pub type)
        // but also has VersionArgs which matches the convention. Should NOT flag.
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "commands/deploy.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("DeployArgs".to_string()),
                type_names: vec!["DeployArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/lint.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("LintArgs".to_string()),
                type_names: vec!["LintArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/version.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                // Primary type is VersionOutput (first pub type in file)
                type_name: Some("VersionOutput".to_string()),
                // But file also contains VersionArgs
                type_names: vec!["VersionOutput".to_string(), "VersionArgs".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Commands", "commands/*.rs", &fingerprints).unwrap();

        // version.rs should NOT be an outlier because it has VersionArgs in type_names
        assert_eq!(
            convention.outliers.len(),
            0,
            "File with matching type in type_names should not be flagged"
        );
        assert_eq!(convention.conforming.len(), 3);
    }

    #[test]
    fn naming_mismatch_when_no_type_names_match() {
        // When type_names is populated but none match the convention, still flag it
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "commands/deploy.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("DeployArgs".to_string()),
                type_names: vec!["DeployArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/lint.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("LintArgs".to_string()),
                type_names: vec!["LintArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/utils.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("HelperUtils".to_string()),
                // No type matches Args convention
                type_names: vec!["HelperUtils".to_string(), "FormatConfig".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Commands", "commands/*.rs", &fingerprints).unwrap();

        // utils.rs should be an outlier — no type in type_names matches the Args convention
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "commands/utils.rs");
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| matches!(d.kind, AuditFinding::NamingMismatch)));
    }

    #[test]
    fn type_names_fallback_to_type_name_when_empty() {
        // When type_names is not populated (legacy extensions), fall back to type_name
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "commands/deploy.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("DeployArgs".to_string()),
                // type_names empty — simulates old extension
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/lint.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("LintArgs".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/utils.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("HelperUtils".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Commands", "commands/*.rs", &fingerprints).unwrap();

        // utils.rs should be flagged via fallback to type_name
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "commands/utils.rs");
    }
}
