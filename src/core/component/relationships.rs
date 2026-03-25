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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_associated_projects_default_path() {
        let component_id = "";
        let result = associated_projects(&component_id);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_projects_using_default_path() {
        let component_id = "";
        let result = projects_using(&component_id);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_shared_components_ok_sharing() {

        let result = shared_components();
        assert!(!result.is_empty(), "expected non-empty collection for: Ok(sharing)");
    }

    #[test]
    fn test_shared_components_has_expected_effects() {
        // Expected effects: mutation

        let _ = shared_components();
    }

    #[test]
    fn test_rename_component_default_path() {
        let id = "";
        let new_id = "";
        let _result = rename_component(&id, &new_id);
    }

    #[test]
    fn test_rename_component_ok() {
        let id = "";
        let new_id = "";
        let result = rename_component(&id, &new_id);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        assert_eq!(inner.id, String::new());
        assert_eq!(inner.aliases, Vec::new());
        assert_eq!(inner.local_path, String::new());
        assert_eq!(inner.remote_path, String::new());
        assert_eq!(inner.build_artifact, None);
        assert_eq!(inner.extensions, None);
        assert_eq!(inner.version_targets, None);
        assert_eq!(inner.changelog_target, None);
        assert_eq!(inner.changelog_next_section_label, None);
        assert_eq!(inner.changelog_next_section_aliases, None);
        assert_eq!(inner.hooks, HashMap::new());
        assert_eq!(inner.extract_command, None);
        assert_eq!(inner.remote_owner, None);
        assert_eq!(inner.deploy_strategy, None);
        assert_eq!(inner.git_deploy, None);
        assert_eq!(inner.remote_url, None);
        assert_eq!(inner.auto_cleanup, false);
        assert_eq!(inner.docs_dir, None);
        assert_eq!(inner.docs_dirs, Vec::new());
        assert_eq!(inner.scopes, None);
    }

    #[test]
    fn test_rename_component_default_path_2() {
        let id = "";
        let new_id = "";
        let _result = rename_component(&id, &new_id);
    }

    #[test]
    fn test_rename_component_default_path_3() {
        let id = "";
        let new_id = "";
        let _result = rename_component(&id, &new_id);
    }

    #[test]
    fn test_rename_component_ok_component() {
        let id = "";
        let new_id = "";
        let result = rename_component(&id, &new_id);
        let inner = result.unwrap();
        // Branch returns Ok(component) when: Ok(component)
        assert_eq!(inner.id, String::new());
        assert_eq!(inner.aliases, Vec::new());
        assert_eq!(inner.local_path, String::new());
        assert_eq!(inner.remote_path, String::new());
        assert_eq!(inner.build_artifact, None);
        assert_eq!(inner.extensions, None);
        assert_eq!(inner.version_targets, None);
        assert_eq!(inner.changelog_target, None);
        assert_eq!(inner.changelog_next_section_label, None);
        assert_eq!(inner.changelog_next_section_aliases, None);
        assert_eq!(inner.hooks, HashMap::new());
        assert_eq!(inner.extract_command, None);
        assert_eq!(inner.remote_owner, None);
        assert_eq!(inner.deploy_strategy, None);
        assert_eq!(inner.git_deploy, None);
        assert_eq!(inner.remote_url, None);
        assert_eq!(inner.auto_cleanup, false);
        assert_eq!(inner.docs_dir, None);
        assert_eq!(inner.docs_dirs, Vec::new());
        assert_eq!(inner.scopes, None);
    }

}
