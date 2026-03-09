//! Entity suggestion utilities for unrecognized CLI subcommands.
//!
//! Provides fuzzy matching against known entities (components, projects, servers, extensions)
//! and generates helpful hints when users mistype command syntax.

use super::token::levenshtein;
use crate::{component, extension, project, server};

/// Types of entities that can be suggested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityType {
    Component,
    Project,
    Server,
    Extension,
}

impl EntityType {
    pub fn label(&self) -> &'static str {
        match self {
            EntityType::Component => "component",
            EntityType::Project => "project",
            EntityType::Server => "server",
            EntityType::Extension => "extension",
        }
    }
}

/// Result of matching an input string against known entities.
#[derive(Debug, Clone)]
pub struct EntityMatch {
    pub entity_type: EntityType,
    pub entity_id: String,
    pub exact: bool,
}

/// Check if a string matches any known entity (exact or fuzzy).
/// Returns the first match found, checking in order: components, projects, servers, extensions.
pub fn find_entity_match(input: &str) -> Option<EntityMatch> {
    let input_lower = input.to_lowercase();

    // Check components
    if let Ok(ids) = component::list_ids() {
        if let Some(m) = find_match_in_list(&input_lower, &ids) {
            return Some(EntityMatch {
                entity_type: EntityType::Component,
                entity_id: m.0,
                exact: m.1,
            });
        }
    }

    // Check projects
    if let Ok(ids) = project::list_ids() {
        if let Some(m) = find_match_in_list(&input_lower, &ids) {
            return Some(EntityMatch {
                entity_type: EntityType::Project,
                entity_id: m.0,
                exact: m.1,
            });
        }
    }

    // Check servers
    if let Ok(servers) = server::list() {
        let ids: Vec<String> = servers.into_iter().map(|s| s.id).collect();
        if let Some(m) = find_match_in_list(&input_lower, &ids) {
            return Some(EntityMatch {
                entity_type: EntityType::Server,
                entity_id: m.0,
                exact: m.1,
            });
        }
    }

    // Check extensions
    let extension_ids = extension::available_extension_ids();
    if let Some(m) = find_match_in_list(&input_lower, &extension_ids) {
        return Some(EntityMatch {
            entity_type: EntityType::Extension,
            entity_id: m.0,
            exact: m.1,
        });
    }

    None
}

/// Find a match in a list of IDs. Returns (matched_id, is_exact).
fn find_match_in_list(input_lower: &str, ids: &[String]) -> Option<(String, bool)> {
    // Exact match first
    for id in ids {
        if id.to_lowercase() == *input_lower {
            return Some((id.clone(), true));
        }
    }

    // Prefix match (input is prefix of existing)
    for id in ids {
        if id.to_lowercase().starts_with(input_lower) {
            return Some((id.clone(), false));
        }
    }

    // Suffix match (input is suffix of existing)
    for id in ids {
        if id.to_lowercase().ends_with(input_lower) {
            return Some((id.clone(), false));
        }
    }

    // Levenshtein distance <= 3
    for id in ids {
        let dist = levenshtein(input_lower, &id.to_lowercase());
        if dist <= 3 && dist > 0 {
            return Some((id.clone(), false));
        }
    }

    None
}

/// Generate hint messages for a matched entity based on the parent command context.
pub fn generate_entity_hints(
    entity_match: &EntityMatch,
    parent_command: &str,
    unrecognized: &str,
) -> Vec<String> {
    let id = &entity_match.entity_id;
    let entity_label = entity_match.entity_type.label();
    let mut hints = Vec::new();

    // Add fuzzy match hint if not exact
    if !entity_match.exact {
        hints.push(format!("Did you mean {} '{}'?", entity_label, id));
    }

    // Command-specific suggestions
    match parent_command {
        "changelog" => {
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy changelog show {}",
                unrecognized, entity_label, id, id
            ));
        }
        "version" => {
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy version show {}",
                unrecognized, entity_label, id, id
            ));
        }
        "build" => {
            // Build already accepts component ID directly
            hints.push(format!(
                "'{}' matches {} '{}'. Run: homeboy build {}",
                unrecognized, entity_label, id, id
            ));
        }
        "deploy" => {
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy deploy --component {}",
                unrecognized, entity_label, id, id
            ));
        }
        "changes" => {
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy changes show {}",
                unrecognized, entity_label, id, id
            ));
        }
        "git" => {
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy git {} status",
                unrecognized, entity_label, id, id
            ));
        }
        "release" => {
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy release {}",
                unrecognized, entity_label, id, id
            ));
        }
        _ => {
            // Generic hint for other commands
            hints.push(format!(
                "'{}' matches {} '{}'. Try: homeboy {} {}",
                unrecognized, entity_label, id, entity_label, id
            ));
        }
    }

    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_match_in_list_exact() {
        let ids = vec!["foo".to_string(), "bar".to_string(), "baz".to_string()];
        let result = find_match_in_list("foo", &ids);
        assert!(result.is_some());
        let (id, exact) = result.unwrap();
        assert_eq!(id, "foo");
        assert!(exact);
    }

    #[test]
    fn test_find_match_in_list_prefix() {
        let ids = vec!["foobar".to_string(), "bar".to_string()];
        let result = find_match_in_list("foo", &ids);
        assert!(result.is_some());
        let (id, exact) = result.unwrap();
        assert_eq!(id, "foobar");
        assert!(!exact);
    }

    #[test]
    fn test_find_match_in_list_suffix() {
        let ids = vec!["prefoo".to_string(), "bar".to_string()];
        let result = find_match_in_list("foo", &ids);
        assert!(result.is_some());
        let (id, exact) = result.unwrap();
        assert_eq!(id, "prefoo");
        assert!(!exact);
    }

    #[test]
    fn test_find_match_in_list_fuzzy() {
        let ids = vec!["foa".to_string(), "xyz".to_string()];
        let result = find_match_in_list("foo", &ids);
        assert!(result.is_some());
        let (id, exact) = result.unwrap();
        assert_eq!(id, "foa");
        assert!(!exact);
    }

    #[test]
    fn test_find_match_in_list_no_match() {
        // Use strings with Levenshtein distance > 3 from "foo"
        let ids = vec!["xyzabc".to_string(), "qwerty".to_string()];
        let result = find_match_in_list("foo", &ids);
        assert!(result.is_none());
    }

    #[test]
    fn test_entity_type_label() {
        assert_eq!(EntityType::Component.label(), "component");
        assert_eq!(EntityType::Project.label(), "project");
        assert_eq!(EntityType::Server.label(), "server");
        assert_eq!(EntityType::Extension.label(), "extension");
    }
}
