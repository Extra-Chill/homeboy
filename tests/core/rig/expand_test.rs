//! Variable expansion tests for `src/core/rig/expand.rs`.
//!
//! Audit convention requires a `test_expand_vars` method matching the sole
//! public function of the module; additional cases cover edge conditions.

use crate::core::rig::expand::{expand_resources, expand_vars};
use crate::core::rig::spec::{
    ComponentSpec, DiscoverSpec, RigSpec, ServiceKind, ServiceSpec, SymlinkSpec,
};
use crate::core::paths;
use crate::test_support::with_isolated_home;
use std::collections::HashMap;
use std::fs;

fn rig_with(id: &str, components: HashMap<String, ComponentSpec>) -> RigSpec {
    RigSpec {
        id: id.to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        pipeline: Default::default(),
        bench: None,
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
        app_launcher: None,
    }
}

#[test]
fn test_expand_vars() {
    // Core contract: strings with no tokens pass through unchanged.
    let rig = rig_with("t", HashMap::new());
    assert_eq!(expand_vars(&rig, "plain/path"), "plain/path");
}

#[test]
fn test_expand_vars_component_path() {
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: "/tmp/studio".to_string(),
            checkout_root: None,
            remote_url: None,
            triage_remote_url: None,
            stack: None,
            branch: None,
            extensions: None,
        },
    );
    let rig = rig_with("t", components);
    assert_eq!(
        expand_vars(&rig, "${components.studio.path}/dist"),
        "/tmp/studio/dist"
    );
}

#[test]
fn test_expand_vars_package_root_from_installed_source_metadata() {
    with_isolated_home(|home| {
        let package_root = home.path().join("rig-packages/studio-web");
        let metadata_dir = paths::rig_sources().expect("rig sources dir");
        fs::create_dir_all(&metadata_dir).expect("mkdir rig sources");
        fs::write(
            metadata_dir.join("studio-web-product-matrix.json"),
            serde_json::json!({
                "source": package_root,
                "package_path": package_root,
                "rig_path": package_root.join("rigs/studio-web-product-matrix/rig.json"),
                "discovery_path": package_root,
                "linked": true,
                "source_revision": null
            })
            .to_string(),
        )
        .expect("write rig source metadata");

        let mut components = HashMap::new();
        components.insert(
            "studio-web".to_string(),
            ComponentSpec {
                path: "${package.root}".to_string(),
                checkout_root: None,
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                extensions: None,
            },
        );
        let rig = rig_with("studio-web-product-matrix", components);

        assert_eq!(
            expand_vars(&rig, "${package.root}/scripts/workflow-bench.mjs"),
            package_root.join("scripts/workflow-bench.mjs").to_string_lossy()
        );
        assert_eq!(
            expand_vars(&rig, "${components.studio-web.path}/bench"),
            package_root.join("bench").to_string_lossy()
        );
    });
}

#[test]
fn test_expand_vars_env_variable() {
    std::env::set_var("RIG_EXPAND_TEST_VAR", "hello");
    let rig = rig_with("t", HashMap::new());
    assert_eq!(expand_vars(&rig, "x=${env.RIG_EXPAND_TEST_VAR}"), "x=hello");
}

#[test]
fn test_expand_vars_unknown_token_is_literal() {
    let rig = rig_with("t", HashMap::new());
    assert_eq!(expand_vars(&rig, "${unknown.thing}"), "${unknown.thing}");
}

#[test]
fn test_expand_vars_unterminated_braces() {
    let rig = rig_with("t", HashMap::new());
    assert_eq!(expand_vars(&rig, "${unterminated"), "${unterminated");
}

#[test]
fn test_expand_resources_expands_string_entries() {
    with_isolated_home(|home| {
        let previous_resource_path = std::env::var("RIG_RESOURCE_PATH").ok();
        let previous_resource_namespace = std::env::var("RIG_RESOURCE_NAMESPACE").ok();
        std::env::set_var("RIG_RESOURCE_PATH", "studio@resources");
        std::env::set_var("RIG_RESOURCE_NAMESPACE", "bench-a");

        let mut components = HashMap::new();
        components.insert(
            "studio".to_string(),
            ComponentSpec {
                path: "~/Developer/studio".to_string(),
                checkout_root: None,
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                extensions: None,
            },
        );
        let mut rig = rig_with("t", components);
        rig.resources.exclusive = vec![
            "studio-runtime".to_string(),
            "studio-runtime:${env.RIG_RESOURCE_NAMESPACE}".to_string(),
        ];
        rig.resources.paths = vec![
            "~/Developer/${env.RIG_RESOURCE_PATH}".to_string(),
            "${components.studio.path}/apps/cli".to_string(),
        ];
        rig.resources.ports = vec![9724];
        rig.resources.process_patterns = vec!["wordpress-server-child.mjs".to_string()];

        let resources = expand_resources(&rig);
        let expected_paths = vec![
            home.path()
                .join("Developer/studio@resources")
                .to_string_lossy()
                .to_string(),
            home.path()
                .join("Developer/studio/apps/cli")
                .to_string_lossy()
                .to_string(),
        ];

        match previous_resource_path {
            Some(value) => std::env::set_var("RIG_RESOURCE_PATH", value),
            None => std::env::remove_var("RIG_RESOURCE_PATH"),
        }
        match previous_resource_namespace {
            Some(value) => std::env::set_var("RIG_RESOURCE_NAMESPACE", value),
            None => std::env::remove_var("RIG_RESOURCE_NAMESPACE"),
        }

        assert_eq!(
            resources.exclusive,
            vec!["studio-runtime", "studio-runtime:bench-a"]
        );
        assert_eq!(resources.ports, vec![9724]);
        assert_eq!(
            resources.process_patterns,
            vec!["wordpress-server-child.mjs"]
        );
        assert_eq!(resources.paths, expected_paths);
    });
}

#[test]
fn test_expand_resources_unset_env_in_exclusive_uses_default_namespace() {
    let previous = std::env::var("RIG_RESOURCE_NAMESPACE_NEVER_SET_XYZ").ok();
    std::env::remove_var("RIG_RESOURCE_NAMESPACE_NEVER_SET_XYZ");

    let mut rig = rig_with("t", HashMap::new());
    rig.resources.exclusive = vec![
        "studio-runtime".to_string(),
        "studio-runtime:${env.RIG_RESOURCE_NAMESPACE_NEVER_SET_XYZ}".to_string(),
    ];

    let resources = expand_resources(&rig);

    match previous {
        Some(value) => std::env::set_var("RIG_RESOURCE_NAMESPACE_NEVER_SET_XYZ", value),
        None => std::env::remove_var("RIG_RESOURCE_NAMESPACE_NEVER_SET_XYZ"),
    }

    assert_eq!(
        resources.exclusive,
        vec!["studio-runtime", "studio-runtime:<default>"]
    );
}

#[test]
fn test_expand_resources_derives_paths_ports_and_process_patterns() {
    with_isolated_home(|home| {
        let mut rig = rig_with("derived", HashMap::new());
        rig.symlinks = vec![
            SymlinkSpec {
                link: "~/bin/studio".to_string(),
                target: "/tmp/studio".to_string(),
            },
            SymlinkSpec {
                link: "~/bin/playground".to_string(),
                target: "/tmp/playground".to_string(),
            },
        ];
        rig.services.insert(
            "tarballs".to_string(),
            ServiceSpec {
                kind: ServiceKind::HttpStatic,
                cwd: None,
                port: Some(9724),
                command: None,
                env: HashMap::new(),
                health: None,
                discover: None,
            },
        );
        rig.services.insert(
            "daemon".to_string(),
            ServiceSpec {
                kind: ServiceKind::External,
                cwd: None,
                port: None,
                command: None,
                env: HashMap::new(),
                health: None,
                discover: Some(DiscoverSpec {
                    pattern: "wordpress-server-child.mjs".to_string(),
                    argv_contains: Vec::new(),
                }),
            },
        );

        let resources = expand_resources(&rig);

        assert_eq!(
            resources.paths,
            vec![
                home.path()
                    .join("bin/playground")
                    .to_string_lossy()
                    .to_string(),
                home.path().join("bin/studio").to_string_lossy().to_string(),
            ]
        );
        assert_eq!(resources.ports, vec![9724]);
        assert_eq!(
            resources.process_patterns,
            vec!["wordpress-server-child.mjs"]
        );
    });
}

#[test]
fn test_expand_resources_merges_explicit_resources_and_deduplicates() {
    with_isolated_home(|home| {
        let mut rig = rig_with("dedupe", HashMap::new());
        rig.resources.paths = vec!["~/bin/studio".to_string(), "~/Developer/manual".to_string()];
        rig.resources.ports = vec![9724, 3000];
        rig.resources.process_patterns = vec![
            "wordpress-server-child.mjs".to_string(),
            "manual-process".to_string(),
        ];
        rig.symlinks = vec![SymlinkSpec {
            link: "~/bin/studio".to_string(),
            target: "/tmp/studio".to_string(),
        }];
        rig.services.insert(
            "tarballs".to_string(),
            ServiceSpec {
                kind: ServiceKind::HttpStatic,
                cwd: None,
                port: Some(9724),
                command: None,
                env: HashMap::new(),
                health: None,
                discover: None,
            },
        );
        rig.services.insert(
            "daemon".to_string(),
            ServiceSpec {
                kind: ServiceKind::External,
                cwd: None,
                port: None,
                command: None,
                env: HashMap::new(),
                health: None,
                discover: Some(DiscoverSpec {
                    pattern: "wordpress-server-child.mjs".to_string(),
                    argv_contains: Vec::new(),
                }),
            },
        );

        let resources = expand_resources(&rig);

        assert_eq!(resources.exclusive, Vec::<String>::new());
        assert_eq!(
            resources.paths,
            vec![
                home.path().join("bin/studio").to_string_lossy().to_string(),
                home.path()
                    .join("Developer/manual")
                    .to_string_lossy()
                    .to_string(),
            ]
        );
        assert_eq!(resources.ports, vec![9724, 3000]);
        assert_eq!(
            resources.process_patterns,
            vec!["wordpress-server-child.mjs", "manual-process"]
        );
    });
}
