use crate::error::{Error, Result};
use crate::project::Project;

use super::discovery::discover_attached_component;
use super::overrides::apply_component_overrides;

pub fn resolve_project_component(
    project: &Project,
    component_id: &str,
) -> Result<crate::component::Component> {
    let component = if let Some(attachment) = project
        .components
        .iter()
        .find(|component| component.id == component_id)
    {
        discover_attached_component(std::path::Path::new(&attachment.local_path)).ok_or_else(
            || {
                Error::validation_invalid_argument(
                    "components.local_path",
                    format!(
                        "Project component '{}' points to '{}' but no homeboy.json was found",
                        component_id, attachment.local_path
                    ),
                    Some(project.id.clone()),
                    None,
                )
            },
        )?
    } else {
        return Err(Error::validation_invalid_argument(
            "components",
            format!(
                "Project '{}' has no attached component '{}'",
                project.id, component_id
            ),
            Some(project.id.clone()),
            None,
        ));
    };

    let mut resolved = apply_component_overrides(&component, project);

    // Auto-resolve remote_path if still empty after all config layers.
    // Repo homeboy.json intentionally omits remote_path (it's deploy config),
    // so auto-detect it from source files when possible (#812).
    resolved.resolve_remote_path();

    Ok(resolved)
}

pub fn resolve_project_components(project: &Project) -> Result<Vec<crate::component::Component>> {
    project
        .components
        .iter()
        .map(|component| resolve_project_component(project, &component.id))
        .collect()
}
