use crate::component::Component;
use crate::error::{Error, Result};
use crate::module::{self, ModuleManifest};
use crate::pipeline::PipelineCapabilityResolver;

use super::types::ReleaseStepType;

pub(crate) struct ReleaseCapabilityResolver {
    modules: Vec<ModuleManifest>,
}

impl ReleaseCapabilityResolver {
    pub fn new(modules: Vec<ModuleManifest>) -> Self {
        Self { modules }
    }

    fn supports_module_action(&self, step_type: &str) -> bool {
        let action_id = format!("release.{}", step_type);
        self.modules
            .iter()
            .any(|module| module.actions.iter().any(|action| action.id == action_id))
    }
}

impl PipelineCapabilityResolver for ReleaseCapabilityResolver {
    fn is_supported(&self, step_type: &str) -> bool {
        let st = ReleaseStepType::from(step_type);
        st == ReleaseStepType::ModuleRun
            || st.is_core_step()
            || self.supports_module_action(step_type)
    }

    fn missing(&self, step_type: &str) -> Vec<String> {
        if ReleaseStepType::from(step_type) == ReleaseStepType::ModuleRun {
            return Vec::new();
        }
        let action_id = format!("release.{}", step_type);
        vec![format!("Missing action '{}'", action_id)]
    }
}

pub(crate) fn resolve_modules(
    component: &Component,
    module_id: Option<&str>,
) -> Result<Vec<ModuleManifest>> {
    if module_id.is_some() {
        return Err(Error::validation_invalid_argument(
            "module",
            "Module selection is configured via component.modules; --module is not supported",
            None,
            None,
        ));
    }

    let mut modules = Vec::new();
    if let Some(configured) = component.modules.as_ref() {
        let mut module_ids: Vec<String> = configured.keys().cloned().collect();
        module_ids.sort();
        let suggestions = module::available_module_ids();
        for module_id in module_ids {
            let manifest = module::load_module(&module_id).map_err(|_| {
                Error::module_not_found(module_id.to_string(), suggestions.clone())
            })?;
            modules.push(manifest);
        }
    }

    Ok(modules)
}

pub(crate) fn resolve_module_actions(
    modules: &[ModuleManifest],
    action_id: &str,
) -> Result<Vec<ModuleManifest>> {
    let matches: Vec<ModuleManifest> = modules
        .iter()
        .filter(|module| module.actions.iter().any(|action| action.id == action_id))
        .cloned()
        .collect();

    if matches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.steps",
            format!("No module provides action '{}'", action_id),
            None,
            None,
        ));
    }

    Ok(matches)
}
