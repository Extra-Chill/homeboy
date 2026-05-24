use homeboy::core::ci_profile::{self, CiResolvedJob};
use homeboy::core::component::Component;
use homeboy::core::engine::execution_context::{self, ExecutionContext, ResolveOptions};
use homeboy::core::extension::ExtensionCapability;

#[derive(Clone, Debug)]
pub(crate) struct SourceContextRequest {
    component_id: Option<String>,
    path_override: Option<String>,
    settings_overrides: Vec<(String, String)>,
    settings_json_overrides: Vec<(String, serde_json::Value)>,
    extension_overrides: Vec<String>,
}

impl SourceContextRequest {
    pub(crate) fn new(component_id: Option<String>, path_override: Option<String>) -> Self {
        Self {
            component_id,
            path_override,
            settings_overrides: Vec::new(),
            settings_json_overrides: Vec::new(),
            extension_overrides: Vec::new(),
        }
    }

    pub(crate) fn with_settings(mut self, settings: Vec<(String, String)>) -> Self {
        self.settings_overrides = settings;
        self
    }

    pub(crate) fn with_json_settings(
        mut self,
        settings_json: Vec<(String, serde_json::Value)>,
    ) -> Self {
        self.settings_json_overrides = settings_json;
        self
    }

    pub(crate) fn with_extension_overrides(mut self, extension_overrides: Vec<String>) -> Self {
        self.extension_overrides = extension_overrides;
        self
    }

    pub(crate) fn resolve(
        &self,
        capability: Option<ExtensionCapability>,
    ) -> homeboy::core::Result<ExecutionContext> {
        execution_context::resolve(&ResolveOptions {
            component_id: self.component_id.clone(),
            path_override: self.path_override.clone(),
            capability,
            settings_overrides: self.settings_overrides.clone(),
            settings_json_overrides: self.settings_json_overrides.clone(),
            extension_overrides: self.extension_overrides.clone(),
        })
    }
}

pub(crate) fn resolve_ci_job_for_command(
    job_id: Option<&str>,
    component: &Component,
    command: &'static str,
) -> homeboy::core::Result<Option<CiResolvedJob>> {
    let Some(job_id) = job_id else {
        return Ok(None);
    };
    let extension_ids = component_extension_ids(component);
    let extension_id = ci_profile::select_extension_id(&extension_ids)?;
    let job = ci_profile::resolve_job_for_extension(&extension_id, job_id)?;
    ci_profile::validate_job_command(&job, command)?;
    Ok(Some(job))
}

pub(crate) fn finish_observed_workflow<O, T, F, E>(
    observation: Option<O>,
    workflow: homeboy::core::Result<T>,
    finish_success: F,
    finish_error: E,
) -> homeboy::core::Result<T>
where
    F: FnOnce(O, &T),
    E: FnOnce(O, &homeboy::core::Error),
{
    match workflow {
        Ok(workflow) => {
            if let Some(observation) = observation {
                finish_success(observation, &workflow);
            }
            Ok(workflow)
        }
        Err(error) => {
            if let Some(observation) = observation {
                finish_error(observation, &error);
            }
            Err(error)
        }
    }
}

fn component_extension_ids(component: &Component) -> Vec<String> {
    let mut ids: Vec<String> = component
        .extensions
        .as_ref()
        .map(|extensions| extensions.keys().cloned().collect())
        .unwrap_or_default();
    ids.sort();
    ids
}

#[cfg(test)]
mod tests {
    use super::component_extension_ids;
    use homeboy::core::component::{Component, ScopedExtensionConfig};
    use std::collections::HashMap;

    #[test]
    fn component_extension_ids_are_sorted() {
        let mut extensions = HashMap::new();
        extensions.insert("wordpress".to_string(), ScopedExtensionConfig::default());
        extensions.insert("nodejs".to_string(), ScopedExtensionConfig::default());
        let mut component = Component::new(
            "demo".to_string(),
            "/tmp/demo".to_string(),
            String::new(),
            None,
        );
        component.extensions = Some(extensions);

        assert_eq!(
            component_extension_ids(&component),
            vec!["nodejs", "wordpress"]
        );
    }
}
