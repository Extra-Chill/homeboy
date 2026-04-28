//! Variable expansion tests for `src/core/rig/expand.rs`.
//!
//! Audit convention requires a `test_expand_vars` method matching the sole
//! public function of the module; additional cases cover edge conditions.

use crate::rig::expand::{expand_resources, expand_vars};
use crate::rig::spec::{ComponentSpec, RigSpec};
use crate::test_support::with_isolated_home;
use std::collections::HashMap;

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
fn test_expand_resources_expands_path_entries_only() {
    with_isolated_home(|home| {
        let previous_resource_path = std::env::var("RIG_RESOURCE_PATH").ok();
        std::env::set_var("RIG_RESOURCE_PATH", "studio@resources");

        let mut components = HashMap::new();
        components.insert(
            "studio".to_string(),
            ComponentSpec {
                path: "~/Developer/studio".to_string(),
                remote_url: None,
                triage_remote_url: None,
                stack: None,
                branch: None,
                extensions: None,
            },
        );
        let mut rig = rig_with("t", components);
        rig.resources.exclusive = vec!["studio-runtime".to_string()];
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

        assert_eq!(resources.exclusive, vec!["studio-runtime"]);
        assert_eq!(resources.ports, vec![9724]);
        assert_eq!(
            resources.process_patterns,
            vec!["wordpress-server-child.mjs"]
        );
        assert_eq!(resources.paths, expected_paths);
    });
}
