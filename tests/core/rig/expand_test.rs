//! Variable expansion tests for `src/core/rig/expand.rs`.
//!
//! Audit convention requires a `test_expand_vars` method matching the sole
//! public function of the module; additional cases cover edge conditions.

use crate::rig::expand::expand_vars;
use crate::rig::spec::{ComponentSpec, RigSpec};
use std::collections::HashMap;

fn rig_with(id: &str, components: HashMap<String, ComponentSpec>) -> RigSpec {
    RigSpec {
        id: id.to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        pipeline: Default::default(),
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
            stack: None,
            branch: None,
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
