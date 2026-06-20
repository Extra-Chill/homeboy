//! Tests for project-level remote path resolution (#5456).
//!
//! Included into the lib via `#[path]` from
//! `src/core/project/path_resolution.rs`, so `super::*` reaches the module
//! under test and `crate::*` reaches the rest of the crate.

use super::*;

use crate::core::component::ScopedExtensionConfig;
use crate::core::extension::{DeployCapability, ExtensionManifest};
use crate::test_support::with_isolated_home;
use std::collections::HashMap;

fn install_wordpress_extension() {
    crate::core::extension::save_manifest(&ExtensionManifest {
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
fn absolute_paths_are_used_verbatim() {
    with_isolated_home(|_| {
        install_wordpress_extension();
        let project = wp_project("/htdocs/__wp__", Some("/srv/htdocs/wp-content"));

        let resolved = resolve_project_remote_path(
            &project,
            "/htdocs/__wp__",
            "/srv/htdocs/wp-content/plugins/studio-web/inc/frontend-chat-integration.php",
        )
        .expect("resolve absolute");

        assert_eq!(
            resolved,
            "/srv/htdocs/wp-content/plugins/studio-web/inc/frontend-chat-integration.php"
        );
    });
}

#[test]
fn managed_prefix_resolves_through_configured_root() {
    // The crux of #5456: a relative `wp-content/...` path must resolve to the
    // managed wp_content root (where deploy writes active plugins), NOT to
    // base_path/wp-content under `/htdocs/__wp__`.
    with_isolated_home(|_| {
        install_wordpress_extension();
        let project = wp_project("/htdocs/__wp__", Some("/srv/htdocs/wp-content"));

        let resolved = resolve_project_remote_path(
            &project,
            "/htdocs/__wp__",
            "wp-content/plugins/studio-web",
        )
        .expect("resolve managed prefix");

        assert_eq!(resolved, "/srv/htdocs/wp-content/plugins/studio-web");
    });
}

#[test]
fn managed_prefix_without_configured_root_falls_back_to_base_path() {
    // Projects that never configured a wp_content root keep the legacy
    // base_path-joined behavior — no surprise breakage.
    with_isolated_home(|_| {
        install_wordpress_extension();
        let project = wp_project("/srv/site", None);

        let resolved = resolve_project_remote_path(&project, "/srv/site", "wp-content/plugins/foo")
            .expect("resolve fallback");

        assert_eq!(resolved, "/srv/site/wp-content/plugins/foo");
    });
}

#[test]
fn non_managed_relative_paths_join_against_base_path() {
    with_isolated_home(|_| {
        install_wordpress_extension();
        let project = wp_project("/htdocs/__wp__", Some("/srv/htdocs/wp-content"));

        let resolved = resolve_project_remote_path(&project, "/htdocs/__wp__", "wp-config.php")
            .expect("resolve non-managed");

        assert_eq!(resolved, "/htdocs/__wp__/wp-config.php");
    });
}

#[test]
fn relative_root_is_joined_against_base_path() {
    // A path_root configured as a relative value resolves under base_path,
    // mirroring deploy's `resolve_with_project_root` semantics.
    with_isolated_home(|_| {
        install_wordpress_extension();
        let project = wp_project("/srv/site", Some("wp-content"));

        let resolved =
            resolve_project_remote_path(&project, "/srv/site", "wp-content/themes/theme")
                .expect("resolve relative root");

        assert_eq!(resolved, "/srv/site/wp-content/themes/theme");
    });
}

#[test]
fn path_matches_prefix_handles_boundaries() {
    assert!(path_matches_prefix("wp-content", "wp-content"));
    assert!(path_matches_prefix("wp-content/plugins/foo", "wp-content"));
    assert!(path_matches_prefix("/wp-content/plugins", "wp-content"));
    assert!(!path_matches_prefix("wp-content-extra/foo", "wp-content"));
    assert!(!path_matches_prefix("var/log", "wp-content"));
    assert!(!path_matches_prefix("anything", ""));
}

#[test]
fn strip_path_prefix_removes_managed_prefix() {
    assert_eq!(
        strip_path_prefix("wp-content/plugins/foo", "wp-content"),
        "plugins/foo"
    );
    assert_eq!(strip_path_prefix("wp-content", "wp-content"), "");
    assert_eq!(
        strip_path_prefix("var/log/app.log", "wp-content"),
        "var/log/app.log"
    );
}
