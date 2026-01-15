use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::component::{self, Component};
use crate::module::ModuleManifest;
use crate::project::{self, Project};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct ReleaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ReleaseStep>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ReleaseStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]

pub struct EffectiveReleaseConfig {
    pub config: ReleaseConfig,
    pub sources: ReleaseSources,
}

#[derive(Debug, Clone, Default)]

pub struct ReleaseSources {
    pub module: bool,
    pub project: bool,
    pub component: bool,
}

pub fn resolve_effective_config(
    component: Option<&Component>,
    project: Option<&Project>,
    module: Option<&ModuleManifest>,
) -> Option<EffectiveReleaseConfig> {
    // Load project from component if not provided directly
    let loaded_project = if project.is_none() {
        resolve_project_from_component(component)
    } else {
        None
    };
    let project_settings = project.or(loaded_project.as_ref());

    let module_release = module.and_then(|m| m.release.as_ref());
    let project_release = project_settings.and_then(|p| p.release.as_ref());
    let component_release = component.and_then(|c| c.release.as_ref());

    let config = merge_release_configs(module_release, project_release, component_release)?;

    Some(EffectiveReleaseConfig {
        config,
        sources: ReleaseSources {
            module: module_release.is_some(),
            project: project_release.is_some(),
            component: component_release.is_some(),
        },
    })
}

pub fn resolve_component_release(
    component: &Component,
    module: Option<&ModuleManifest>,
) -> Option<EffectiveReleaseConfig> {
    resolve_effective_config(Some(component), None, module)
}

fn resolve_project_from_component(component: Option<&Component>) -> Option<Project> {
    let component = component?;
    let projects = component::projects_using(&component.id).ok()?;
    if projects.len() == 1 {
        project::load(&projects[0]).ok()
    } else {
        None
    }
}

fn merge_release_configs(
    module: Option<&ReleaseConfig>,
    project: Option<&ReleaseConfig>,
    component: Option<&ReleaseConfig>,
) -> Option<ReleaseConfig> {
    let merged = merge_release_config(module, project);
    merge_release_config(merged.as_ref(), component)
}

fn merge_release_config(
    base: Option<&ReleaseConfig>,
    overlay: Option<&ReleaseConfig>,
) -> Option<ReleaseConfig> {
    match (base, overlay) {
        (None, None) => None,
        (Some(config), None) => Some(config.clone()),
        (None, Some(config)) => Some(config.clone()),
        (Some(base), Some(overlay)) => {
            let enabled = overlay.enabled.or(base.enabled);
            let steps = if !overlay.steps.is_empty() {
                overlay.steps.clone()
            } else {
                base.steps.clone()
            };
            let mut settings = base.settings.clone();
            for (key, value) in &overlay.settings {
                settings.insert(key.clone(), value.clone());
            }

            Some(ReleaseConfig {
                enabled,
                steps,
                settings,
            })
        }
    }
}
