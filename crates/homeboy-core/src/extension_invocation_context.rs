use std::collections::HashMap;

use crate::component::{self, Component};
use crate::engine::template;
use crate::error::{Error, Result};
use crate::project::{self, Project};

use crate::extension_scope::ExtensionScope;
use homeboy_extension_contract::ExtensionManifest;

/// The resolved project/component/settings identity for one extension invocation.
///
/// Resolve this once at the command boundary. Execution code must use the retained
/// component instead of resolving its ID again, because a project attachment can
/// deliberately differ from the globally registered component path.
#[derive(Debug, Clone)]
pub struct ResolvedExtensionInvocationContext {
    pub extension_id: String,
    pub project_id: Option<String>,
    pub project: Option<Project>,
    pub component: Option<Component>,
    pub settings: HashMap<String, serde_json::Value>,
}

impl ResolvedExtensionInvocationContext {
    pub fn for_component(
        extension_id: &str,
        project: Option<Project>,
        component: Component,
    ) -> Result<Self> {
        let settings =
            ExtensionScope::effective_settings(extension_id, project.as_ref(), Some(&component))?;

        Ok(Self {
            extension_id: extension_id.to_string(),
            project_id: project.as_ref().map(|project| project.id.clone()),
            project,
            component: Some(component),
            settings,
        })
    }

    pub fn resolve_runtime(
        extension: &ExtensionManifest,
        extension_id: &str,
        project_id: Option<&str>,
        component_id: Option<&str>,
        run_command: &str,
    ) -> Result<Self> {
        let requires_project = extension.requires.is_some()
            || template::is_present(run_command, "projectId")
            || template::is_present(run_command, "sitePath")
            || template::is_present(run_command, "cliPath")
            || template::is_present(run_command, "domain");

        let project = if requires_project {
            let project_id = project_id.ok_or_else(|| {
                Error::config(format!(
                    "Extension {} requires a project context, but no project ID was provided",
                    extension.id
                ))
            })?;
            let project = project::load(project_id)?;
            ExtensionScope::validate_project_compatibility(extension, &project)?;
            Some(project)
        } else {
            None
        };

        let component_id = match project.as_ref() {
            Some(project) => {
                ExtensionScope::resolve_component_scope(extension, project, component_id)?
            }
            None => component_id.map(str::to_string),
        };
        let component = component_id
            .as_deref()
            .map(|component_id| {
                component::resolve_effective(Some(component_id), None, project.as_ref())
            })
            .transpose()?;

        let settings =
            ExtensionScope::effective_settings(extension_id, project.as_ref(), component.as_ref())?;

        Ok(Self {
            extension_id: extension_id.to_string(),
            project_id: project.as_ref().map(|project| project.id.clone()),
            project,
            component,
            settings,
        })
    }
}
