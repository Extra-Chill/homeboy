//! Project-level remote path resolution for ad-hoc file operations.
//!
//! Deploy resolves a component's `remote_path` against project `path_roots`
//! (e.g. mapping a `<managed-root>/...` prefix onto an absolute managed root
//! such as `/srv/htdocs/<managed-root>`). Ad-hoc `homeboy file` commands
//! historically joined every relative path against `base_path` alone, so on
//! installs whose active component directory lives outside `base_path` (e.g.
//! a managed host where `base_path` is `/htdocs/__app__` but active components
//! are written under `/srv/htdocs/<managed-root>/...`) operators could not
//! inspect the path deploy actually writes to. (#5456)
//!
//! This module applies the *same* managed-prefix resolution deploy uses, but
//! at the project layer (no component context). It stays agnostic: the managed
//! prefixes and root names come from extension manifests, not hard-coded here.

use std::collections::HashMap;

use crate::core::component;
use crate::core::extension::{self, RemotePathRootRule};
use crate::core::paths as base_path;
use crate::core::project::Project;

/// Resolve a remote path for an ad-hoc file operation against a project.
///
/// Resolution order:
/// 1. Absolute paths are used verbatim (operators can always target an exact
///    path such as `/srv/htdocs/<managed-root>/components/foo`).
/// 2. Relative paths matching an extension-declared managed prefix (e.g.
///    `<managed-root>`) resolve through the project's configured `path_roots`,
///    matching deploy's behavior so the inspectable path agrees with the
///    deployed path.
/// 3. Everything else joins against `base_path` (unchanged legacy behavior).
pub fn resolve_project_remote_path(
    project: &Project,
    base_path_value: &str,
    path: &str,
) -> crate::core::error::Result<String> {
    let trimmed = path.trim();

    // Absolute paths win immediately — join_remote_path already returns them
    // verbatim, but short-circuiting keeps the managed-prefix scan off the hot
    // path and makes the intent explicit.
    if trimmed.starts_with('/') {
        return base_path::join_remote_path(Some(base_path_value), trimmed);
    }

    if let Some(resolved) = resolve_with_path_roots(project, base_path_value, trimmed)? {
        return Ok(resolved);
    }

    base_path::join_remote_path(Some(base_path_value), trimmed)
}

fn resolve_with_path_roots(
    project: &Project,
    base_path_value: &str,
    path: &str,
) -> crate::core::error::Result<Option<String>> {
    for rule in project_remote_path_root_rules(project) {
        if !path_matches_prefix(path, &rule.path_prefix) {
            continue;
        }

        // Only resolve through a root that is actually configured for this
        // project. Without one we fall back to base_path joining so behavior is
        // unchanged for projects that never set path_roots.
        let Some(root) = project.path_roots.get(&rule.root) else {
            return Ok(None);
        };

        let child = if rule.strip_prefix {
            strip_path_prefix(path, &rule.path_prefix)
        } else {
            path
        };

        let resolved_root = base_path::join_remote_path(Some(base_path_value), root)?;

        if child.is_empty() {
            return Ok(Some(resolved_root));
        }

        return base_path::join_remote_path(Some(&resolved_root), child).map(Some);
    }

    Ok(None)
}

/// Collect every path-root rule declared by extensions attached to the project
/// (project-level extensions plus any extensions on attached components). Rules
/// are deduplicated by `(path_prefix, root)` so multiple components linking the
/// same extension don't produce redundant scans.
fn project_remote_path_root_rules(project: &Project) -> Vec<RemotePathRootRule> {
    let mut extension_ids: Vec<String> = Vec::new();

    if let Some(extensions) = &project.extensions {
        extension_ids.extend(extensions.keys().cloned());
    }

    // Attached components only carry IDs at the project layer; load each to read
    // the extensions declared in its repo-owned metadata. Failures are
    // non-fatal — a missing component must not break file path resolution.
    for attachment in &project.components {
        if let Ok(loaded) = component::load(&attachment.id) {
            if let Some(extensions) = &loaded.extensions {
                extension_ids.extend(extensions.keys().cloned());
            }
        }
    }

    let mut seen_rules: HashMap<(String, String), ()> = HashMap::new();
    let mut rules = Vec::new();
    let mut seen_extensions = HashMap::new();

    for id in extension_ids {
        if seen_extensions.insert(id.clone(), ()).is_some() {
            continue;
        }
        let Ok(manifest) = extension::load_extension(&id) else {
            continue;
        };
        let Some(deploy) = manifest.deploy else {
            continue;
        };
        for rule in deploy.path_roots {
            let key = (rule.path_prefix.clone(), rule.root.clone());
            if seen_rules.insert(key, ()).is_none() {
                rules.push(rule);
            }
        }
    }

    rules
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let path = path.trim_matches('/');
    let prefix = prefix.trim_matches('/');

    !prefix.is_empty() && (path == prefix || path.starts_with(&format!("{}/", prefix)))
}

fn strip_path_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    let path = path.trim_start_matches('/');
    let prefix = prefix.trim_matches('/');

    path.strip_prefix(prefix)
        .map(|remaining| remaining.trim_start_matches('/'))
        .unwrap_or(path)
}

#[cfg(test)]
#[path = "../../../tests/core/project/path_resolution_test.rs"]
mod path_resolution_test;
