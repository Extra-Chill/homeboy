//! Convention discovery — detect structural patterns across similar files.
//!
//! Scans files matched by glob patterns, extracts structural fingerprints
//! (method names, registration calls, naming patterns), then groups them
//! to discover conventions and outliers.

mod audit_finding;
mod language;
mod signature_consistency;
mod types;

pub use audit_finding::*;
pub use language::*;
pub use signature_consistency::*;
pub use types::*;


use std::collections::HashMap;
use std::path::Path;

use super::fingerprint::FileFingerprint;
use super::import_matching::has_import;
use super::naming::{detect_naming_suffix, suffix_matches};
use super::signatures::{compute_signature_skeleton, tokenize_signature};

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

    pub fn from_path(path: &std::path::Path) -> Self {
        path.extension()
            .and_then(|e| e.to_str())
            .map(Self::from_extension)
            .unwrap_or(Self::Unknown)
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
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
            "ignored_parameter",
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
            "broken_doc_reference",
            "undocumented_feature",
            "stale_doc_reference",
            "compiler_warning",
            "missing_wrapper_declaration",
            "shadow_module",
            "repeated_field_pattern",
        ]
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

// ============================================================================
// Signature Consistency
// ============================================================================

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
        // Uses Rust files so the test works in CI (only rust extension/grammar installed)
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        // Two conforming files with matching signatures
        std::fs::write(
            dir.join("handlers/chat.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/webhook.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        )
        .unwrap();

        // One file with structurally different signature (different param count)
        std::fs::write(
            dir.join("handlers/ping.rs"),
            "pub fn execute(config: &Config) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string(), "register".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/chat.rs".to_string(),
                "handlers/webhook.rs".to_string(),
                "handlers/ping.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        // ping.rs should be moved to outliers
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        assert_eq!(conv.outliers[0].file, "handlers/ping.rs");
        assert!(conv.outliers[0].deviations.iter().any(|d| {
            d.kind == AuditFinding::SignatureMismatch && d.description.contains("execute")
        }));
    }

    #[test]
    fn signature_check_adds_to_existing_outliers() {
        // Uses Rust files so the test works in CI (only rust extension/grammar installed)
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/chat.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        ).unwrap();

        std::fs::write(
            dir.join("handlers/webhook.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        ).unwrap();

        // File already an outlier (missing register) AND has structurally different execute (1 param vs 2)
        std::fs::write(
            dir.join("handlers/bad.rs"),
            "pub fn execute(config: &Config) -> Result<()> { Ok(()) }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string(), "register".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/chat.rs".to_string(),
                "handlers/webhook.rs".to_string(),
            ],
            outliers: vec![Outlier {
                file: "handlers/bad.rs".to_string(),
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
        // Uses Rust files so the test works in CI
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/a.rs"),
            "pub fn execute(config: &Config) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/b.rs"),
            "pub fn execute(config: &Config) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["handlers/a.rs".to_string(), "handlers/b.rs".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert!(conv.outliers.is_empty());
        assert!((conv.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn signature_check_skips_unknown_language() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
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
    }

    #[test]
    fn signature_check_majority_wins() {
        // Uses Rust files so the test works in CI (only rust extension/grammar installed)
        // 2 files have one signature (2 params), 1 file has another (1 param) — the 2-file version is canonical
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/a.rs"),
            "pub fn run(input: &Input, context: &Context) -> bool { true }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/b.rs"),
            "pub fn run(input: &Input, context: &Context) -> bool { true }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/c.rs"),
            "pub fn run(input: &Input) -> bool { true }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["run".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/a.rs".to_string(),
                "handlers/b.rs".to_string(),
                "handlers/c.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        assert_eq!(conv.outliers[0].file, "handlers/c.rs");
    }

    #[test]
    fn signature_check_skips_ambiguous_tie() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("undo")).unwrap();

        std::fs::write(
            dir.join("undo/snapshot.rs"),
            "pub fn new(root: &Path, label: &str) -> Self { Self {} }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("undo/rollback.rs"),
            "pub fn new() -> Self { Self {} }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Undo".to_string(),
            glob: "undo/*".to_string(),
            expected_methods: vec!["new".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "undo/snapshot.rs".to_string(),
                "undo/rollback.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir);

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert!(conv.outliers.is_empty());
    }

    #[test]
    fn return_type_difference_not_a_mismatch() {
        // Files with and without return types should NOT produce a SignatureMismatch.
        // Uses Rust files so the test works in CI.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("api")).unwrap();

        std::fs::write(
            dir.join("api/users.rs"),
            "pub fn register() -> Result<()> { Ok(()) }\npub fn check(request: &Request) {}\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("api/posts.rs"),
            "pub fn register() {}\npub fn check(request: &Request) {}\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Api".to_string(),
            glob: "api/*".to_string(),
            expected_methods: vec!["register".to_string(), "check".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["api/users.rs".to_string(), "api/posts.rs".to_string()],
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
            d.kind == AuditFinding::NamespaceMismatch && d.description.contains("Missing namespace")
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
