//! Extension-driven `remote_path` auto-resolution for components.
//!
//! These functions used to be `Component` methods, but they reach into core's
//! `extension_store` (to load extension deploy rules) and the filesystem (to
//! test rule file-content conditions), so they cannot live in the leaf
//! `homeboy-component-contract` crate. They stay in core as free functions over
//! `&Component` / `&mut Component`.

use std::collections::HashSet;

use homeboy_component_contract::model::render_remote_path_template;
use homeboy_component_contract::Component;

/// Auto-resolve `remote_path` from linked extension deploy rules when not
/// explicitly set.
///
/// Extensions can declare generic file-content checks and target-path
/// templates. Core does not know framework-specific deploy paths; it only
/// evaluates the extension-provided contract.
///
/// Extension templates can use the **local directory name** (basename of
/// `local_path`) separately from the component ID. This keeps deploy paths
/// correct when a component ID differs from the on-disk package directory.
///
/// Returns `Some(path)` if auto-resolved, `None` if not applicable or not
/// detectable.
pub fn auto_resolve_remote_path(component: &Component) -> Option<String> {
    // File components cannot auto-resolve — they must have explicit remote_path.
    if std::path::Path::new(&component.local_path).is_file() {
        return None;
    }

    let local = std::path::Path::new(&component.local_path);

    // Use the directory basename as the remote directory name.
    let dir_name = local.file_name()?.to_str()?;

    let mut matches = HashSet::new();
    for extension_id in component.extensions.as_ref()?.keys() {
        let Ok(extension) = crate::extension_store::load_extension(extension_id) else {
            continue;
        };

        for rule in extension.remote_path_inference_rules() {
            if remote_path_inference_rule_matches(component, rule, local, dir_name) {
                matches.insert(render_remote_path_template(
                    &rule.remote_path,
                    &component.id,
                    dir_name,
                ));
            }
        }
    }

    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

fn remote_path_inference_rule_matches(
    component: &Component,
    rule: &homeboy_extension_contract::RemotePathInferenceRule,
    local: &std::path::Path,
    dir_name: &str,
) -> bool {
    let relative_file =
        render_remote_path_template(&rule.when_file_contains.file, &component.id, dir_name);
    let relative_path = std::path::Path::new(&relative_file);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return false;
    }

    let file = local.join(relative_path);
    let Ok(content) = std::fs::read_to_string(file) else {
        return false;
    };

    content.contains(&rule.when_file_contains.text)
}

/// Ensure `remote_path` is populated. If empty, attempt auto-resolution.
///
/// This should be called after all config layers (repo portable, project
/// overrides) have been applied. It fills in `remote_path` only if still empty.
pub fn resolve_remote_path(component: &mut Component) {
    if component.remote_path.trim().is_empty() {
        if let Some(resolved) = auto_resolve_remote_path(component) {
            component.remote_path = resolved;
        }
    }
}
