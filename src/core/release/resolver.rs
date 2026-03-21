use crate::component::Component;
use crate::engine::pipeline::PipelineCapabilityResolver;
use crate::error::{Error, Result};
use crate::extension::{self, ExtensionManifest};

use super::types::ReleaseStepType;

pub(crate) struct ReleaseCapabilityResolver {
    extensions: Vec<ExtensionManifest>,
}

impl ReleaseCapabilityResolver {
    pub fn new(extensions: Vec<ExtensionManifest>) -> Self {
        Self { extensions }
    }

    fn supports_package(&self) -> bool {
        self.extensions
            .iter()
            .any(|extension| extension.actions.iter().any(|a| a.id == "release.package"))
    }

    fn supports_publish_target(&self, target: &str) -> bool {
        self.extensions.iter().any(|extension| {
            extension.id == target && extension.actions.iter().any(|a| a.id == "release.publish")
        })
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
            | ReleaseStepType::Cleanup
            | ReleaseStepType::PostRelease => true,
            ReleaseStepType::Package => self.supports_package(),
            ReleaseStepType::Publish(ref target) => self.supports_publish_target(target),
        }
    }

    fn missing(&self, step_type: &str) -> Vec<String> {
        let st = ReleaseStepType::from_str(step_type);
        match st {
            ReleaseStepType::Package => {
                if !self.supports_package() {
                    vec!["Missing extension with action 'release.package'".to_string()]
                } else {
                    Vec::new()
                }
            }
            ReleaseStepType::Publish(ref target) => {
                vec![format!(
                    "Missing extension '{}' with action 'release.publish'",
                    target
                )]
            }
            _ => Vec::new(),
        }
    }
}

pub(crate) fn resolve_extensions(
    component: &Component,
    extension_id: Option<&str>,
) -> Result<Vec<ExtensionManifest>> {
    if extension_id.is_some() {
        return Err(Error::validation_invalid_argument(
            "extension",
            "Extension selection is configured via component.extensions; --extension is not supported",
            None,
            None,
        ));
    }

    let mut extensions = Vec::new();
    if let Some(configured) = component.extensions.as_ref() {
        let mut extension_ids: Vec<String> = configured.keys().cloned().collect();
        extension_ids.sort();
        let suggestions = extension::available_extension_ids();
        for extension_id in extension_ids {
            let manifest = extension::load_extension(&extension_id).map_err(|_| {
                Error::extension_not_found(extension_id.to_string(), suggestions.clone())
            })?;
            extensions.push(manifest);
        }
    }

    Ok(extensions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_default_path() {
        let instance = ReleaseCapabilityResolver::default();
        let extensions = Vec::new();
        let _result = instance.new(extensions);
    }
}
