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
        file: "plugin.php".to_string(),
        pattern: Some("Version: (.*)".to_string()),
        artifact_path: None,
    }];

    let result = validate_version_target_conflict(
        &existing,
        "plugin.php",
        "define('VER', '(.*)')",
        "test-comp",
    );
    // Multiple targets per file with different patterns are now allowed
    // (e.g. plugin header Version: + PHP define() constant in same file)
    assert!(result.is_ok());
}

#[test]
fn component_lifecycle_defaults_to_active_and_is_omitted_when_serialized() {
    let component = Component::new(
        "sample-plugin".to_string(),
        "/tmp/sample-plugin".to_string(),
        "wp-content/plugins/sample-plugin".to_string(),
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
        "id": "old-plugin",
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
        "sample-plugin".to_string(),
        "/tmp/sample-plugin".to_string(),
        "wp-content/plugins/sample-plugin".to_string(),
        None,
    );
    component.priority_labels = Some(vec!["urgent".to_string()]);

    let json = serde_json::to_string(&component).unwrap();
    let parsed: Component = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.priority_labels, Some(vec!["urgent".to_string()]));
}

#[test]
fn component_deploy_config_reads_legacy_flat_fields() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "sample-plugin",
        "local_path": "/repo/sample-plugin",
        "remote_path": "wp-content/plugins/sample-plugin",
        "build_artifact": "dist/sample-plugin.zip",
        "extract_command": "unzip -o dist/sample-plugin.zip",
        "remote_owner": "www-data",
        "deploy_strategy": "git",
        "git_deploy": {
            "remote": "upstream",
            "branch": "stable",
            "post_pull": ["wp cache flush"],
            "tag_pattern": "v{{version}}"
        },
        "remote_url": "https://github.com/example/sample-plugin.git",
        "cli_path": "lando wp",
        "artifact_inputs": [{
            "component": "builder",
            "artifact": "zip",
            "target": "dist/sample-plugin.zip",
            "sha256": "abc123"
        }],
        "cleanup_artifacts": [{
            "label": "package",
            "path": "dist/sample-plugin.zip"
        }]
    }))
    .unwrap();

    let deploy = component.deploy_config();
    assert_eq!(deploy.local_path, "/repo/sample-plugin");
    assert_eq!(deploy.remote_path, "wp-content/plugins/sample-plugin");
    assert_eq!(deploy.build_artifact, Some("dist/sample-plugin.zip"));
    assert_eq!(
        deploy.extract_command,
        Some("unzip -o dist/sample-plugin.zip")
    );
    assert_eq!(deploy.remote_owner, Some("www-data"));
    assert_eq!(deploy.deploy_strategy, Some("git"));
    assert!(deploy.is_git_deploy());
    assert_eq!(component.deploy_strategy(), Some("git"));
    assert_eq!(
        component.remote_url(),
        Some("https://github.com/example/sample-plugin.git")
    );
    assert_eq!(deploy.cli_path, Some("lando wp"));
    assert_eq!(deploy.artifact_inputs.len(), 1);
    assert_eq!(deploy.artifact_inputs[0].component, "builder");
    assert_eq!(deploy.cleanup_artifacts.len(), 1);
    assert_eq!(deploy.cleanup_artifacts[0].label, "package");

    let git_deploy = component.git_deploy_config().expect("git deploy config");
    assert_eq!(git_deploy.remote, "upstream");
    assert_eq!(git_deploy.branch, "stable");
    assert_eq!(git_deploy.post_pull, vec!["wp cache flush".to_string()]);
    assert_eq!(git_deploy.tag_pattern.as_deref(), Some("v{{version}}"));
}

#[test]
fn component_deploy_config_serializes_as_legacy_flat_fields() {
    let mut component = Component::new(
        "sample-plugin".to_string(),
        "/repo/sample-plugin".to_string(),
        "wp-content/plugins/sample-plugin".to_string(),
        Some("dist/sample-plugin.zip".to_string()),
    );
    component.deploy_strategy = Some("git".to_string());
    component.git_deploy = Some(GitDeployConfig {
        remote: "upstream".to_string(),
        branch: "stable".to_string(),
        post_pull: vec!["wp cache flush".to_string()],
        tag_pattern: Some("v{{version}}".to_string()),
    });
    component.remote_url = Some("https://github.com/example/sample-plugin.git".to_string());

    let json = serde_json::to_value(&component).unwrap();
    assert!(json.get("deploy").is_none());
    assert_eq!(json["deploy_strategy"], serde_json::json!("git"));
    assert_eq!(
        json["build_artifact"],
        serde_json::json!("dist/sample-plugin.zip")
    );
    assert_eq!(
        json["remote_url"],
        serde_json::json!("https://github.com/example/sample-plugin.git")
    );
    assert_eq!(json["git_deploy"]["remote"], serde_json::json!("upstream"));
    assert_eq!(json["git_deploy"]["branch"], serde_json::json!("stable"));

    let reparsed: Component = serde_json::from_value(json).unwrap();
    let deploy = reparsed.deploy_config();
    assert_eq!(deploy.deploy_strategy, Some("git"));
    assert!(deploy.is_git_deploy());
    assert_eq!(deploy.build_artifact, Some("dist/sample-plugin.zip"));
}

#[test]
fn component_ignores_legacy_hook_fields() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "fixture",
        "pre_version_bump_commands": ["cargo build"],
        "post_version_bump_commands": ["cargo test"],
        "post_release_commands": ["echo done"],
        "hooks": {
            "post:deploy": ["wp cache flush"]
        }
    }))
    .unwrap();

    assert!(component.hooks.get("pre:version:bump").is_none());
    assert!(component.hooks.get("post:version:bump").is_none());
    assert!(component.hooks.get("post:release").is_none());
    assert_eq!(
        component.hooks.get("post:deploy"),
        Some(&vec!["wp cache flush".to_string()])
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
        file: "plugin.php".to_string(),
        pattern: Some("Version: (.*)".to_string()),
        artifact_path: None,
    }];

    let result =
        validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
    assert!(result.is_ok());
}

#[test]
fn validate_version_target_conflict_different_file_ok() {
    let existing = vec![VersionTarget {
        file: "plugin.php".to_string(),
        pattern: Some("Version: (.*)".to_string()),
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
        validate_version_target_conflict(&existing, "plugin.php", "Version: (.*)", "test-comp");
    assert!(result.is_ok());
}

#[test]
fn validate_version_pattern_rejects_template_syntax() {
    let result = validate_version_pattern("Version: {version}");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.details.to_string().contains("template syntax"));
}

#[test]
fn validate_version_pattern_rejects_no_capture_group() {
    let result = validate_version_pattern(r"Version: \d+\.\d+\.\d+");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.details.to_string().contains("no capture group"));
}

#[test]
fn validate_version_pattern_rejects_invalid_regex() {
    let result = validate_version_pattern(r"Version: (\d+\.\d+");
    assert!(result.is_err());
}

#[test]
fn validate_supported_build_config_rejects_legacy_build_command() {
    let component = Component {
        id: "sample-extension".to_string(),
        build_command: Some("npm run package:browser-extension".to_string()),
        ..Default::default()
    };

    let err = component
        .validate_supported_build_config()
        .expect_err("legacy build_command should be unsupported");

    assert!(err.message.contains("unsupported legacy build_command"));
    assert!(err.message.contains("Use scripts.build instead"));
    assert_eq!(err.details["field"].as_str(), Some("build_command"));
    assert!(err.details["tried"].to_string().contains("scripts"));
}

#[test]
fn validate_version_pattern_accepts_valid_pattern() {
    assert!(validate_version_pattern(r"Version:\s*(\d+\.\d+\.\d+)").is_ok());
}

#[test]
fn parse_version_targets_rejects_template_syntax() {
    let targets = vec!["style.css::Version: {version}".to_string()];
    let result = parse_version_targets(&targets);
    assert!(result.is_err());
}

#[test]
fn normalize_version_pattern_converts_double_escaped() {
    // Pattern with double-escaped backslashes (as stored in config)
    let double_escaped = r"Version:\\s*(\\d+\\.\\d+\\.\\d+)";
    let normalized = normalize_version_pattern(double_escaped);
    assert_eq!(normalized, r"Version:\s*(\d+\.\d+\.\d+)");

    // Pattern already correct should stay the same
    let correct = r"Version:\s*(\d+\.\d+\.\d+)";
    let normalized2 = normalize_version_pattern(correct);
    assert_eq!(normalized2, r"Version:\s*(\d+\.\d+\.\d+)");
}

#[test]
fn parse_version_targets_normalizes_double_escaped_patterns() {
    // Simulate pattern stored with double-escaped backslashes
    let targets = vec!["plugin.php::Version:\\s*(\\d+\\.\\d+\\.\\d+)".to_string()];
    let result = parse_version_targets(&targets).unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file, "plugin.php");
    assert_eq!(
        result[0].pattern.as_ref().unwrap(),
        r"Version:\s*(\d+\.\d+\.\d+)"
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
            component.auto_resolve_remote_path(),
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
            component.auto_resolve_remote_path(),
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

    assert_eq!(component.auto_resolve_remote_path(), None);
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

        assert_eq!(component.auto_resolve_remote_path(), None);
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

        component.resolve_remote_path();
        assert_eq!(component.remote_path, "remote/my-component");
    });
}

#[test]
fn resolve_remote_path_preserves_explicit_value() {
    let mut component = Component {
        id: "my-plugin".to_string(),
        local_path: "/tmp".to_string(),
        remote_path: "custom/deploy/path".to_string(),
        extensions: Some(HashMap::from([(
            "wordpress".to_string(),
            ScopedExtensionConfig::default(),
        )])),
        ..Component::default()
    };

    component.resolve_remote_path();
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
                "context_id": "sample-plugin",
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
            "wordpress": {}
        },
        "id": "sample-plugin",
        "version_targets": [
            {"file": "sample-plugin.php", "pattern": "(?m)^\\s*\\*?\\s*Version:\\s*([0-9.]+)"}
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
    assert_eq!(comp.id, "sample-plugin");
    assert_eq!(comp.local_path, dir.to_string_lossy());
    // extensions must be present
    assert!(
        comp.extensions.is_some(),
        "extensions should be set from portable config"
    );
    assert!(
        comp.extensions.as_ref().unwrap().contains_key("wordpress"),
        "wordpress extension should be present"
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
fn component_homeboy_json_routes_nested_wordpress_test_backend_setting() {
    let component: Component = serde_json::from_value(serde_json::json!({
        "id": "roadie",
        "local_path": "/tmp/roadie",
        "extensions": {
            "wordpress": {
                "settings": {
                    "test_backend": "host-smoke"
                }
            }
        }
    }))
    .unwrap();

    let wordpress_settings = component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("wordpress"))
        .expect("wordpress extension config")
        .settings
        .clone();

    assert_eq!(
        wordpress_settings
            .get("test_backend")
            .and_then(|value| value.as_str()),
        Some("host-smoke")
    );
    assert!(wordpress_settings.get("settings").is_none());
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
