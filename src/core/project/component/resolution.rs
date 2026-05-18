use crate::core::error::{Error, Result};
use crate::core::project::Project;

use super::discovery::discover_attached_component;
use super::overrides::apply_component_overrides;

pub fn resolve_project_component(
    project: &Project,
    component_id: &str,
) -> Result<crate::core::component::Component> {
    let (mut component, attachment_remote_path) = if let Some(attachment) = project
        .components
        .iter()
        .find(|component| component.id == component_id)
    {
        (
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
            )?,
            attachment.remote_path.clone(),
        )
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

    if let Some(remote_path) = attachment_remote_path {
        if !remote_path.trim().is_empty() {
            component.remote_path = remote_path;
        }
    }

    let mut resolved = apply_component_overrides(&component, project);

    // Inherit project-level extensions when the component's homeboy.json doesn't
    // declare any. This handles clean tag clones from older releases where
    // extensions weren't yet in homeboy.json. (#932)
    if resolved.extensions.is_none() || resolved.extensions.as_ref().is_some_and(|e| e.is_empty()) {
        if let Some(project_extensions) = &project.extensions {
            if !project_extensions.is_empty() {
                resolved.extensions = Some(project_extensions.clone());
            }
        }
    }

    // Auto-resolve remote_path if still empty after all config layers.
    // Repo homeboy.json intentionally omits remote_path (it's deploy config),
    // so auto-detect it from source files when possible (#812).
    resolved.resolve_remote_path();

    Ok(resolved)
}

pub fn resolve_project_components(
    project: &Project,
) -> Result<Vec<crate::core::component::Component>> {
    project
        .components
        .iter()
        .map(|component| resolve_project_component(project, &component.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project::{ProjectComponentAttachment, ProjectComponentOverrides};
    use tempfile::TempDir;

    fn repo_with_portable_remote_path(remote_path: &str) -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        std::fs::write(
            dir.path().join("homeboy.json"),
            serde_json::json!({
                "id": "fixture",
                "remote_path": remote_path,
                "build_artifact": "dist/fixture.zip"
            })
            .to_string(),
        )
        .expect("write homeboy.json");
        dir
    }

    fn project_with_attachment(remote_path: Option<&str>, local_path: String) -> Project {
        Project {
            id: "site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "fixture".to_string(),
                local_path,
                remote_path: remote_path.map(str::to_string),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn attachment_remote_path_overrides_portable_remote_path() {
        let repo = repo_with_portable_remote_path("../wp-content/plugins/fixture");
        let project = project_with_attachment(
            Some("wp-content/plugins/fixture"),
            repo.path().to_string_lossy().to_string(),
        );

        let component = resolve_project_component(&project, "fixture").expect("component");

        assert_eq!(component.remote_path, "wp-content/plugins/fixture");
    }

    #[test]
    fn component_overrides_still_win_over_attachment_remote_path() {
        let repo = repo_with_portable_remote_path("portable/plugins/fixture");
        let mut project = project_with_attachment(
            Some("attachment/plugins/fixture"),
            repo.path().to_string_lossy().to_string(),
        );
        project.component_overrides.insert(
            "fixture".to_string(),
            ProjectComponentOverrides {
                remote_path: Some("override/plugins/fixture".to_string()),
                ..Default::default()
            },
        );

        let component = resolve_project_component(&project, "fixture").expect("component");

        assert_eq!(component.remote_path, "override/plugins/fixture");
    }
}
