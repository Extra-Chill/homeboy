use crate::component::Component;
use crate::error::{Error, Result};
use crate::module::{self, ModuleManifest};
use crate::engine::pipeline::PipelineCapabilityResolver;

use super::types::ReleaseStepType;

pub(crate) struct ReleaseCapabilityResolver {
    modules: Vec<ModuleManifest>,
}

impl ReleaseCapabilityResolver {
    pub fn new(modules: Vec<ModuleManifest>) -> Self {
        Self { modules }
    }

    fn supports_package(&self) -> bool {
        self.modules
            .iter()
            .any(|module| module.actions.iter().any(|a| a.id == "release.package"))
    }

    fn supports_publish_target(&self, target: &str) -> bool {
        self.modules
            .iter()
            .any(|module| module.id == target && module.actions.iter().any(|a| a.id == "release.publish"))
    }
}

impl PipelineCapabilityResolver for ReleaseCapabilityResolver {
    fn is_supported(&self, step_type: &str) -> bool {
        let st = ReleaseStepType::from_str(step_type);
        match st {
            ReleaseStepType::Version
            | ReleaseStepType::GitCommit
            | ReleaseStepType::GitTag
            | ReleaseStepType::GitPush
            | ReleaseStepType::Cleanup => true,
            ReleaseStepType::Package => self.supports_package(),
            ReleaseStepType::Publish(ref target) => self.supports_publish_target(target),
        }
    }

    fn missing(&self, step_type: &str) -> Vec<String> {
        let st = ReleaseStepType::from_str(step_type);
        match st {
            ReleaseStepType::Package => {
                if !self.supports_package() {
                    vec!["Missing module with action 'release.package'".to_string()]
                } else {
                    Vec::new()
                }
            }
            ReleaseStepType::Publish(ref target) => {
                vec![format!(
                    "Missing module '{}' with action 'release.publish'",
                    target
                )]
            }
            _ => Vec::new(),
        }
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
