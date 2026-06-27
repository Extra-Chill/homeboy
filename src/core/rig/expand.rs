//! Variable expansion for rig spec strings.
//!
//! Supports three substitutions in `cwd`, `command`, `link`, `target`, and
//! check fields:
//!
//! - `${components.<id>.path}` — component path from the rig spec
//! - `${env.<NAME>}` — process environment variable (empty if unset)
//! - `${package.root}` — installed rig package root, when source metadata exists
//! - `~` — home directory (via `shellexpand::tilde`)
//!
//! Unknown `${...}` patterns are left untouched so users get a clear
//! command-run failure instead of a silent empty string.

use super::spec::{RigResourcesSpec, RigSpec};
use crate::core::expand;
use std::collections::BTreeSet;

/// Expand variables + tilde in a string.
pub fn expand_vars(rig: &RigSpec, input: &str) -> String {
    expand::expand_with_tilde(input, |token| resolve_token(rig, token))
}

/// Return a copy of the rig resource declarations with expandable string entries expanded.
pub fn expand_resources(rig: &RigSpec) -> RigResourcesSpec {
    let mut resources = rig.resources.clone();
    resources.exclusive = merge_expanded_strings(
        rig,
        resources.exclusive.iter().map(String::as_str),
        std::iter::empty(),
    )
    .into_iter()
    .map(normalize_exclusive_resource)
    .collect();

    let derived_paths = rig.symlinks.iter().map(|symlink| symlink.link.as_str());
    resources.paths = merge_expanded_strings(
        rig,
        resources.paths.iter().map(String::as_str),
        derived_paths,
    );

    let derived_ports = rig.services.values().filter_map(|service| service.port);
    resources.ports = merge_values(resources.ports.iter().copied(), derived_ports);

    let derived_process_patterns = rig
        .services
        .values()
        .filter_map(|service| service.discover.as_ref())
        .map(|discover| discover.pattern.as_str());
    resources.process_patterns = merge_strings(
        resources.process_patterns.iter().map(String::as_str),
        derived_process_patterns,
    );
    resources
}

fn merge_expanded_strings<'a>(
    rig: &RigSpec,
    explicit: impl Iterator<Item = &'a str>,
    derived: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    merge_strings(
        explicit.map(|value| expand_vars(rig, value)),
        derived.map(|value| expand_vars(rig, value)),
    )
}

fn merge_strings(
    explicit: impl Iterator<Item = impl Into<String>>,
    derived: impl Iterator<Item = impl Into<String>>,
) -> Vec<String> {
    merge_values(explicit.map(Into::into), derived.map(Into::into))
}

fn merge_values<T: Clone + Eq + Ord>(
    explicit: impl Iterator<Item = T>,
    derived: impl Iterator<Item = T>,
) -> Vec<T> {
    let mut values: Vec<T> = explicit.collect();
    for value in derived.collect::<BTreeSet<_>>() {
        if !values.contains(&value) {
            values.push(value);
        }
    }
    values
}

fn normalize_exclusive_resource(resource: String) -> String {
    let resource = resource.trim().to_string();
    if resource.is_empty() {
        return "<default>".to_string();
    }
    if resource.ends_with(':') {
        return format!("{}<default>", resource);
    }
    resource
}

fn resolve_token(rig: &RigSpec, token: &str) -> Option<String> {
    if token == "package.root" {
        if let Some(package_root) = super::local_package_root(&rig.id) {
            return Some(package_root.to_string_lossy().to_string());
        }
        return super::install::read_source_metadata(&rig.id).map(|metadata| metadata.package_path);
    }
    if let Some(rest) = token.strip_prefix("components.") {
        // Expect "<id>.path" — future fields can add here.
        let (id, field) = rest.split_once('.')?;
        if field != "path" {
            return None;
        }
        if let Ok(path) = super::component_resolution::resolve_component_path(rig, id) {
            return Some(path);
        }
        let component = rig.components.get(id)?;
        // A caller may pin a component to an effective filesystem path via a
        // generic per-component override env var. Lab offload uses this to point
        // `${components.<id>.path}` at the runner-side materialized checkout
        // instead of the controller path the rig spec declares. This is generic:
        // it works for any rig/component and is a no-op when the override is
        // unset, so local checks keep their declared-path behavior.
        if let Some(override_path) = component_path_override_from_env(&rig.id, id) {
            return Some(override_path);
        }
        let expanded = expand::expand_with_tilde(&component.path, |token| match token {
            "package.root" => resolve_token(rig, token),
            token if token.starts_with("env.") => resolve_token(rig, token),
            _ => None,
        });
        return Some(expanded);
    }
    if let Some(name) = token.strip_prefix("env.") {
        return Some(std::env::var(name).unwrap_or_default());
    }
    None
}

/// Read a per-component effective-path override from the environment.
///
/// The override env var name is derived generically from the rig and component
/// ids (see [`rig_component_path_override_env_name`]). When present and
/// non-empty, it pins `${components.<id>.path}` to that filesystem path. Tilde
/// is expanded against the local home of the process reading it (the runner,
/// when the check executes on the runner), so a portable value still resolves.
pub(crate) fn component_path_override_from_env(rig_id: &str, component_id: &str) -> Option<String> {
    let name = rig_component_path_override_env_name(rig_id, component_id);
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(expand::expand_with_tilde(trimmed, |_| None))
}

/// Build the generic override env var name for a rig component's effective path.
///
/// Non-alphanumeric characters in the rig/component ids are normalized to `_`
/// and uppercased so the name is a valid shell identifier. Example:
/// rig `studio-web`, component `studio` -> `HOMEBOY_RIG_COMPONENT_PATH__STUDIO_WEB__STUDIO`.
pub fn rig_component_path_override_env_name(rig_id: &str, component_id: &str) -> String {
    format!(
        "HOMEBOY_RIG_COMPONENT_PATH__{}__{}",
        sanitize_env_segment(rig_id),
        sanitize_env_segment(component_id),
    )
}

fn sanitize_env_segment(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../../../tests/core/rig/expand_test.rs"]
mod expand_test;
