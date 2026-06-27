use homeboy::core::ci_profile::{self, CiResolvedJob};
use homeboy::core::component::Component;
use homeboy::core::engine::execution_context::{self, ExecutionContext, ResolveOptions};
use homeboy::core::extension::ExtensionCapability;

use crate::commands::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};

pub(crate) fn resolve_source_context(
    comp: &PositionalComponentArgs,
    settings: &SettingArgs,
    extension_override: &ExtensionOverrideArgs,
    capability: Option<ExtensionCapability>,
) -> homeboy::core::Result<ExecutionContext> {
    execution_context::resolve(&ResolveOptions {
        component_id: comp.component.clone(),
        path_override: comp.path.clone(),
        capability,
        settings_overrides: settings.setting.clone(),
        settings_json_overrides: settings.setting_json.clone(),
        extension_overrides: extension_override.extensions.clone(),
    })
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
        extensions.insert("fixture-b".to_string(), ScopedExtensionConfig::default());
        extensions.insert("fixture-a".to_string(), ScopedExtensionConfig::default());
        let mut component = Component::new(
            "demo".to_string(),
            "/tmp/demo".to_string(),
            String::new(),
            None,
        );
        component.extensions = Some(extensions);

        assert_eq!(
            component_extension_ids(&component),
            vec!["fixture-a", "fixture-b"]
        );
    }
}
