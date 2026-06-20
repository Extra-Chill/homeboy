//! Entity suggestion utilities for unrecognized CLI subcommands.

use homeboy::core::engine::text::levenshtein;
use homeboy::core::{component, extension, project, server};
use std::sync::{OnceLock, RwLock};

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

#[derive(Debug, Clone)]
struct EntityIdList {
    entity_type: EntityType,
    ids: Vec<String>,
}

static ENTITY_SUGGESTION_SNAPSHOT: OnceLock<RwLock<Option<Vec<EntityIdList>>>> = OnceLock::new();

fn entity_suggestion_cache() -> &'static RwLock<Option<Vec<EntityIdList>>> {
    ENTITY_SUGGESTION_SNAPSHOT.get_or_init(|| RwLock::new(None))
}

fn load_entity_suggestion_snapshot() -> Vec<EntityIdList> {
    vec![
        EntityIdList {
            entity_type: EntityType::Component,
            ids: component::list_ids().unwrap_or_default(),
        },
        EntityIdList {
            entity_type: EntityType::Project,
            ids: project::list_ids().unwrap_or_default(),
        },
        EntityIdList {
            entity_type: EntityType::Server,
            ids: server::list()
                .unwrap_or_default()
                .into_iter()
                .map(|server| server.id)
                .collect(),
        },
        EntityIdList {
            entity_type: EntityType::Extension,
            ids: extension::available_extension_ids(),
        },
    ]
}

fn entity_suggestion_snapshot() -> Vec<EntityIdList> {
    if let Some(snapshot) = match entity_suggestion_cache().read() {
        Ok(slot) => slot.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    } {
        return snapshot;
    }

    let snapshot = load_entity_suggestion_snapshot();
    match entity_suggestion_cache().write() {
        Ok(mut slot) => *slot = Some(snapshot.clone()),
        Err(poisoned) => *poisoned.into_inner() = Some(snapshot.clone()),
    }
    snapshot
}

#[cfg(test)]
pub(crate) fn reset_entity_suggestion_cache_for_test() {
    match entity_suggestion_cache().write() {
        Ok(mut slot) => *slot = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

pub fn find_entity_match(input: &str) -> Option<EntityMatch> {
    let input_lower = input.to_lowercase();

    for entry in entity_suggestion_snapshot() {
        if let Some(m) = find_match_in_list(&input_lower, &entry.ids) {
            return Some(EntityMatch {
                entity_type: entry.entity_type,
                entity_id: m.0,
                exact: m.1,
            });
        }
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

pub fn generate_entity_hints(
    entity_match: &EntityMatch,
    parent_command: &str,
    unrecognized: &str,
) -> Vec<String> {
    let id = &entity_match.entity_id;
    let entity_label = entity_match.entity_type.label();
    let mut hints = Vec::new();

    if !entity_match.exact {
        hints.push(format!("Did you mean {} '{}' ?", entity_label, id).replace("' ?", "'?"));
    }

    match parent_command {
        "changelog" => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy changelog show {}",
            unrecognized, entity_label, id, id
        )),
        "version" => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy version show {}",
            unrecognized, entity_label, id, id
        )),
        "build" => hints.push(format!(
            "'{}' matches {} '{}'. Run: homeboy build {}",
            unrecognized, entity_label, id, id
        )),
        "deploy" => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy deploy --component {}",
            unrecognized, entity_label, id, id
        )),
        "changes" => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy changes show {}",
            unrecognized, entity_label, id, id
        )),
        "git" => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy git {} status",
            unrecognized, entity_label, id, id
        )),
        "release" => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy release {}",
            unrecognized, entity_label, id, id
        )),
        _ => hints.push(format!(
            "'{}' matches {} '{}'. Try: homeboy {} {}",
            unrecognized, entity_label, id, entity_label, id
        )),
    }

    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::project::{self, Project};

    #[test]
    fn isolated_home_guard_resets_entity_suggestion_cache_between_homes() {
        with_isolated_home(|_| {
            project::save(&Project {
                id: "cached-project".to_string(),
                ..Project::default()
            })
            .expect("save project");

            let matched = find_entity_match("cached-project").expect("project match");
            assert_eq!(matched.entity_type, EntityType::Project);
            assert_eq!(matched.entity_id, "cached-project");
        });

        with_isolated_home(|_| {
            assert!(find_entity_match("cached-project").is_none());
        });
    }
}
