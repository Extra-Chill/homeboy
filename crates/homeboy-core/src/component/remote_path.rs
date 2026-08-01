//! Extension-driven `remote_path` auto-resolution for components.
//!
//! These functions used to be `Component` methods, but they reach into core's
//! `extension_store` (to load extension deploy rules) and the filesystem (to
//! test rule file-content conditions), so they cannot live in the leaf
//! `homeboy-component-contract` crate. They stay in core as free functions over
//! `&Component` / `&mut Component`.

use std::collections::{BTreeMap, BTreeSet};

use homeboy_component_contract::model::render_remote_path_template;
use homeboy_component_contract::Component;

use crate::error::Result;
use crate::extension_execution::{resolve_owner, REMOTE_PATH_SURFACE};

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
///
/// Infallible discovery form. Component discovery/probe paths (`resolve_target`,
/// `prefer_cwd_for_component`, rig materialization) have no error channel and
/// must degrade to "remote_path stays unset" rather than fail, so a genuine
/// ownership conflict is swallowed here. Deploy takes the fallible
/// [`try_auto_resolve_remote_path`] via
/// [`crate::project::component_remote_path`], where an unresolved conflict is a
/// hard error instead of an empty path.
pub fn auto_resolve_remote_path(component: &Component) -> Option<String> {
    try_auto_resolve_remote_path(component).ok().flatten()
}

/// Auto-resolve `remote_path`, surfacing genuine multi-extension ownership
/// conflicts instead of silently returning `None`.
///
/// Rules were previously collected into a `HashSet<String>` of rendered paths
/// and `if matches.len() == 1 { .. } else { None }` — so two extensions with
/// conflicting `remote_path_inference` produced `None`, *indistinguishable from
/// "no rule matched"*, and deploy then proceeded with an empty `remote_path`.
/// That is the same silent-wrong-answer shape that
/// [`crate::component::resolve_artifact`] documents as "a silent wrong-artifact
/// deploy (#10281)", left unfixed here (#11119).
///
/// Now providers are collected per extension in a `BTreeMap` (deterministic,
/// unlike `Component.extensions`' `HashMap`) and the distinct rendered paths
/// decide:
///
/// - no rule matched → `Ok(None)`, as before
/// - one distinct path → `Ok(Some(path))`, however many rules or extensions
///   produced it (agreeing rules are not a conflict — WordPress's two
///   plugin-detection rules both render `wp-content/plugins/{{dir_name}}`)
/// - conflicting paths → [`resolve_owner`] on the `remote_path` surface:
///   explicit `capability_extensions.remote_path`, then `composition.includes`
///   primacy, then a hard error carrying a runnable fix command
/// - conflicting paths *within the single owning extension* → hard error, since
///   extension-level ownership cannot break a tie its own rules created
pub fn try_auto_resolve_remote_path(component: &Component) -> Result<Option<String>> {
    // File components cannot auto-resolve — they must have explicit remote_path.
    if std::path::Path::new(&component.local_path).is_file() {
        return Ok(None);
    }

    let local = std::path::Path::new(&component.local_path);

    // Use the directory basename as the remote directory name.
    let Some(dir_name) = local.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };

    let Some(extensions) = component.extensions.as_ref() else {
        return Ok(None);
    };

    let mut providers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for extension_id in extensions.keys() {
        let Ok(extension) = crate::extension_store::load_extension(extension_id) else {
            continue;
        };

        for rule in extension.remote_path_inference_rules() {
            if remote_path_inference_rule_matches(component, rule, local, dir_name) {
                providers.entry(extension_id.clone()).or_default().insert(
                    render_remote_path_template(&rule.remote_path, &component.id, dir_name),
                );
            }
        }
    }

    let distinct: BTreeSet<&str> = providers
        .values()
        .flatten()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    match distinct.len() {
        0 => Ok(None),
        1 => Ok(distinct.into_iter().next().map(ToOwned::to_owned)),
        _ => {
            let candidates: Vec<String> = providers.keys().cloned().collect();
            let owner = resolve_owner(component, REMOTE_PATH_SURFACE, &candidates)?;
            let owned = providers.get(&owner).cloned().unwrap_or_default();

            match owned.len() {
                1 => Ok(owned.into_iter().next()),
                // The winning extension's own rules disagree. Extension-level
                // ownership cannot arbitrate that, so fail loudly rather than
                // pick one and deploy somewhere arbitrary.
                _ => Err(conflicting_rules_within_extension_error(
                    component, &owner, &owned,
                )),
            }
        }
    }
}

fn conflicting_rules_within_extension_error(
    component: &Component,
    owner: &str,
    paths: &BTreeSet<String>,
) -> crate::error::Error {
    crate::error::Error::validation_invalid_argument(
        "remotePath",
        format!(
            "Component '{}' matches conflicting remote_path_inference rules from extension '{}': {}",
            component.id,
            owner,
            paths.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
        None,
        Some(vec![format!(
            "Set an explicit remote_path: homeboy component set {} --remote-path <path>",
            component.id
        )]),
    )
    .with_hint(format!(
        "Set an explicit remote_path: homeboy component set {} --remote-path <path>",
        component.id
    ))
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
