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
        return super::install::read_source_metadata(&rig.id).map(|metadata| metadata.package_path);
    }
    if let Some(rest) = token.strip_prefix("components.") {
        // Expect "<id>.path" — future fields can add here.
        let (id, field) = rest.split_once('.')?;
        if field != "path" {
            return None;
        }
        let component = rig.components.get(id)?;
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

#[cfg(test)]
#[path = "../../../tests/core/rig/expand_test.rs"]
mod expand_test;
