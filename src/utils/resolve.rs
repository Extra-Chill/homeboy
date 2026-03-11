//! Project/component argument resolution utilities.
//!
//! Provides centralized logic for resolving ambiguous project/component arguments
//! across all commands, supporting flexible argument order and inference.

use crate::{component, project, Error, Result};

/// Resolve ambiguous project/component arguments.
///
/// Supports both orderings: `<project> <component>` OR `<component> <project>`
///
/// # Inference rules
/// - If component belongs to exactly ONE project: auto-infer
/// - If component belongs to MULTIPLE projects: error with project list
/// - If component belongs to NO projects: error with hint to add it
///
/// # Returns
/// `(project_id, component_ids)` or helpful error
pub fn resolve_project_components(first: &str, rest: &[String]) -> Result<(String, Vec<String>)> {
    let projects = project::list_ids().unwrap_or_default();
    let components = component::list_ids().unwrap_or_default();

    if projects.contains(&first.to_string()) {
        // Standard order: project first
        Ok((first.to_string(), rest.to_vec()))
    } else if components.contains(&first.to_string()) {
        // Component first - find project in rest or infer from context
        if let Some(project_idx) = rest.iter().position(|r| projects.contains(r)) {
            // Project found in remaining args
            let project = rest[project_idx].clone();
            let mut comps = vec![first.to_string()];
            comps.extend(
                rest.iter()
                    .enumerate()
                    .filter(|(i, _)| *i != project_idx)
                    .map(|(_, s)| s.clone()),
            );
            Ok((project, comps))
        } else {
            // No project in args - try to infer from component's project associations
            let mut all_component_ids = vec![first.to_string()];
            all_component_ids.extend(rest.iter().filter(|r| components.contains(*r)).cloned());

            if let Some(project_id) = infer_project_for_components(&all_component_ids) {
                Ok((project_id, all_component_ids))
            } else {
                // Build helpful error message
                let associated_projects = component::associated_projects(first).unwrap_or_default();

                let hint = if associated_projects.is_empty() {
                    format!(
                        "Component '{}' is not associated with any project.\n  \
                        Add it to a project: homeboy project components add <project> {}\n  \
                        Or attach a repo directly: homeboy project components attach-path <project> {} <path>",
                        first, first, first
                    )
                } else if associated_projects.len() == 1 {
                    format!(
                        "Component '{}' belongs to project '{}'.\n  \
                        Run: homeboy deploy {} {}",
                        first, associated_projects[0], associated_projects[0], first
                    )
                } else {
                    format!(
                        "Component '{}' belongs to multiple projects: {}.\n  \
                        Specify the project explicitly: homeboy deploy <project> {}",
                        first,
                        associated_projects.join(", "),
                        first
                    )
                };

                Err(Error::validation_invalid_argument(
                    "project_id",
                    "No project ID found in arguments and could not be inferred",
                    None,
                    Some(vec![hint]),
                ))
            }
        }
    } else {
        // First arg is neither - provide helpful error
        Err(Error::validation_invalid_argument(
            "project_id",
            format!("'{}' is not a known project or component", first),
            None,
            Some(vec![
                format!("Available projects: {}", projects.join(", ")),
                format!("Available components: {}", components.join(", ")),
            ]),
        ))
    }
}

/// Infer project for a set of components.
///
/// Returns the project ID only if ALL components belong to exactly ONE common project.
///
/// # Returns
/// - `Some(project_id)` if there's exactly one common project
/// - `None` if:
///   - Any component has no project association
///   - Components belong to different projects
///   - Multiple projects contain all the components (ambiguous)
pub fn infer_project_for_components(component_ids: &[String]) -> Option<String> {
    if component_ids.is_empty() {
        return None;
    }

    // Get projects for each component
    let mut common_projects: Option<Vec<String>> = None;

    for comp_id in component_ids {
        let projects = component::associated_projects(comp_id).unwrap_or_default();
        if projects.is_empty() {
            return None; // Component has no project
        }

        match &mut common_projects {
            None => common_projects = Some(projects),
            Some(current) => {
                // Keep only projects that contain all components
                current.retain(|p| projects.contains(p));
                if current.is_empty() {
                    return None; // No common project
                }
            }
        }
    }

    // Return the project only if there's exactly one common project
    common_projects.and_then(|p| {
        if p.len() == 1 {
            Some(p.into_iter().next().unwrap())
        } else {
            None // Ambiguous - multiple projects contain these components
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_project_for_empty_components() {
        assert_eq!(infer_project_for_components(&[]), None);
    }
}
