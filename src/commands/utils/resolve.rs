//! Project/component argument resolution helpers for CLI commands.

use homeboy::{component, project, Error, Result};

pub fn resolve_project_components(first: &str, rest: &[String]) -> Result<(String, Vec<String>)> {
    let projects = project::list_ids().unwrap_or_default();
    let components = component::list_ids().unwrap_or_default();

    if projects.contains(&first.to_string()) {
        Ok((first.to_string(), rest.to_vec()))
    } else if components.contains(&first.to_string()) {
        if let Some(project_idx) = rest.iter().position(|r| projects.contains(r)) {
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
            let mut all_component_ids = vec![first.to_string()];
            all_component_ids.extend(rest.iter().filter(|r| components.contains(*r)).cloned());

            if let Some(project_id) = infer_project_for_components(&all_component_ids) {
                Ok((project_id, all_component_ids))
            } else {
                let associated_projects = component::projects_using(first).unwrap_or_default();

                let hint = if associated_projects.is_empty() {
                    format!(
                        "Component '{}' is not associated with any project.\n  Add it to a project: homeboy project components add <project> {}\n  Or attach a repo directly: homeboy project components attach-path <project> {} <path>",
                        first, first, first
                    )
                } else if associated_projects.len() == 1 {
                    format!(
                        "Component '{}' belongs to project '{}'.\n  Run: homeboy deploy {} {}",
                        first, associated_projects[0], associated_projects[0], first
                    )
                } else {
                    format!(
                        "Component '{}' belongs to multiple projects: {}.\n  Specify the project explicitly: homeboy deploy <project> {}",
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

pub fn infer_project_for_components(component_ids: &[String]) -> Option<String> {
    if component_ids.is_empty() {
        return None;
    }

    let mut common_projects: Option<Vec<String>> = None;

    for comp_id in component_ids {
        let projects = component::projects_using(comp_id).unwrap_or_default();
        if projects.is_empty() {
            return None;
        }

        match &mut common_projects {
            None => common_projects = Some(projects),
            Some(current) => {
                current.retain(|p| projects.contains(p));
                if current.is_empty() {
                    return None;
                }
            }
        }
    }

    common_projects.and_then(|p| {
        if p.len() == 1 {
            Some(p.into_iter().next().unwrap())
        } else {
            None
        }
    })
}
