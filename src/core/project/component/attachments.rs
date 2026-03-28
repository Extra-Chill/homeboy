use std::path::Path;

use crate::error::{Error, Result};

use super::discovery::{discover_attached_component, infer_attached_component_id};
use crate::project::{load, save, Project, ProjectComponentAttachment};

fn component_ids_from_attachments(components: &[ProjectComponentAttachment]) -> Vec<String> {
    components
        .iter()
        .map(|component| component.id.clone())
        .collect()
}

pub fn project_component_ids(project: &Project) -> Vec<String> {
    component_ids_from_attachments(&project.components)
}

pub fn has_component(project: &Project, component_id: &str) -> bool {
    project
        .components
        .iter()
        .any(|component| component.id == component_id)
}

pub fn set_component_attachments(
    project_id: &str,
    components: Vec<ProjectComponentAttachment>,
) -> Result<Vec<String>> {
    if components.is_empty() {
        return Err(Error::validation_invalid_argument(
            "components",
            "At least one component attachment is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for component in components {
        if component.local_path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "components.local_path",
                "Project component attachments require a non-empty local_path",
                Some(project_id.to_string()),
                None,
            ));
        }
        if seen.insert(component.id.clone()) {
            deduped.push(component);
        }
    }

    let mut project = load(project_id)?;
    project.components = deduped;
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn remove_components(project_id: &str, component_ids: Vec<String>) -> Result<Vec<String>> {
    if component_ids.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "At least one component ID is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut project = load(project_id)?;

    let mut missing = Vec::new();
    for id in &component_ids {
        if !has_component(&project, id) {
            missing.push(id.clone());
        }
    }

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "Component IDs not attached to project",
            Some(project_id.to_string()),
            Some(missing),
        ));
    }

    project
        .components
        .retain(|component| !component_ids.contains(&component.id));
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn clear_component_attachments(project_id: &str) -> Result<Vec<String>> {
    let mut project = load(project_id)?;
    project.components.clear();
    save(&project)?;
    Ok(project_component_ids(&project))
}

pub fn attach_component_path(project_id: &str, component_id: &str, local_path: &str) -> Result<()> {
    let mut project = load(project_id)?;

    let is_update = project.components.iter().any(|c| c.id == component_id);

    // When updating an existing component's path, preserve the current remote_path
    // as a project override if the new path's homeboy.json doesn't provide one.
    // This prevents clean tag clones (whose homeboy.json omits remote_path) from
    // blanking the deploy target. (#932)
    if is_update {
        preserve_remote_path_on_reattach(&mut project, component_id, local_path);
    }

    if let Some(component) = project.components.iter_mut().find(|c| c.id == component_id) {
        component.local_path = local_path.to_string();
    } else {
        project.components.push(ProjectComponentAttachment {
            id: component_id.to_string(),
            local_path: local_path.to_string(),
        });
    }

    save(&project)
}

/// When re-attaching a component to a new path, check whether the current remote_path
/// would be lost. If the existing resolved component has a non-empty remote_path and the
/// new path's homeboy.json doesn't provide one, store the current value as a project
/// override so deploy config survives path changes.
fn preserve_remote_path_on_reattach(
    project: &mut Project,
    component_id: &str,
    new_local_path: &str,
) {
    // Already has a project-level remote_path override — nothing to preserve.
    if let Some(overrides) = project.component_overrides.get(component_id) {
        if overrides.remote_path.is_some() {
            return;
        }
    }

    // Resolve the current component to capture its remote_path.
    let current_attachment = project.components.iter().find(|c| c.id == component_id);
    let current_remote_path = current_attachment
        .and_then(|a| discover_attached_component(Path::new(&a.local_path)))
        .map(|c| c.remote_path.clone())
        .unwrap_or_default();

    if current_remote_path.trim().is_empty() {
        return;
    }

    // Check whether the new path's homeboy.json provides a remote_path.
    let new_remote_path = discover_attached_component(Path::new(new_local_path))
        .map(|c| c.remote_path.clone())
        .unwrap_or_default();

    if !new_remote_path.trim().is_empty() {
        return; // New path provides its own remote_path — no need to preserve.
    }

    // The new path would blank remote_path. Store the current value as an override.
    crate::log_status!(
        "project",
        "Preserving remote_path '{}' as project override for '{}' (new path's homeboy.json omits it)",
        current_remote_path,
        component_id
    );

    let overrides = project
        .component_overrides
        .entry(component_id.to_string())
        .or_default();
    overrides.remote_path = Some(current_remote_path);
}

pub fn attach_discovered_component_path(project_id: &str, local_path: &Path) -> Result<String> {
    let inferred_id = infer_attached_component_id(local_path)?;

    // When the inferred ID doesn't match any existing project component, check
    // whether a directory-name fallback produced a version-stamped ID (e.g.
    // "data-machine-v0402-clean" from a clone path). If an existing component
    // whose ID is a prefix of the inferred ID exists, prefer the existing ID.
    // This prevents component identity mutation from clone directory names. (#932)
    let project = load(project_id)?;
    let component_id = if has_component(&project, &inferred_id) {
        inferred_id
    } else {
        find_prefix_match(&project, &inferred_id).unwrap_or(inferred_id)
    };

    attach_component_path(project_id, &component_id, &local_path.to_string_lossy())?;
    Ok(component_id)
}

/// Find an existing project component whose ID is a prefix of the inferred ID.
///
/// When a clean clone directory name like "data-machine-v0.40.2-clean" gets slugified
/// to "data-machine-v0402-clean", the real component ID "data-machine" is a prefix.
/// This function detects that pattern and returns the existing component's ID.
///
/// Only matches if:
/// - The existing ID is a proper prefix of the inferred ID
/// - The character after the prefix is a separator (dash followed by 'v' + digit,
///   or just a digit), suggesting a version/tag suffix
fn find_prefix_match(project: &Project, inferred_id: &str) -> Option<String> {
    let mut best_match: Option<&str> = None;

    for attachment in &project.components {
        let existing_id = &attachment.id;
        if inferred_id.starts_with(existing_id.as_str()) && inferred_id.len() > existing_id.len() {
            let suffix = &inferred_id[existing_id.len()..];
            // The suffix should look like a version/clone qualifier: "-v...", "-0...", etc.
            if let Some(after_dash) = suffix.strip_prefix('-') {
                let is_version_like = after_dash.starts_with('v')
                    || after_dash
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit());
                if is_version_like {
                    // Prefer the longest matching prefix (most specific existing component)
                    if best_match.is_none_or(|prev| existing_id.len() > prev.len()) {
                        best_match = Some(existing_id);
                    }
                }
            }
        }
    }

    best_match.map(|id| {
        crate::log_status!(
            "project",
            "Matched inferred ID '{}' to existing component '{}' (directory name appears version-stamped)",
            inferred_id,
            id
        );
        id.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_components(ids: &[&str]) -> Project {
        let mut project = Project::default();
        for id in ids {
            project.components.push(ProjectComponentAttachment {
                id: id.to_string(),
                local_path: format!("/workspace/{}", id),
            });
        }
        project
    }

    #[test]
    fn find_prefix_match_version_suffix() {
        let project = project_with_components(&["data-machine", "chubes-theme"]);
        // Clone dir "data-machine-v0402-clean" → slugified inferred ID
        assert_eq!(
            find_prefix_match(&project, "data-machine-v0402-clean"),
            Some("data-machine".to_string()),
        );
    }

    #[test]
    fn find_prefix_match_numeric_suffix() {
        let project = project_with_components(&["data-machine"]);
        // Clone dir "data-machine-0402" → numeric version suffix
        assert_eq!(
            find_prefix_match(&project, "data-machine-0402"),
            Some("data-machine".to_string()),
        );
    }

    #[test]
    fn find_prefix_match_no_match_non_version_suffix() {
        let project = project_with_components(&["data-machine"]);
        // "data-machine-socials" is NOT a version suffix, it's a different component
        assert_eq!(find_prefix_match(&project, "data-machine-socials"), None);
    }

    #[test]
    fn find_prefix_match_exact_match_not_prefix() {
        let project = project_with_components(&["data-machine"]);
        // Exact match — not a prefix scenario
        assert_eq!(find_prefix_match(&project, "data-machine"), None);
    }

    #[test]
    fn find_prefix_match_prefers_longest() {
        let project = project_with_components(&["data", "data-machine"]);
        // Both "data" and "data-machine" are prefixes, but "data-machine" is longer
        assert_eq!(
            find_prefix_match(&project, "data-machine-v1"),
            Some("data-machine".to_string()),
        );
    }

    #[test]
    fn find_prefix_match_no_components() {
        let project = project_with_components(&[]);
        assert_eq!(find_prefix_match(&project, "anything-v1"), None);
    }
}
