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

/// Expand variables + tilde in a string.
pub fn expand_vars(rig: &RigSpec, input: &str) -> String {
    let substituted = substitute(rig, input);
    shellexpand::tilde(&substituted).into_owned()
}

fn substitute(rig: &RigSpec, input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut token = String::new();
            let mut closed = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    closed = true;
                    break;
                }
                token.push(inner);
            }
            if !closed {
                // Unterminated — emit literal to avoid data loss.
                out.push_str("${");
                out.push_str(&token);
                continue;
            }
            match resolve_token(rig, &token) {
                Some(value) => out.push_str(&value),
                None => {
                    // Unknown token: leave literal for diagnostics.
                    out.push_str("${");
                    out.push_str(&token);
                    out.push('}');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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
mod tests {
    use super::super::spec::ComponentSpec;
    use super::*;
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
    fn expands_component_path() {
        let mut components = HashMap::new();
        components.insert(
            "studio".to_string(),
            ComponentSpec {
                path: "/tmp/studio".to_string(),
                stack: None,
                branch: None,
            },
        );
        let rig = rig_with("test", components);
        assert_eq!(
            expand_vars(&rig, "${components.studio.path}/dist"),
            "/tmp/studio/dist"
        );
    }

    #[test]
    fn expands_env_var() {
        std::env::set_var("RIG_TEST_VAR", "hello");
        let rig = rig_with("test", HashMap::new());
        assert_eq!(expand_vars(&rig, "x=${env.RIG_TEST_VAR}"), "x=hello");
    }

    #[test]
    fn leaves_unknown_token_literal() {
        let rig = rig_with("test", HashMap::new());
        assert_eq!(expand_vars(&rig, "${unknown.thing}"), "${unknown.thing}");
    }

    #[test]
    fn handles_unterminated_braces() {
        let rig = rig_with("test", HashMap::new());
        assert_eq!(expand_vars(&rig, "${unterminated"), "${unterminated");
    }

    #[test]
    fn no_substitution_when_no_dollar() {
        let rig = rig_with("test", HashMap::new());
        assert_eq!(expand_vars(&rig, "plain/path"), "plain/path");
    }
}
