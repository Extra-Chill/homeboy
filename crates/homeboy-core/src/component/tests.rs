#![cfg(test)]

use super::*;
use std::collections::HashMap;
use std::path::Path;

fn with_isolated_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    crate::test_support::with_isolated_home(|home| f(home.path()))
}

fn write_extension_fixture(home: &Path, id: &str, deploy_json: &str) {
    let dir = home.join(".config/homeboy/extensions").join(id);
    std::fs::create_dir_all(&dir).expect("extension dir");
    std::fs::write(
        dir.join(format!("{}.json", id)),
        format!(
            r#"{{
  "name": "{} extension",
  "version": "1.0.0",
  "deploy": {}
}}"#,
            id, deploy_json
        ),
    )
    .expect("extension manifest");
}

#[test]
fn validate_version_target_conflict_different_pattern_errors() {
    let existing = vec![VersionTarget {
        file: "component.meta".to_string(),
        pattern: Some("version=(.*)".to_string()),
        artifact_path: None,
    }];

    let result =
        validate_version_target_conflict(&existing, "component.meta", "build=(.*)", "test-comp");
    // Multiple targets per file with different patterns are now allowed
    // (e.g. release version + build metadata in the same file).
    assert!(result.is_ok());
}

#[test]
fn component_lifecycle_defaults_to_active_and_is_omitted_when_serialized() {
    let component = Component::new(
        "sample-component".to_string(),
        "/tmp/sample-component".to_string(),
        "remote/components/sample-component".to_string(),
        None,
    );

    assert_eq!(component.lifecycle, ComponentLifecycle::Active);
    assert!(component.is_active_lifecycle());
    assert!(component.lifecycle_suppression_reason().is_none());

    // Active is the default and must not be written into config.
    let json = serde_json::to_value(&component).unwrap();
    assert!(json.get("lifecycle").is_none());
    assert!(json.get("bundled_into").is_none());
}

#[test]
fn component_lifecycle_bundled_roundtrips_and_suppresses() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "shared-runtime",
        "lifecycle": "bundled",
        "bundled_into": "host-app"
    }))
    .unwrap();

    assert_eq!(component.lifecycle, ComponentLifecycle::Bundled);
    assert!(!component.is_active_lifecycle());
    assert_eq!(
        component.bundled_into.as_deref(),
        Some("host-app"),
        "bundled_into host should be preserved"
    );
    assert_eq!(
        component.lifecycle_suppression_reason().as_deref(),
        Some("Component is bundled into 'host-app'")
    );

    // Roundtrip must preserve the lifecycle marker so config is stable.
    let json = serde_json::to_value(&component).unwrap();
    assert_eq!(json["lifecycle"], serde_json::json!("bundled"));
    assert_eq!(json["bundled_into"], serde_json::json!("host-app"));
    let reparsed: Component = serde_json::from_value(json).unwrap();
    assert_eq!(reparsed.lifecycle, ComponentLifecycle::Bundled);
}

#[test]
fn component_lifecycle_retired_is_not_active() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "old-component",
        "lifecycle": "retired"
    }))
    .unwrap();

    assert_eq!(component.lifecycle, ComponentLifecycle::Retired);
    assert!(!component.is_active_lifecycle());
    assert!(component.bundled_into.is_none());
    assert_eq!(
        component.lifecycle_suppression_reason().as_deref(),
        Some("Component is retired")
    );
}

#[test]
fn component_lifecycle_unknown_value_is_rejected() {
    // A typo in the lifecycle should fail loudly rather than silently
    // defaulting to active (which would re-expose deploy drift).
    let result: std::result::Result<Component, _> = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "lifecycle": "archived"
    }));
    assert!(result.is_err());
}

#[test]
fn component_priority_labels_serialization_roundtrip() {
    let mut component = Component::new(
        "sample-component".to_string(),
        "/tmp/sample-component".to_string(),
        "remote/components/sample-component".to_string(),
        None,
    );
    component.priority_labels = Some(vec!["urgent".to_string()]);

    let json = serde_json::to_string(&component).unwrap();
    let parsed: Component = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.priority_labels, Some(vec!["urgent".to_string()]));
}

#[test]
fn package_coverage_normalizes_paths_and_rejects_malformed_declarations() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "release": {
            "package_coverage": [{
                "artifact": " ./dist//archive.zip/ ",
                "source_roots": [" ./source//runtime/ "],
                "archive_root": " ./bundle// "
            }]
        }
    }))
    .expect("valid package coverage should parse");
    component
        .release
        .validate_package_coverage()
        .expect("valid package coverage should validate");
    let coverage = &component.release.package_coverage[0];
    assert_eq!(coverage.artifact, "dist/archive.zip");
    assert_eq!(coverage.source_roots, ["source/runtime"]);
    assert_eq!(coverage.archive_root, "bundle");

    let malformed: Component = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "release": {
            "package_coverage": [{
                "artifact": "dist/archive.zip",
                "source_roots": [],
                "archive_root": "bundle"
            }]
        }
    }))
    .expect("shape remains parseable for actionable validation");
    assert!(malformed.release.validate_package_coverage().is_err());

    let backslash_traversal: std::result::Result<Component, _> =
        serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "release": {
                "package_coverage": [{
                    "artifact": "dist/archive.zip",
                    "source_roots": ["source\\..\\outside"],
                    "archive_root": "bundle"
                }]
            }
        }));
    assert!(backslash_traversal.is_err());

    let windows_absolute: std::result::Result<Component, _> =
        serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "release": {
                "package_coverage": [{
                    "artifact": "C:\\build\\archive.zip",
                    "source_roots": ["source"],
                    "archive_root": "bundle"
                }]
            }
        }));
    assert!(windows_absolute.is_err());

    let unc_absolute: std::result::Result<Component, _> =
        serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "release": {
                "package_coverage": [{
                    "artifact": "\\\\server\\share\\archive.zip",
                    "source_roots": ["source"],
                    "archive_root": "bundle"
                }]
            }
        }));
    assert!(unc_absolute.is_err());

    let dot_overlap: Component = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "release": {
            "package_coverage": [{
                "artifact": "dist/archive.zip",
                "source_roots": [".", "source"],
                "archive_root": "bundle"
            }]
        }
    }))
    .expect("canonical paths should parse");
    assert!(dot_overlap.release.validate_package_coverage().is_err());

    let direct_valid = ComponentReleaseConfig {
        package_coverage: vec![PackageCoverageConfig {
            artifact: "dist/archive.zip".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec!["source/runtime".to_string()],
            archive_root: "bundle".to_string(),
        }],
        ..Default::default()
    };
    direct_valid
        .validate_package_coverage()
        .expect("canonical direct configuration should validate");

    for (artifact, source_root, archive_root) in [
        ("../archive.zip", "source", "bundle"),
        ("/archive.zip", "source", "bundle"),
        ("C:\\archive.zip", "source", "bundle"),
        ("\\\\server\\share\\archive.zip", "source", "bundle"),
        ("dist/archive.zip", "source\\..\\outside", "bundle"),
        ("dist/archive.zip", "source", "\\bundle"),
    ] {
        let direct = ComponentReleaseConfig {
            package_coverage: vec![PackageCoverageConfig {
                artifact: artifact.to_string(),
                artifact_match: PackageCoverageArtifactMatch::Exact,
                source_roots: vec![source_root.to_string()],
                archive_root: archive_root.to_string(),
            }],
            ..Default::default()
        };
        assert!(direct.validate_package_coverage().is_err());
    }

    let noncanonical_direct = ComponentReleaseConfig {
        package_coverage: vec![PackageCoverageConfig {
            artifact: " ./dist//archive.zip/ ".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec![" ./source//runtime/ ".to_string()],
            archive_root: " ./bundle// ".to_string(),
        }],
        ..Default::default()
    };
    let error = noncanonical_direct
        .validate_package_coverage()
        .expect_err("direct values must already be canonical");
    assert_eq!(error.code.as_str(), "validation.invalid_argument");
    assert!(error.message.contains("canonical slash-separated form"));
}

#[test]
fn component_env_serialization_roundtrip() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "env": {
            "CARGO_TARGET_DIR": "/tmp/homeboy/cargo-target",
            "SHARED_CACHE_DIR": "/tmp/homeboy/shared-cache"
        }
    }))
    .unwrap();

    assert_eq!(
        component.env.get("CARGO_TARGET_DIR").map(String::as_str),
        Some("/tmp/homeboy/cargo-target")
    );

    let json = serde_json::to_value(&component).unwrap();
    assert_eq!(
        json["env"]["SHARED_CACHE_DIR"],
        serde_json::json!("/tmp/homeboy/shared-cache")
    );
}

#[test]
fn component_ignores_changelog_targets_alias() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "changelog_targets": "CHANGELOG.md"
    }))
    .unwrap();

    assert!(component.changelog_target.is_none());
}

#[test]
fn validate_version_target_conflict_same_pattern_ok() {
    let existing = vec![VersionTarget {
        file: "component.meta".to_string(),
        pattern: Some("version=(.*)".to_string()),
        artifact_path: None,
    }];

    let result =
        validate_version_target_conflict(&existing, "component.meta", "version=(.*)", "test-comp");
    assert!(result.is_ok());
}

#[test]
fn validate_version_target_conflict_different_file_ok() {
    let existing = vec![VersionTarget {
        file: "component.meta".to_string(),
        pattern: Some("version=(.*)".to_string()),
        artifact_path: None,
    }];

    let result = validate_version_target_conflict(
        &existing,
        "package.json",
        "\"version\": \"(.*)\"",
        "test-comp",
    );
    assert!(result.is_ok());
}

#[test]
fn validate_version_target_conflict_empty_existing_ok() {
    let existing: Vec<VersionTarget> = vec![];

    let result =
        validate_version_target_conflict(&existing, "component.meta", "version=(.*)", "test-comp");
    assert!(result.is_ok());
}

#[test]
fn validate_version_pattern_rejects_template_syntax() {
    let result = validate_version_pattern("version={version}");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.details.to_string().contains("template syntax"));
}

#[test]
fn validate_version_pattern_rejects_no_capture_group() {
    let result = validate_version_pattern(r"version=\d+\.\d+\.\d+");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.details.to_string().contains("no capture group"));
}

#[test]
fn validate_version_pattern_rejects_invalid_regex() {
    let result = validate_version_pattern(r"version=(\d+\.\d+");
    assert!(result.is_err());
}

#[test]
fn validate_version_pattern_accepts_valid_pattern() {
    assert!(validate_version_pattern(r"version:\s*(\d+\.\d+\.\d+)").is_ok());
}

#[test]
fn parse_version_targets_rejects_template_syntax() {
    let targets = vec!["component.meta::version={version}".to_string()];
    let result = parse_version_targets(&targets);
    assert!(result.is_err());
}

#[test]
fn normalize_version_pattern_converts_double_escaped() {
    // Pattern with double-escaped backslashes (as stored in config)
    let double_escaped = r"version:\\s*(\\d+\\.\\d+\\.\\d+)";
    let normalized = normalize_version_pattern(double_escaped);
    assert_eq!(normalized, r"version:\s*(\d+\.\d+\.\d+)");

    // Pattern already correct should stay the same
    let correct = r"version:\s*(\d+\.\d+\.\d+)";
    let normalized2 = normalize_version_pattern(correct);
    assert_eq!(normalized2, r"version:\s*(\d+\.\d+\.\d+)");
}

#[test]
fn parse_version_targets_normalizes_double_escaped_patterns() {
    // Simulate pattern stored with double-escaped backslashes
    let targets = vec!["component.meta::version:\\s*(\\d+\\.\\d+\\.\\d+)".to_string()];
    let result = parse_version_targets(&targets).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file, "component.meta");
    assert_eq!(
        result[0].pattern.as_ref().unwrap(),
        r"version:\s*(\d+\.\d+\.\d+)"
    );
}

// ========================================================================
// Auto-resolve remote_path tests
// ========================================================================

#[test]
fn auto_resolve_remote_path_uses_extension_rule() {
    with_isolated_home(|home| {
        write_extension_fixture(
            home,
            "example",
            r#"{
"remote_path_inference": [
  {
    "when_file_contains": { "file": "{{dir_name}}.txt", "text": "Deployable" },
    "remote_path": "remote/{{dir_name}}"
  }
]
  }"#,
        );

        let dir = home.join("my-component");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("my-component.txt"), "Deployable component").unwrap();

        let component = Component {
            id: "my-component".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "example".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        assert_eq!(
            crate::component::auto_resolve_remote_path(&component),
            Some("remote/my-component".to_string()),
        );
    });
}

#[test]
fn auto_resolve_remote_path_uses_dirname_not_component_id() {
    with_isolated_home(|home| {
        write_extension_fixture(
            home,
            "example",
            r#"{
"remote_path_inference": [
  {
    "when_file_contains": { "file": "marker.txt", "text": "Deployable" },
    "remote_path": "remote/{{dir_name}}"
  }
]
  }"#,
        );

        let dir = home.join("source-dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "Deployable component").unwrap();

        let component = Component {
            id: "component-id".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "example".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        assert_eq!(
            crate::component::auto_resolve_remote_path(&component),
            Some("remote/source-dir".to_string()),
        );
    });
}

#[test]
fn auto_resolve_remote_path_returns_none_without_matching_extension_rule() {
    let component = Component {
        id: "my-crate".to_string(),
        local_path: "/tmp".to_string(),
        extensions: Some(HashMap::from([(
            "rust".to_string(),
            ScopedExtensionConfig::default(),
        )])),
        ..Component::default()
    };

    assert_eq!(crate::component::auto_resolve_remote_path(&component), None);
}

#[test]
fn auto_resolve_remote_path_returns_none_on_conflicting_extension_rules() {
    with_isolated_home(|home| {
        let rule = |path: &str| {
            format!(
                r#"{{
"remote_path_inference": [
  {{
    "when_file_contains": {{ "file": "marker.txt", "text": "Deployable" }},
    "remote_path": "{}"
  }}
]
  }}"#,
                path
            )
        };
        write_extension_fixture(home, "alpha", &rule("remote/alpha/{{dir_name}}"));
        write_extension_fixture(home, "beta", &rule("remote/beta/{{dir_name}}"));

        let dir = home.join("my-component");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "Deployable component").unwrap();

        let component = Component {
            id: "my-component".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([
                ("alpha".to_string(), ScopedExtensionConfig::default()),
                ("beta".to_string(), ScopedExtensionConfig::default()),
            ])),
            ..Component::default()
        };

        assert_eq!(crate::component::auto_resolve_remote_path(&component), None);
    });
}

#[test]
fn resolve_remote_path_fills_empty() {
    with_isolated_home(|home| {
        write_extension_fixture(
            home,
            "example",
            r#"{
"remote_path_inference": [
  {
    "when_file_contains": { "file": "marker.txt", "text": "Deployable" },
    "remote_path": "remote/{{dir_name}}"
  }
]
  }"#,
        );

        let dir = home.join("my-component");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("marker.txt"), "Deployable component").unwrap();

        let mut component = Component {
            id: "my-component".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            remote_path: String::new(),
            extensions: Some(HashMap::from([(
                "example".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Component::default()
        };

        crate::component::resolve_remote_path(&mut component);
        assert_eq!(component.remote_path, "remote/my-component");
    });
}

#[test]
fn resolve_remote_path_preserves_explicit_value() {
    let mut component = Component {
        id: "my-component".to_string(),
        local_path: "/tmp".to_string(),
        remote_path: "custom/deploy/path".to_string(),
        extensions: Some(HashMap::from([(
            "example".to_string(),
            ScopedExtensionConfig::default(),
        )])),
        ..Component::default()
    };

    crate::component::resolve_remote_path(&mut component);
    assert_eq!(component.remote_path, "custom/deploy/path");
}

// ========================================================================
// Portable config discovery tests
// ========================================================================

#[test]
fn discover_from_portable_creates_component_from_homeboy_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();

    let config = serde_json::json!({
        "id": "test-discover",
        "version_targets": [{"file": "Cargo.toml", "pattern": "(?m)^version\\s*=\\s*\"([0-9.]+)\""}],
        "changelog_target": "docs/CHANGELOG.md",
        "extensions": {"rust": {}}
    });
    std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

    let result = discover_from_portable(&dir);
    assert!(
        result.is_some(),
        "Should discover component from homeboy.json"
    );

    let comp = result.unwrap();
    assert_eq!(comp.id, "test-discover");
    assert_eq!(comp.local_path, dir.to_string_lossy());
    assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
    assert!(comp
        .extensions
        .as_ref()
        .is_some_and(|m| m.contains_key("rust")));
    assert!(comp.version_targets.is_some());
    assert!(comp.remote_path.is_empty()); // default
}

#[test]
fn discover_from_portable_returns_none_without_homeboy_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    // No homeboy.json in the temp dir

    let result = discover_from_portable(&dir);
    assert!(result.is_none());
}

#[test]
fn discover_from_portable_ignores_machine_specific_in_portable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();

    let config = serde_json::json!({
        "id": "test-machine-fields",
        "local_path": "/wrong/path",
        "remote_path": "/also/wrong",
        "extract_command": "tar -xf artifact.tar.gz"
    });
    std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

    let comp = discover_from_portable(&dir).unwrap();
    // id comes from portable JSON
    assert_eq!(comp.id, "test-machine-fields");
    // local_path is derived from actual dir, overriding the portable value
    assert_eq!(comp.local_path, dir.to_string_lossy());
    // remote_path from portable is preserved
    assert_eq!(comp.remote_path, "/also/wrong");
    assert_eq!(
        comp.extract_command.as_deref(),
        Some("tar -xf artifact.tar.gz")
    );
}

#[test]
fn discover_from_portable_with_baselines_and_extensions() {
    // Mirrors a real homeboy.json — includes subsystem-owned
    // baselines and component-owned extensions. This must not silently fail.
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();

    let config = serde_json::json!({
        "auto_cleanup": false,
        "baselines": {
            "lint": {
                "context_id": "sample-component",
                "created_at": "2026-03-06T04:47:29Z",
                "item_count": 0,
                "known_fingerprints": [],
                "metadata": {
                    "findings_count": 0
                }
            }
        },
        "changelog_target": "docs/CHANGELOG.md",
        "extensions": {
            "example": {}
        },
        "id": "sample-component",
        "version_targets": [
            {"file": "sample-component.meta", "pattern": "(?m)^version=([0-9.]+)"}
        ]
    });
    std::fs::write(dir.join("homeboy.json"), config.to_string()).unwrap();

    let result = discover_from_portable(&dir);
    assert!(
        result.is_some(),
        "Should discover component even with baselines field in homeboy.json"
    );

    let comp = result.unwrap();
    // id comes from portable JSON
    assert_eq!(comp.id, "sample-component");
    assert_eq!(comp.local_path, dir.to_string_lossy());
    // extensions must be present
    assert!(
        comp.extensions.is_some(),
        "extensions should be set from portable config"
    );
    assert!(
        comp.extensions.as_ref().unwrap().contains_key("example"),
        "example extension should be present"
    );
    assert_eq!(comp.changelog_target.as_deref(), Some("docs/CHANGELOG.md"));
    assert!(comp.version_targets.is_some());
}

#[test]
fn scoped_extension_config_captures_flat_settings() {
    // Flat keys (the current convention in homeboy.json) must be captured
    // as settings — not silently dropped.
    let json = serde_json::json!({
        "database_type": "mysql",
        "mysql_host": "localhost",
        "mysql_user": "root"
    });

    let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
    assert_eq!(
        config
            .settings
            .get("database_type")
            .and_then(|v| v.as_str()),
        Some("mysql")
    );
    assert_eq!(
        config.settings.get("mysql_host").and_then(|v| v.as_str()),
        Some("localhost")
    );
    assert_eq!(
        config.settings.get("mysql_user").and_then(|v| v.as_str()),
        Some("root")
    );
    assert!(config.version.is_none());
}

#[test]
fn scoped_extension_config_captures_nested_settings() {
    let json = serde_json::json!({
        "version": ">=2.0.0",
        "settings": {
            "database_type": "mysql",
            "mysql_host": "localhost"
        }
    });

    let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
    assert_eq!(config.version.as_deref(), Some(">=2.0.0"));
    assert_eq!(
        config
            .settings
            .get("database_type")
            .and_then(|v| v.as_str()),
        Some("mysql")
    );
    assert_eq!(
        config.settings.get("mysql_host").and_then(|v| v.as_str()),
        Some("localhost")
    );
    assert!(config.settings.get("settings").is_none());
}

#[test]
fn scoped_extension_config_flat_settings_override_nested_settings() {
    let json = serde_json::json!({
        "settings": {
            "database_type": "mysql"
        },
        "mysql_host": "localhost",
        "database_type": "sqlite"
    });

    let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
    assert_eq!(
        config
            .settings
            .get("database_type")
            .and_then(|v| v.as_str()),
        Some("sqlite"),
        "flat keys are canonical and must not be overwritten by nested settings"
    );
    assert_eq!(
        config.settings.get("mysql_host").and_then(|v| v.as_str()),
        Some("localhost")
    );
    assert!(config.settings.get("settings").is_none());
}

#[test]
fn component_homeboy_json_routes_nested_extension_test_backend_setting() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "roadie",
        "local_path": "/tmp/roadie",
        "extensions": {
            "example": {
                "settings": {
                    "test_backend": "host-smoke"
                }
            }
        }
    }))
    .unwrap();

    let extension_settings = component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("example"))
        .expect("example extension config")
        .settings
        .clone();

    assert_eq!(
        extension_settings
            .get("test_backend")
            .and_then(|value| value.as_str()),
        Some("host-smoke")
    );
    assert!(extension_settings.get("settings").is_none());
}

#[test]
fn scoped_extension_config_serializes_flat_settings() {
    let config = ScopedExtensionConfig {
        version: Some(">=2.0.0".to_string()),
        settings: HashMap::from([("package_manager".to_string(), serde_json::json!("pnpm"))]),
    };

    let json = serde_json::to_value(config).unwrap();
    assert_eq!(json["version"], serde_json::json!(">=2.0.0"));
    assert_eq!(json["package_manager"], serde_json::json!("pnpm"));
    assert!(json.get("settings").is_none());
}

#[test]
fn scoped_extension_config_empty_object() {
    let json = serde_json::json!({});
    let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
    assert!(config.version.is_none());
    assert!(config.settings.is_empty());
}

#[test]
fn scoped_extension_config_version_only() {
    let json = serde_json::json!({ "version": "^1.0" });
    let config: ScopedExtensionConfig = serde_json::from_value(json).unwrap();
    assert_eq!(config.version.as_deref(), Some("^1.0"));
    assert!(config.settings.is_empty());
}
