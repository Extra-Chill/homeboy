//! Entity suggestion utilities for unrecognized CLI subcommands.
//!
//! Provides fuzzy matching against known entities (components, projects, servers, modules)
//! and generates helpful hints when users mistype command syntax.

use crate::{component, module, project, server};

/// Types of entities that can be suggested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityType {
    Component,
    Project,
    Server,
    Module,
}

impl EntityType {
    pub fn label(&self) -> &'static str {
        match self {
            EntityType::Component => "component",
            EntityType::Project => "project",
            EntityType::Server => "server",
            EntityType::Module => "module",
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
/// Returns the first match found, checking in order: components, projects, servers, modules.
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

    // Check modules
    let module_ids = module::available_module_ids();
    if let Some(m) = find_match_in_list(&input_lower, &module_ids) {
        return Some(EntityMatch {
            entity_type: EntityType::Module,
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

/// Simple Levenshtein distance implementation.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for i in 0..=a_len {
        matrix[i][0] = i;
    }
    for j in 0..=b_len {
        matrix[0][j] = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }

    matrix[a_len][b_len]
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
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy changelog show {}", unrecognized, entity_label, id, id));
        }
        "version" => {
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy version show {}", unrecognized, entity_label, id, id));
        }
        "build" => {
            // Build already accepts component ID directly
            hints.push(format!("'{}' matches {} '{}'. Run: homeboy build {}", unrecognized, entity_label, id, id));
        }
        "deploy" => {
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy deploy --component {}", unrecognized, entity_label, id, id));
        }
        "changes" => {
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy changes show {}", unrecognized, entity_label, id, id));
        }
        "git" => {
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy git {} status", unrecognized, entity_label, id, id));
        }
        "release" => {
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy release {}", unrecognized, entity_label, id, id));
        }
        _ => {
            // Generic hint for other commands
            hints.push(format!("'{}' matches {} '{}'. Try: homeboy {} {}", unrecognized, entity_label, id, entity_label, id));
        }
    }

    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn test_levenshtein_equal() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
    }

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
        assert_eq!(EntityType::Module.label(), "module");
    }
}
