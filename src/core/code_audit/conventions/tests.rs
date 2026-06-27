#![cfg(test)]

use super::*;

fn discover_conventions(
    group_name: &str,
    glob_pattern: &str,
    fingerprints: &[FileFingerprint],
) -> Option<Convention> {
    discover_conventions_with_config(
        group_name,
        glob_pattern,
        fingerprints,
        &AuditConfig::default(),
    )
}

fn framework_like_audit_config() -> AuditConfig {
    AuditConfig {
        utility_suffixes: vec![
            "Helper".to_string(),
            "Helpers".to_string(),
            "Constants".to_string(),
            "Categories".to_string(),
            "Sanitizer".to_string(),
            "Renderer".to_string(),
            "Validator".to_string(),
            "Verifier".to_string(),
            "Resolver".to_string(),
            "Factory".to_string(),
            "Builder".to_string(),
            "Result".to_string(),
            "Scheduling".to_string(),
        ],
        ..Default::default()
    }
}

/// Return `true` only when the Rust grammar is discoverable via the
/// extension registry.
///
/// `check_signature_consistency` → `extract_signatures_from_items` →
/// `load_grammar_for_ext("rs")` depends on the `rust` extension being
/// installed under `~/.config/homeboy/extensions/`. In CI that's
/// guaranteed, but on developer machines (or minimal dev setups that
/// only have the `wordpress` extension) it may be absent — without
/// this guard the signature-consistency tests fail with a confusing
/// assertion instead of a clear skip.
///
/// Tests that parse real Rust source via the grammar call this helper
/// and early-return when it reports `false`. `eprintln!` surfaces the
/// skip in test output so the gap is visible rather than silent.
fn rust_grammar_available() -> bool {
    crate::core::code_audit::core_fingerprint::load_grammar_for_ext("rs").is_some()
}

/// Short-circuit the calling test when the Rust grammar isn't
/// available, emitting a notice to stderr so CI output still records
/// the skip.
macro_rules! require_rust_grammar {
    ($test_name:expr) => {
        if !rust_grammar_available() {
            eprintln!(
                "skip: {} requires the `rust` extension/grammar to be installed",
                $test_name
            );
            return;
        }
    };
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
fn test_from_path() {
    assert_eq!(Language::from_path(Path::new("src/lib.rs")), Language::Rust);
    assert_eq!(
        Language::from_path(Path::new("src/app.tsx")),
        Language::TypeScript
    );
    assert_eq!(Language::from_path(Path::new("README")), Language::Unknown);
}

#[test]
fn test_all_names() {
    assert!(AuditFinding::all_names().contains(&"write_only_config_key"));
    assert!(AuditFinding::all_names().contains(&"core_boundary_leak"));
}

#[test]
fn test_discover_conventions_with_config() {
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
    ];

    let convention = discover_conventions_with_config(
        "Abilities",
        "abilities/*.php",
        &fingerprints,
        &AuditConfig::default(),
    )
    .unwrap();

    let mut expected_methods = convention.expected_methods.clone();
    expected_methods.sort();
    assert_eq!(expected_methods, vec!["execute", "register"]);
    assert_eq!(convention.conforming.len(), 2);
    assert!(convention.outliers.is_empty());
}

#[test]
fn test_check_signature_consistency() {
    let mut conventions = vec![Convention {
        name: "No Methods".to_string(),
        glob: "src/*.rs".to_string(),
        expected_methods: Vec::new(),
        expected_registrations: Vec::new(),
        expected_interfaces: Vec::new(),
        expected_namespace: None,
        expected_imports: Vec::new(),
        conforming: vec!["src/a.rs".to_string()],
        outliers: Vec::new(),
        total_files: 1,
        confidence: 1.0,
    }];

    check_signature_consistency(&mut conventions, Path::new("."), &AuditConfig::default());

    assert!(conventions[0].outliers.is_empty());
}

#[test]
fn test_member_requirement_deviation() {
    let deviation = member_requirement_deviation(
        AuditFinding::MissingMethod,
        "Missing method",
        "Add",
        "execute",
        "()",
        "Abilities",
    );

    assert_eq!(deviation.kind, AuditFinding::MissingMethod);
    assert_eq!(deviation.description, "Missing method: execute");
    assert_eq!(
        deviation.suggestion,
        "Add execute() to match the convention in Abilities"
    );
}

#[test]
fn utility_like_outlier_is_not_promoted_to_naming_mismatch() {
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

    let convention = discover_conventions_with_config(
        "Abilities",
        "abilities/*.php",
        &fingerprints,
        &framework_like_audit_config(),
    )
    .unwrap();

    assert!(
        convention.outliers.is_empty(),
        "recognized helper files are intentional utilities, got: {:?}",
        convention.outliers
    );
}

#[test]
fn generic_scaffolding_suffixes_are_not_promoted_to_naming_mismatch() {
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
            relative_path: "abilities/DeleteAbility.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string(), "register".to_string()],
            type_name: Some("DeleteAbility".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/WikiAbilityBase.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string(), "register".to_string()],
            type_name: Some("WikiAbilityBase".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/WikiActionHandlers.php".to_string(),
            language: Language::Php,
            methods: vec!["register".to_string()],
            type_name: Some("WikiActionHandlers".to_string()),
            ..Default::default()
        },
    ];

    let convention = discover_conventions("Abilities", "abilities/*.php", &fingerprints).unwrap();

    assert!(
        convention.outliers.is_empty(),
        "generic scaffolding suffixes are utility-like, got: {:?}",
        convention.outliers
    );

    let projector = FileFingerprint {
        relative_path: "handlers/McpPacketProjector.php".to_string(),
        language: Language::Php,
        methods: vec!["project".to_string()],
        type_name: Some("McpPacketProjector".to_string()),
        ..Default::default()
    };

    assert!(is_utility_like_file(&projector, &AuditConfig::default()));
}

#[test]
fn non_utility_helper_like_outlier_still_reports_naming_mismatch() {
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
            relative_path: "abilities/FlowThing.php".to_string(),
            language: Language::Php,
            methods: vec!["formatFlow".to_string()],
            type_name: Some("FlowThing".to_string()),
            ..Default::default()
        },
    ];

    let convention = discover_conventions("Abilities", "abilities/*.php", &fingerprints).unwrap();

    assert_eq!(convention.outliers.len(), 1);
    assert!(matches!(
        convention.outliers[0].deviations[0].kind,
        AuditFinding::NamingMismatch
    ));
}

#[test]
fn opaque_convention_exception_globs_suppress_group_findings() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "src/items/one.item".to_string(),
            language: Language::Unknown,
            methods: vec!["run".to_string()],
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "src/items/two.item".to_string(),
            language: Language::Unknown,
            methods: vec!["run".to_string()],
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "src/generated/fixture.item".to_string(),
            language: Language::Unknown,
            methods: vec![],
            ..Default::default()
        },
    ];
    let audit_config = AuditConfig {
        convention_exception_globs: vec!["src/generated/*".to_string()],
        ..Default::default()
    };

    let convention =
        discover_conventions_with_config("Items", "src/items/*", &fingerprints, &audit_config)
            .unwrap();

    assert!(
        convention.outliers.is_empty(),
        "configured exception globs are opaque to core and should suppress convention deviations"
    );
}

#[test]
fn declared_traits_do_not_become_missing_interfaces() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "chat/ListChatSessionsAbility.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string()],
            type_name: Some("ListChatSessionsAbility".to_string()),
            implements: vec!["ChatSessionHelpers".to_string()],
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "chat/DeleteChatSessionAbility.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string()],
            type_name: Some("DeleteChatSessionAbility".to_string()),
            implements: vec!["ChatSessionHelpers".to_string()],
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "chat/ChatSessionHelpers.php".to_string(),
            language: Language::Php,
            methods: vec!["verifySessionOwnership".to_string()],
            type_name: Some("ChatSessionHelpers".to_string()),
            content: "<?php\ntrait ChatSessionHelpers {}".to_string(),
            ..Default::default()
        },
    ];

    let convention = discover_conventions_with_config(
        "Chat",
        "chat/*.php",
        &fingerprints,
        &framework_like_audit_config(),
    )
    .unwrap();
    assert!(
        convention.expected_interfaces.is_empty(),
        "traits should not be treated as interfaces: {:?}",
        convention.expected_interfaces
    );
    assert!(
        convention
            .outliers
            .iter()
            .flat_map(|o| &o.deviations)
            .all(|d| d.kind != AuditFinding::MissingInterface),
        "declared trait should not produce MissingInterface deviations"
    );
}

#[test]
fn utility_classes_do_not_need_dispatch_registration() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "endpoints/PostsEndpoint.php".to_string(),
            language: Language::Php,
            methods: vec!["register".to_string()],
            registrations: vec!["runtime_dispatch".to_string()],
            type_name: Some("PostsEndpoint".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "endpoints/PagesEndpoint.php".to_string(),
            language: Language::Php,
            methods: vec!["register".to_string()],
            registrations: vec!["runtime_dispatch".to_string()],
            type_name: Some("PagesEndpoint".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "endpoints/SignatureVerifier.php".to_string(),
            language: Language::Php,
            methods: vec!["verify".to_string()],
            type_name: Some("SignatureVerifier".to_string()),
            ..Default::default()
        },
    ];

    let convention = discover_conventions_with_config(
        "Api",
        "endpoints/*.php",
        &fingerprints,
        &framework_like_audit_config(),
    )
    .unwrap();
    assert!(
        convention
            .outliers
            .iter()
            .flat_map(|o| &o.deviations)
            .all(|d| d.kind != AuditFinding::MissingRegistration
                && d.kind != AuditFinding::MissingMethod),
        "utility classes should not be treated as runtime endpoint registrants"
    );
}

#[test]
fn typeless_files_do_not_need_type_subject_methods_or_registration() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "abilities/CreateAbility.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string(), "register".to_string()],
            registrations: vec!["runtime_dispatch".to_string()],
            type_name: Some("CreateAbility".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/UpdateAbility.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string(), "register".to_string()],
            registrations: vec!["runtime_dispatch".to_string()],
            type_name: Some("UpdateAbility".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/DeleteAbility.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string(), "register".to_string()],
            registrations: vec!["runtime_dispatch".to_string()],
            type_name: Some("DeleteAbility".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/bootstrap.php".to_string(),
            language: Language::Php,
            methods: vec![],
            type_name: None,
            type_names: vec![],
            ..Default::default()
        },
    ];

    let convention = discover_conventions_with_config(
        "Abilities",
        "abilities/*.php",
        &fingerprints,
        &AuditConfig::default(),
    )
    .unwrap();

    assert!(
        convention.outliers.is_empty(),
        "typeless composition files should not inherit type-subject conventions: {:?}",
        convention.outliers
    );
    assert!(
        !convention
            .conforming
            .contains(&"abilities/bootstrap.php".to_string()),
        "typeless composition files are skipped rather than counted as convention members"
    );
}

#[test]
fn factories_do_not_need_methods_of_created_type() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "chat/DatabaseConversationStore.php".to_string(),
            language: Language::Php,
            methods: vec!["update_title".to_string(), "delete_session".to_string()],
            type_name: Some("DatabaseConversationStore".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "chat/MemoryConversationStore.php".to_string(),
            language: Language::Php,
            methods: vec!["update_title".to_string(), "delete_session".to_string()],
            type_name: Some("MemoryConversationStore".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "chat/ConversationStoreFactory.php".to_string(),
            language: Language::Php,
            methods: vec!["get".to_string()],
            type_name: Some("ConversationStoreFactory".to_string()),
            ..Default::default()
        },
    ];

    let convention = discover_conventions_with_config(
        "Chat",
        "chat/*.php",
        &fingerprints,
        &framework_like_audit_config(),
    )
    .unwrap();
    assert!(
        convention
            .outliers
            .iter()
            .flat_map(|o| &o.deviations)
            .all(|d| d.kind != AuditFinding::MissingMethod),
        "factories produce stores; they should not implement store methods"
    );
}

#[test]
fn test_file_helper_functions_do_not_create_missing_method_conventions() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "tests/core/stack/apply_test.rs".to_string(),
            language: Language::Rust,
            methods: vec![
                "run".to_string(),
                "init_repo".to_string(),
                "write_and_commit".to_string(),
            ],
            type_name: None,
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "tests/core/stack/status_test.rs".to_string(),
            language: Language::Rust,
            methods: vec![
                "run".to_string(),
                "init_repo".to_string(),
                "write_and_commit".to_string(),
            ],
            type_name: None,
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "tests/core/stack/spec_test.rs".to_string(),
            language: Language::Rust,
            methods: vec!["spec_round_trips".to_string()],
            type_name: None,
            ..Default::default()
        },
    ];

    let convention = discover_conventions_with_config(
        "Stack (Tests)",
        "tests/core/stack/*_test.rs",
        &fingerprints,
        &AuditConfig::default(),
    );

    assert!(
            convention.is_none(),
            "test helper functions are local scaffolding, not methods every sibling test file must define"
        );
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
    let _audit_guard = crate::test_support::AuditGuard::new();
    // Uses Rust files so the test works in CI (only rust extension/grammar installed).
    // When the grammar isn't discoverable (e.g. dev machine without the rust
    // extension installed), skip instead of failing the assertion downstream.
    require_rust_grammar!("signature_check_detects_mismatch");
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

    for _ in 0..5 {
        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());
        if conventions[0]
            .outliers
            .iter()
            .flat_map(|outlier| outlier.deviations.iter())
            .any(|d| d.kind == AuditFinding::SignatureMismatch)
        {
            break;
        }
        std::thread::yield_now();
    }

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
    let _audit_guard = crate::test_support::AuditGuard::new();
    // Uses Rust files so the test works in CI (only rust extension/grammar installed).
    require_rust_grammar!("signature_check_adds_to_existing_outliers");
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

    for _ in 0..5 {
        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());
        if conventions[0].outliers[0]
            .deviations
            .iter()
            .any(|d| d.kind == AuditFinding::SignatureMismatch)
        {
            break;
        }
        std::thread::yield_now();
    }

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
    let _audit_guard = crate::test_support::AuditGuard::new();
    // Uses Rust files so the test works in CI (only rust extension/grammar installed).
    require_rust_grammar!("signature_check_no_change_when_all_match");
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

    check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

    let conv = &conventions[0];
    assert_eq!(conv.conforming.len(), 2);
    assert!(conv.outliers.is_empty());
    assert!((conv.confidence - 1.0).abs() < f32::EPSILON);
}

#[test]
fn signature_check_skips_convention_exception_files() {
    let _audit_guard = crate::test_support::AuditGuard::new();
    require_rust_grammar!("signature_check_skips_convention_exception_files");
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

    std::fs::write(
        dir.join("handlers/register.rs"),
        "pub fn execute(config: &Config, context: &Context) -> Vec<Item> { vec![] }\n",
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
        conforming: vec![
            "handlers/a.rs".to_string(),
            "handlers/b.rs".to_string(),
            "handlers/register.rs".to_string(),
        ],
        outliers: vec![],
        total_files: 3,
        confidence: 1.0,
    }];
    let audit_config = AuditConfig {
        convention_exception_globs: vec!["**/register.rs".to_string()],
        ..Default::default()
    };

    check_signature_consistency(&mut conventions, &dir, &audit_config);

    assert!(conventions[0].outliers.is_empty());
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

    check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

    // Should not change anything for unknown language
    assert_eq!(conventions[0].conforming.len(), 2);
    assert!(conventions[0].outliers.is_empty());
}

#[test]
fn signature_check_majority_wins() {
    let _audit_guard = crate::test_support::AuditGuard::new();
    // Uses Rust files so the test works in CI (only rust extension/grammar installed).
    // 2 files have one signature (2 params), 1 file has another (1 param) — the 2-file version is canonical
    require_rust_grammar!("signature_check_majority_wins");
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

    check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

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

    check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

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

    check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

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
            namespace: Some("SamplePlugin\\Abilities\\Flow".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/UpdateFlow.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string()],
            type_name: Some("UpdateFlow".to_string()),
            namespace: Some("SamplePlugin\\Abilities\\Flow".to_string()),
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/DeleteFlow.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string()],
            type_name: Some("DeleteFlow".to_string()),
            namespace: Some("SamplePlugin\\Flow".to_string()), // WRONG namespace
            ..Default::default()
        },
    ];

    let convention = discover_conventions("Flow", "abilities/*", &fingerprints).unwrap();

    assert_eq!(
        convention.expected_namespace,
        Some("SamplePlugin\\Abilities\\Flow".to_string())
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
fn missing_import_not_flagged_for_same_namespace_reference() {
    // Regression test for #1135 (case 2).
    //
    // Two classes in the same namespace don't need `use` statements to
    // reference each other. PHP resolves unqualified same-namespace
    // references automatically.
    let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/AgentTokenAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("AgentTokenAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                // Imports PermissionHelper via fully-qualified name in the import list
                // in most files, but THIS file relies on same-namespace resolution.
                imports: vec![],
                content: "namespace SamplePlugin\\Abilities;\n\nclass AgentTokenAbilities {\n    public function register() { PermissionHelper::can_manage(); }\n}".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("FlowAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/JobAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("JobAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                ..Default::default()
            },
        ];

    let convention = discover_conventions("Abilities", "abilities/*", &fingerprints).unwrap();

    assert!(convention
        .expected_imports
        .contains(&"SamplePlugin\\Abilities\\PermissionHelper".to_string()));

    // AgentTokenAbilities references PermissionHelper (same namespace) —
    // it should NOT be flagged as a missing import.
    let agent_outlier = convention
        .outliers
        .iter()
        .find(|o| o.file == "abilities/AgentTokenAbilities.php");

    if let Some(outlier) = agent_outlier {
        assert!(
            !outlier.deviations.iter().any(|d| {
                d.kind == AuditFinding::MissingImport && d.description.contains("PermissionHelper")
            }),
            "Same-namespace reference should not be flagged as missing import. Got: {:?}",
            outlier.deviations
        );
    }
}

#[test]
fn missing_import_not_flagged_for_self_import() {
    // Regression test for #1135 (case 1).
    //
    // A file that *defines* class Foo in namespace X\Y should never be
    // flagged as needing `use X\Y\Foo;` — that's a self-import.
    let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/PermissionHelper.php".to_string(),
                language: Language::Php,
                methods: vec!["can_manage".to_string()],
                type_name: Some("PermissionHelper".to_string()),
                type_names: vec!["PermissionHelper".to_string()],
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                // File defines the class; its convention peers might import it,
                // but self-import is nonsensical.
                imports: vec![],
                content: "namespace SamplePlugin\\Abilities;\n\nclass PermissionHelper { public function can_manage() {} }".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["can_manage".to_string()],
                type_name: Some("FlowAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                content: "use SamplePlugin\\Abilities\\PermissionHelper;".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/JobAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["can_manage".to_string()],
                type_name: Some("JobAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                content: "use SamplePlugin\\Abilities\\PermissionHelper;".to_string(),
                ..Default::default()
            },
        ];

    let convention = discover_conventions("Abilities", "abilities/*", &fingerprints).unwrap();

    let helper_outlier = convention
        .outliers
        .iter()
        .find(|o| o.file == "abilities/PermissionHelper.php");

    if let Some(outlier) = helper_outlier {
        assert!(
            !outlier.deviations.iter().any(|d| {
                d.kind == AuditFinding::MissingImport && d.description.contains("PermissionHelper")
            }),
            "Self-import should not be flagged. Got deviations: {:?}",
            outlier.deviations
        );
    }
}

#[test]
fn missing_import_not_flagged_when_terminal_only_appears_in_namespace() {
    let fingerprints = vec![
            FileFingerprint {
                relative_path: "core/agents/AgentBundler.php".to_string(),
                language: Language::Php,
                methods: vec!["__construct".to_string()],
                type_name: Some("AgentBundler".to_string()),
                namespace: Some("SamplePlugin\\Core\\Agents".to_string()),
                imports: vec!["SamplePlugin\\Core\\Database\\Agents\\Agents".to_string()],
                content: "namespace SamplePlugin\\Core\\Agents;\nuse SamplePlugin\\Core\\Database\\Agents\\Agents;\nclass AgentBundler { public function __construct() { new Agents(); } }".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "core/agents/AgentIdentityResolver.php".to_string(),
                language: Language::Php,
                methods: vec!["__construct".to_string()],
                type_name: Some("AgentIdentityResolver".to_string()),
                namespace: Some("SamplePlugin\\Core\\Agents".to_string()),
                imports: vec!["SamplePlugin\\Core\\Database\\Agents\\Agents".to_string()],
                content: "namespace SamplePlugin\\Core\\Agents;\nuse SamplePlugin\\Core\\Database\\Agents\\Agents;\nclass AgentIdentityResolver { public function __construct() { new Agents(); } }".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "core/agents/AgentIdentity.php".to_string(),
                language: Language::Php,
                methods: vec!["__construct".to_string()],
                type_name: Some("AgentIdentity".to_string()),
                namespace: Some("SamplePlugin\\Core\\Agents".to_string()),
                imports: vec![],
                content: "namespace SamplePlugin\\Core\\Agents;\nclass AgentIdentity { public function __construct() {} }".to_string(),
                ..Default::default()
            },
        ];

    let convention = discover_conventions("Agents", "core/agents/*", &fingerprints).unwrap();

    assert!(convention
        .expected_imports
        .contains(&"SamplePlugin\\Core\\Database\\Agents\\Agents".to_string()));

    let identity_outlier = convention
        .outliers
        .iter()
        .find(|o| o.file == "core/agents/AgentIdentity.php");

    if let Some(outlier) = identity_outlier {
        assert!(
                !outlier.deviations.iter().any(|d| {
                    d.kind == AuditFinding::MissingImport && d.description.contains("Agents")
                }),
                "Namespace-only terminal segment should not be flagged as missing import. Got deviations: {:?}",
                outlier.deviations
            );
    }
}

#[test]
fn missing_import_detected_in_convention() {
    let fingerprints = vec![
        FileFingerprint {
            relative_path: "abilities/A.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string()],
            imports: vec!["SamplePlugin\\Core\\Base".to_string()],
            ..Default::default()
        },
        FileFingerprint {
            relative_path: "abilities/B.php".to_string(),
            language: Language::Php,
            methods: vec!["execute".to_string()],
            imports: vec!["SamplePlugin\\Core\\Base".to_string()],
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
        .contains(&"SamplePlugin\\Core\\Base".to_string()));
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
