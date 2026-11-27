//! Tests for project-level remote path resolution (#5456).
//!
//! Included into the lib via `#[path]` from
//! `src/core/project/path_resolution.rs`, so `super::*` reaches the module
//! under test and `crate::*` reaches the rest of the crate.

use super::*;

use crate::component::ScopedExtensionConfig;
use crate::test_support::with_isolated_home;
use homeboy_extension_contract::manifest_capabilities::DeployCapability;
use homeboy_extension_contract::ExtensionManifest;
use std::collections::HashMap;

fn install_wordpress_extension() {
    homeboy_core::extension::catalog::save_manifest(&ExtensionManifest {
        id: "wordpress".to_string(),
        name: "WordPress".to_string(),
        version: "1.0.0".to_string(),
        deploy: Some(DeployCapability {
            verifications: Vec::new(),
            overrides: Vec::new(),
            protected_path_suffixes: Vec::new(),
            owner_hints: Vec::new(),
            archive_install: Vec::new(),
            remote_path_inference: Vec::new(),
            dependency_inference: Vec::new(),
            path_roots: vec![RemotePathRootRule {
                path_prefix: "wp-content".to_string(),
                root: "wp_content".to_string(),
                strip_prefix: true,
                detect_command: None,
            }],
            version_patterns: Vec::new(),
            since_tag: None,
        }),
        ..serde_json::from_value(serde_json::json!({
            "name": "WordPress",
            "version": "1.0.0"
        }))
        .expect("manifest")
    })
    .expect("save extension");
}

fn wp_project(base_path: &str, wp_content_root: Option<&str>) -> Project {
    let mut path_roots = HashMap::new();
    if let Some(root) = wp_content_root {
        path_roots.insert("wp_content".to_string(), root.to_string());
    }

    Project {
        id: "studioweb-runtime".to_string(),
        base_path: Some(base_path.to_string()),
        path_roots,
        extensions: Some(HashMap::from([(
            "wordpress".to_string(),
            ScopedExtensionConfig::default(),
        )])),
        ..Project::default()
    }
}

#[test]
fn resolve_project_remote_path_routes_managed_prefixes_through_path_roots() {
    // (base_path, configured wp_content root, input path, expected remote path)
    let cases = [
        // Absolute paths are used verbatim.
        (
            "/htdocs/__wp__",
            Some("/srv/htdocs/wp-content"),
            "/srv/htdocs/wp-content/plugins/studio-web/inc/chat.php",
            "/srv/htdocs/wp-content/plugins/studio-web/inc/chat.php",
        ),
        // #5456: a relative `wp-content/...` path resolves to the managed
        // wp_content root, NOT base_path/wp-content under `/htdocs/__wp__`.
        (
            "/htdocs/__wp__",
            Some("/srv/htdocs/wp-content"),
            "wp-content/plugins/studio-web",
            "/srv/htdocs/wp-content/plugins/studio-web",
        ),
        // The bare managed prefix resolves to the root itself.
        (
            "/htdocs/__wp__",
            Some("/srv/htdocs/wp-content"),
            "wp-content",
            "/srv/htdocs/wp-content",
        ),
        // Without a configured root, managed prefixes fall back to base_path.
        (
            "/srv/site",
            None,
            "wp-content/plugins/foo",
            "/srv/site/wp-content/plugins/foo",
        ),
        // Non-managed relative paths join against base_path.
        (
            "/htdocs/__wp__",
            Some("/srv/htdocs/wp-content"),
            "wp-config.php",
            "/htdocs/__wp__/wp-config.php",
        ),
        // Prefix matching respects path segment boundaries.
        (
            "/htdocs/__wp__",
            Some("/srv/htdocs/wp-content"),
            "wp-content-extra/foo",
            "/htdocs/__wp__/wp-content-extra/foo",
        ),
        // A relative root resolves under base_path, mirroring deploy's
        // `resolve_with_project_root` semantics.
        (
            "/srv/site",
            Some("wp-content"),
            "wp-content/themes/theme",
            "/srv/site/wp-content/themes/theme",
        ),
    ];

    with_isolated_home(|_| {
        install_wordpress_extension();
        for (base_path, root, input, expected) in cases {
            let project = wp_project(base_path, root);
            let resolved =
                resolve_project_remote_path(&project, base_path, input).expect("resolve path");
            assert_eq!(resolved, expected, "input: {input}");
        }
    });
}
