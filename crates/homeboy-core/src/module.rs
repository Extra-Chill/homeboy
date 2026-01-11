use crate::config::AppPaths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: String,
    pub description: String,
    pub author: String,
    pub homepage: Option<String>,
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub inputs: Vec<InputConfig>,
    pub output: OutputConfig,
    #[serde(default)]
    pub actions: Vec<ActionConfig>,
    #[serde(default)]
    pub settings: Vec<SettingConfig>,
    pub requires: Option<RequirementsConfig>,
    #[serde(skip)]
    pub module_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementsConfig {
    pub components: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    pub project_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfig {
    #[serde(rename = "type")]
    pub runtime_type: RuntimeType,
    pub entrypoint: Option<String>,
    pub dependencies: Option<Vec<String>>,
    pub playwright_browsers: Option<Vec<String>>,
    pub args: Option<String>,
    pub default_site: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeType {
    Python,
    Shell,
    Cli,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub input_type: String,
    pub label: String,
    pub placeholder: Option<String>,
    pub default: Option<serde_json::Value>,
    pub min: Option<i32>,
    pub max: Option<i32>,
    pub options: Option<Vec<SelectOption>>,
    pub arg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfig {
    pub schema: OutputSchema,
    pub display: String,
    pub selectable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    pub items: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionConfig {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub action_type: String,
    pub builtin: Option<String>,
    pub column: Option<String>,
    pub endpoint: Option<String>,
    pub method: Option<String>,
    pub requires_auth: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub setting_type: String,
    pub label: String,
    pub placeholder: Option<String>,
    pub default: Option<serde_json::Value>,
}

pub fn load_module(id: &str) -> Option<ModuleManifest> {
    let module_dir = AppPaths::module(id);
    let manifest_path = module_dir.join("module.json");

    if !manifest_path.exists() {
        return None;
    }

    let content = fs::read_to_string(&manifest_path).ok()?;
    let mut manifest: ModuleManifest = serde_json::from_str(&content).ok()?;
    manifest.module_path = Some(module_dir.to_string_lossy().to_string());
    Some(manifest)
}

pub fn load_all_modules() -> Vec<ModuleManifest> {
    let modules_dir = AppPaths::modules();
    if !modules_dir.exists() {
        return Vec::new();
    }

    let entries = match fs::read_dir(&modules_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut modules = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let manifest_path = path.join("module.json");
            if manifest_path.exists() {
                if let Ok(content) = fs::read_to_string(&manifest_path) {
                    if let Ok(mut manifest) = serde_json::from_str::<ModuleManifest>(&content) {
                        manifest.module_path = Some(path.to_string_lossy().to_string());
                        modules.push(manifest);
                    }
                }
            }
        }
    }

    modules.sort_by(|a, b| a.id.cmp(&b.id));
    modules
}

pub fn module_path(id: &str) -> PathBuf {
    AppPaths::module(id)
}
