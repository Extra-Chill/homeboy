use crate::component::Component;
use crate::error::Result;
use crate::project;

fn update_project_references(old_id: &str, new_id: &str) -> Result<()> {
    let projects = project::list().unwrap_or_default();
    for proj in projects {
        if project::has_component(&proj, old_id) {
            let updated_components: Vec<project::ProjectComponentAttachment> = proj
                .components
                .into_iter()
                .map(|mut component| {
                    if component.id == old_id {
                        component.id = new_id.to_string();
                    }
                    component
                })
                .collect();
            project::set_component_attachments(&proj.id, updated_components)?;
        }
    }
    Ok(())
}

/// Find project associations using the canonical project attachment model.
pub fn associated_projects(component_id: &str) -> Result<Vec<String>> {
    let projects = project::list().unwrap_or_default();
    Ok(projects
        .into_iter()
        .filter(|project| project::has_component(project, component_id))
        .map(|project| project.id)
        .collect())
}

pub fn projects_using(component_id: &str) -> Result<Vec<String>> {
    let projects = project::list().unwrap_or_default();
    Ok(projects
        .iter()
        .filter(|p| project::has_component(p, component_id))
        .map(|p| p.id.clone())
        .collect())
}

/// Returns a map of component_id -> Vec<project_id> for all components used by projects.
pub fn shared_components() -> Result<std::collections::HashMap<String, Vec<String>>> {
    let projects = project::list().unwrap_or_default();
    let mut sharing: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for project in projects {
        for component_id in project::project_component_ids(&project) {
            sharing
                .entry(component_id)
                .or_default()
                .push(project.id.clone());
        }
    }

    Ok(sharing)
}

pub fn rename_component(id: &str, new_id: &str) -> Result<Component> {
    let resolved_new_id = crate::engine::identifier::slugify_id(new_id, "component_id")?;
    let component = crate::component::mutate_portable(id, |component| {
        component.id = resolved_new_id.clone();
        Ok(())
    })?;
    update_project_references(id, &resolved_new_id)?;
    Ok(component)
}
