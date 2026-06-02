use super::*;
use crate::core::component::{Component, ScopedExtensionConfig};
use std::collections::HashMap;

#[test]
fn extension_capability_owns_labels_and_scripts() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "scripts": {
            "compiler_warnings": "compiler-warnings.sh",
            "compiler_warning_fixes": "compiler-warning-fixes.sh"
        },
        "runtime": { "runtimes": { "node": { "version": "24" } } },
        "lint": { "extension_script": "lint.sh" },
        "test": {
            "extension_script": "test.sh",
            "result_parse": {
                "rules": [{ "pattern": "Tests: (\\d+)", "field": "total" }]
            }
        },
        "build": { "extension_script": "build.sh" },
        "bench": { "extension_script": "bench.sh" },
        "trace": { "extension_script": "trace.sh" },
        "deps": { "extension_script": "deps.sh" }
    }))
    .unwrap();

    assert_eq!(
        manifest
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.runtimes.get("node"))
            .map(|runtime| runtime.version.as_str()),
        Some("24")
    );
    assert_eq!(
        manifest
            .test
            .as_ref()
            .and_then(|test| test.result_parse.as_ref())
            .map(|spec| spec.rules.len()),
        Some(1)
    );
    assert_eq!(
        manifest.compiler_warnings_script(),
        Some("compiler-warnings.sh")
    );
    assert_eq!(
        manifest.compiler_warning_fixes_script(),
        Some("compiler-warning-fixes.sh")
    );

    for (capability, label, script, requires_script) in [
        (ExtensionCapability::Lint, "lint", "lint.sh", true),
        (ExtensionCapability::Test, "test", "test.sh", true),
        (ExtensionCapability::Build, "build", "build.sh", false),
        (ExtensionCapability::Bench, "bench", "bench.sh", true),
        (ExtensionCapability::Trace, "trace", "trace.sh", true),
        (ExtensionCapability::Deps, "deps", "deps.sh", true),
    ] {
        assert_eq!(capability.label(), label);
        assert!(capability.has_manifest_support(&manifest));
        assert_eq!(capability.script_path(&manifest), Some(script));
        assert_eq!(capability.requires_script(), requires_script);
    }
}

#[test]
fn manifest_parses_declared_structured_sidecars() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "structured_sidecars": {
            "findings": {
                "path": "findings.json",
                "schema_version": "1"
            },
            "producer.summary": {
                "schema_version": "1",
                "producer": "lint"
            },
            "lint.findings": true,
            "lint.producers": true,
            "trace.results": true,
            "trace.artifacts": true,
            "test.coverage": false
        }
    }))
    .unwrap();

    let sidecars = manifest.structured_sidecars();
    assert_eq!(sidecars.len(), 6);
    assert_eq!(sidecars[0].name, "findings");
    assert_eq!(sidecars[0].path, "findings.json");
    assert_eq!(sidecars[0].schema_version.as_deref(), Some("1"));
    assert_eq!(sidecars[0].producer, None);
    assert_eq!(sidecars[1].name, "lint.findings");
    assert_eq!(sidecars[1].path, "lint-findings.json");
    assert_eq!(sidecars[1].schema_version, None);
    assert_eq!(sidecars[1].producer.as_deref(), Some("lint"));
    assert_eq!(sidecars[2].name, "lint.producers");
    assert_eq!(sidecars[2].path, "lint-producers.json");
    assert_eq!(sidecars[2].producer.as_deref(), Some("lint"));
    assert_eq!(sidecars[3].name, "producer.summary");
    assert_eq!(sidecars[3].path, "producer-summary.json");
    assert_eq!(sidecars[3].producer.as_deref(), Some("lint"));
    assert_eq!(sidecars[4].name, "trace.artifacts");
    assert_eq!(sidecars[4].path, "artifacts");
    assert_eq!(sidecars[4].producer.as_deref(), Some("trace"));
    assert_eq!(sidecars[5].name, "trace.results");
    assert_eq!(sidecars[5].path, "trace.json");
    assert_eq!(sidecars[5].producer.as_deref(), Some("trace"));
}

#[test]
fn structured_sidecar_schema_versions_come_from_top_level_contract() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "structured_sidecars": {
            "findings": {
                "path": "findings.json",
                "schema_version": "2"
            },
            "lint.findings": true,
            "test.failures": false
        },
        "lint": {
            "extension_script": "lint.sh",
            "findings_schema_version": "1"
        }
    }))
    .unwrap();

    assert_eq!(
        manifest.structured_sidecar_schema_version("findings"),
        Some("2")
    );
    assert_eq!(
        manifest.structured_sidecar_schema_version("lint.findings"),
        None
    );
    assert_eq!(
        manifest.structured_sidecar_schema_version("test.failures"),
        None
    );
}

#[test]
fn missing_sidecar_declarations_have_no_structured_contract() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "lint": { "extension_script": "lint.sh" },
        "test": { "extension_script": "test.sh" }
    }))
    .unwrap();

    assert!(manifest.structured_sidecars().is_empty());
}

#[test]
fn legacy_sidecar_schema_fields_do_not_declare_structured_contracts() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "lint": {
            "extension_script": "lint.sh",
            "findings_schema_version": "1"
        },
        "test": {
            "extension_script": "test.sh",
            "results_schema_version": "1",
            "failures_schema_version": "1"
        },
        "annotations_schema_version": "1"
    }))
    .unwrap();

    assert!(manifest.structured_sidecars().is_empty());
}

#[test]
fn structured_sidecar_declarations_reject_unknown_fields() {
    let err = serde_json::from_value::<ExtensionManifest>(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "structured_sidecars": {
            "findings": {
                "path": "findings.json",
                "schema_version": "1",
                "legacy": true
            }
        }
    }))
    .expect_err("sidecar declarations should have one explicit shape");

    assert!(err.to_string().contains("data did not match"));
}

#[test]
fn manifest_parses_changed_test_routing_contract() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "test": {
            "extension_script": "test.sh",
            "changed_file_routing": {
                "strategy": "exclusive_env",
                "exclusive_env": {
                    "name": "HOMEBOY_FIXTURE_HOST_SMOKE_FILES",
                    "globs": ["tests/**/*-smoke.php"]
                }
            }
        }
    }))
    .unwrap();

    let routing = manifest
        .test
        .as_ref()
        .and_then(|test| test.changed_file_routing.as_ref())
        .expect("test routing should parse");

    assert_eq!(
        routing.strategy,
        TestChangedFileRoutingStrategy::ExclusiveEnv
    );
    assert_eq!(
        routing
            .exclusive_env
            .as_ref()
            .map(|exclusive_env| exclusive_env.name.as_str()),
        Some("HOMEBOY_FIXTURE_HOST_SMOKE_FILES")
    );
}

#[test]
fn manifest_rejects_legacy_discovery_marker_alias() {
    let err = serde_json::from_value::<ExtensionManifest>(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "provides": {
            "discoveryMarkers": [{ "all": ["package.json"] }]
        }
    }))
    .expect_err("camelCase discovery marker alias should be rejected");

    assert!(err.to_string().contains("discoveryMarkers"));
}

#[test]
fn manifest_parses_archive_install_deploy_contract() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "deploy": {
            "protected_path_suffixes": ["/srv/extensions"],
            "owner_hints": [
                {
                    "path_contains": "extensions/",
                    "suggested_owner": "www-data:www-data"
                }
            ],
            "archive_install": [
                {
                    "path_pattern": "/srv/extensions/",
                    "staging_path": "/tmp/homeboy-extension-staging",
                    "root_must_match_target_basename": true,
                    "required_header": {
                        "file_glob": "*.php",
                        "contains": "Plugin Name:"
                    },
                    "skip_permissions_fix": true
                }
            ]
        }
    }))
    .expect("archive install deploy policy should parse");

    let policy = manifest
        .deploy_archive_installs()
        .first()
        .expect("archive install policy");
    assert_eq!(policy.path_pattern, "/srv/extensions/");
    assert_eq!(policy.staging_path, "/tmp/homeboy-extension-staging");
    assert!(policy.root_must_match_target_basename);
    assert!(policy.skip_permissions_fix);
    assert_eq!(
        policy
            .required_header
            .as_ref()
            .and_then(|header| header.file_glob.as_deref()),
        Some("*.php")
    );

    let deploy = manifest.deploy.as_ref().expect("deploy contract");
    assert_eq!(deploy.protected_path_suffixes, ["/srv/extensions"]);
    assert_eq!(deploy.owner_hints[0].path_contains, "extensions/");
    assert_eq!(deploy.owner_hints[0].suggested_owner, "www-data:www-data");
}

#[test]
fn deploy_contract_rejects_unknown_active_policy_keys() {
    let err = serde_json::from_value::<ExtensionManifest>(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "deploy": {
            "archiveInstall": []
        }
    }))
    .expect_err("unsupported deploy keys should be rejected");

    assert!(err.to_string().contains("archiveInstall"));
}

#[test]
fn archive_install_required_header_rejects_ambiguous_selector() {
    let err = serde_json::from_value::<ExtensionManifest>(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "deploy": {
            "archive_install": [
                {
                    "path_pattern": "/wp-content/plugins/",
                    "required_header": {
                        "file": "plugin.php",
                        "file_glob": "*.php",
                        "contains": "Plugin Name:"
                    }
                }
            ]
        }
    }))
    .expect_err("required_header must choose exactly one selector");

    assert!(err.to_string().contains("exactly one of file or file_glob"));
}

#[test]
fn archive_install_required_header_rejects_missing_selector() {
    let err = serde_json::from_value::<ExtensionManifest>(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "deploy": {
            "archive_install": [
                {
                    "path_pattern": "/wp-content/plugins/",
                    "required_header": {
                        "contains": "Plugin Name:"
                    }
                }
            ]
        }
    }))
    .expect_err("required_header must declare a selector");

    assert!(err.to_string().contains("exactly one of file or file_glob"));
}

#[test]
fn runtime_requirements_reject_legacy_top_level_and_string_shapes() {
    let top_level = serde_json::from_value::<RuntimeRequirementsConfig>(serde_json::json!({
        "php": { "version": "8.2" },
        "node": { "version": "22" }
    }))
    .expect_err("top-level runtime aliases should be rejected");
    assert!(top_level.to_string().contains("unknown field"));

    let shorthand = serde_json::from_value::<RuntimeRequirementsConfig>(serde_json::json!({
        "runtimes": {
            "node": "22"
        }
    }))
    .expect_err("runtime string shorthand should be rejected");
    assert!(shorthand.to_string().contains("string"));
}

#[test]
fn test_drift_ignores_audit_test_mapping_fallback() {
    let manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Example",
        "version": "0.0.0",
        "audit": {
            "test_mapping": {
                "source_dirs": ["src"],
                "test_dirs": ["tests"],
                "test_file_pattern": "tests/{dir}/{name}_test.{ext}",
                "inline_tests": true
            }
        }
    }))
    .unwrap();

    assert_eq!(
        manifest.test_mapping().map(|mapping| mapping.inline_tests),
        Some(true)
    );
    assert_eq!(manifest.test_drift(), None);
}

#[test]
fn validate_required_extensions_passes_with_no_modules() {
    let comp = Component {
        id: "test-component".to_string(),
        ..Default::default()
    };
    assert!(validate_required_extensions(&comp).is_ok());
}

#[test]
fn validate_required_extensions_passes_with_empty_modules() {
    let comp = Component {
        id: "test-component".to_string(),
        extensions: Some(HashMap::new()),
        ..Default::default()
    };
    assert!(validate_required_extensions(&comp).is_ok());
}

#[test]
fn validate_required_extensions_fails_with_missing_module() {
    let mut extensions = HashMap::new();
    extensions.insert(
        "nonexistent-extension-abc123".to_string(),
        ScopedExtensionConfig::default(),
    );
    let comp = Component {
        id: "test-component".to_string(),
        extensions: Some(extensions),
        ..Default::default()
    };
    let err = validate_required_extensions(&comp).unwrap_err();
    assert_eq!(err.code, crate::core::error::ErrorCode::ExtensionNotFound);
    assert!(err.message.contains("nonexistent-extension-abc123"));
    assert!(err.message.contains("test-component"));
    // Should have install hint + browse hint
    assert!(err.hints.len() >= 2);
    assert!(err
        .hints
        .iter()
        .any(|h| h.message.contains("homeboy extension install")));
    assert!(err
        .hints
        .iter()
        .any(|h| h.message.contains("homeboy-extensions")));
}

#[test]
fn validate_required_extensions_reports_all_missing() {
    let mut extensions = HashMap::new();
    extensions.insert(
        "missing-mod-a".to_string(),
        ScopedExtensionConfig::default(),
    );
    extensions.insert(
        "missing-mod-b".to_string(),
        ScopedExtensionConfig::default(),
    );
    let comp = Component {
        id: "multi-dep".to_string(),
        extensions: Some(extensions),
        ..Default::default()
    };
    let err = validate_required_extensions(&comp).unwrap_err();
    // Error should mention both missing extensions
    assert!(err.message.contains("missing-mod-a"));
    assert!(err.message.contains("missing-mod-b"));
    // Should have install hint for each + browse hint
    assert!(err.hints.len() >= 3);
}

#[test]
fn test_validate_extension_requirements() {
    let comp = Component {
        id: "test-component".to_string(),
        ..Default::default()
    };
    assert!(validate_extension_requirements(&comp).is_ok());
}

#[test]
fn extension_guidance_hints_point_to_supported_paths() {
    let comp = Component {
        id: "plain-package".to_string(),
        ..Default::default()
    };

    let hints = extension_guidance_hints(&comp, Some(ExtensionCapability::Build));

    assert!(hints
        .iter()
        .any(|hint| { hint.contains("homeboy component set plain-package --extension") }));
    assert!(hints
        .iter()
        .any(|hint| { hint.contains("Use `scripts.build` for component-owned build commands") }));
    assert!(hints
        .iter()
        .any(|hint| hint.contains("homeboy extension list")));
}

#[test]
fn runner_step_filter_applies_step_and_skip() {
    let filter = RunnerStepFilter {
        step: Some("lint,test".to_string()),
        skip: Some("test".to_string()),
    };
    assert!(filter.should_run("lint"));
    assert!(!filter.should_run("test"));
    assert!(!filter.should_run("deploy"));
}

#[test]
fn runner_step_filter_exports_env_pairs() {
    let filter = RunnerStepFilter {
        step: Some("a".to_string()),
        skip: Some("b".to_string()),
    };
    let env = filter.to_env_pairs();
    assert!(env.iter().any(|(k, v)| k == "HOMEBOY_STEP" && v == "a"));
    assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "b"));
}
