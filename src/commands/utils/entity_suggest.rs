//! Entity suggestion utilities for unrecognized CLI subcommands.

use homeboy::engine::text::levenshtein;
use homeboy::{component, extension, project, server};

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

#[derive(Debug, Clone)]
pub struct EntityMatch {
    pub entity_type: EntityType,
    pub entity_id: String,
    pub exact: bool,
}

pub fn find_entity_match(input: &str) -> Option<EntityMatch> {
    let input_lower = input.to_lowercase();

    if let Ok(ids) = component::list_ids() {
        if let Some(m) = find_match_in_list(&input_lower, &ids) {
            return Some(EntityMatch { entity_type: EntityType::Component, entity_id: m.0, exact: m.1 });
        }
    }
    if let Ok(ids) = project::list_ids() {
        if let Some(m) = find_match_in_list(&input_lower, &ids) {
            return Some(EntityMatch { entity_type: EntityType::Project, entity_id: m.0, exact: m.1 });
        }
    }
    if let Ok(servers) = server::list() {
        let ids: Vec<String> = servers.into_iter().map(|s| s.id).collect();
        if let Some(m) = find_match_in_list(&input_lower, &ids) {
            return Some(EntityMatch { entity_type: EntityType::Server, entity_id: m.0, exact: m.1 });
        }
    }
    let extension_ids = extension::available_extension_ids();
    if let Some(m) = find_match_in_list(&input_lower, &extension_ids) {
        return Some(EntityMatch { entity_type: EntityType::Extension, entity_id: m.0, exact: m.1 });
    }
    None
}

fn find_match_in_list(input_lower: &str, ids: &[String]) -> Option<(String, bool)> {
    for id in ids {
        if id.to_lowercase() == *input_lower {
            return Some((id.clone(), true));
        }
    }
    for id in ids {
        if id.to_lowercase().starts_with(input_lower) {
            return Some((id.clone(), false));
        }
    }
    for id in ids {
        if id.to_lowercase().ends_with(input_lower) {
            return Some((id.clone(), false));
        }
    }
    for id in ids {
        let dist = levenshtein(input_lower, &id.to_lowercase());
        if dist <= 3 && dist > 0 {
            return Some((id.clone(), false));
        }
    }
    None
}

pub fn generate_entity_hints(entity_match: &EntityMatch, parent_command: &str, unrecognized: &str) -> Vec<String> {
    let id = &entity_match.entity_id;
    let entity_label = entity_match.entity_type.label();
    let mut hints = Vec::new();

    if !entity_match.exact {
        hints.push(format!("Did you mean {} '{}' ?", entity_label, id).replace("' ?", "'?"));
    }

    match parent_command {
        "changelog" => hints.push(format!("'{}' matches {} '{}'. Try: homeboy changelog show {}", unrecognized, entity_label, id, id)),
        "version" => hints.push(format!("'{}' matches {} '{}'. Try: homeboy version show {}", unrecognized, entity_label, id, id)),
        "build" => hints.push(format!("'{}' matches {} '{}'. Run: homeboy build {}", unrecognized, entity_label, id, id)),
        "deploy" => hints.push(format!("'{}' matches {} '{}'. Try: homeboy deploy --component {}", unrecognized, entity_label, id, id)),
        "changes" => hints.push(format!("'{}' matches {} '{}'. Try: homeboy changes show {}", unrecognized, entity_label, id, id)),
        "git" => hints.push(format!("'{}' matches {} '{}'. Try: homeboy git {} status", unrecognized, entity_label, id, id)),
        "release" => hints.push(format!("'{}' matches {} '{}'. Try: homeboy release {}", unrecognized, entity_label, id, id)),
        _ => hints.push(format!("'{}' matches {} '{}'. Try: homeboy {} {}", unrecognized, entity_label, id, entity_label, id)),
    }

    hints
}
