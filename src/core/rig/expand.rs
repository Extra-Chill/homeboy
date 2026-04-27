//! Variable expansion for rig spec strings.
//!
//! Supports three substitutions in `cwd`, `command`, `link`, `target`, and
//! check fields:
//!
//! - `${components.<id>.path}` — component path from the rig spec
//! - `${env.<NAME>}` — process environment variable (empty if unset)
//! - `~` — home directory (via `shellexpand::tilde`)
//!
//! Unknown `${...}` patterns are left untouched so users get a clear
//! command-run failure instead of a silent empty string.

use super::spec::RigSpec;
use crate::expand;

/// Expand variables + tilde in a string.
pub fn expand_vars(rig: &RigSpec, input: &str) -> String {
    expand::expand_with_tilde(input, |token| resolve_token(rig, token))
}

fn resolve_token(rig: &RigSpec, token: &str) -> Option<String> {
    if let Some(rest) = token.strip_prefix("components.") {
        // Expect "<id>.path" — future fields can add here.
        let (id, field) = rest.split_once('.')?;
        if field != "path" {
            return None;
        }
        let component = rig.components.get(id)?;
        let expanded = shellexpand::tilde(&component.path).into_owned();
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
