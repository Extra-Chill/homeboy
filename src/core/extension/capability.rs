use crate::core::component::Component;
use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, ErrorCode, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::manifest::ExtensionManifest;
use super::registry::{extension_path, load_extension};
use super::runner::ExtensionRunner;

pub(crate) fn stderr_tail(stderr: &str) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = stderr.lines().collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionCapability {
    Lint,
    Test,
    Build,
    Bench,
    Trace,
    Deps,
}

/// Static metadata for an [`ExtensionCapability`] variant.
///
/// Centralizing label, manifest-support probe, and script accessor in one
/// descriptor keeps variant additions localized: a new capability only
/// needs one new arm in [`ExtensionCapability::descriptor`] instead of
/// parallel arms scattered across each getter / policy method.
struct ExtensionCapabilityDescriptor {
    label: &'static str,
    has_manifest_support: fn(&ExtensionManifest) -> bool,
    script_path: fn(&ExtensionManifest) -> Option<&str>,
}

impl ExtensionCapability {
    fn descriptor(self) -> ExtensionCapabilityDescriptor {
        match self {
            ExtensionCapability::Lint => ExtensionCapabilityDescriptor {
                label: "lint",
                has_manifest_support: ExtensionManifest::has_lint,
                script_path: ExtensionManifest::lint_script,
            },
            ExtensionCapability::Test => ExtensionCapabilityDescriptor {
                label: "test",
                has_manifest_support: ExtensionManifest::has_test,
                script_path: ExtensionManifest::test_script,
            },
            ExtensionCapability::Build => ExtensionCapabilityDescriptor {
                label: "build",
                has_manifest_support: ExtensionManifest::has_build,
                script_path: ExtensionManifest::build_script,
            },
            ExtensionCapability::Bench => ExtensionCapabilityDescriptor {
                label: "bench",
                has_manifest_support: ExtensionManifest::has_bench,
                script_path: ExtensionManifest::bench_script,
            },
            ExtensionCapability::Trace => ExtensionCapabilityDescriptor {
                label: "trace",
                has_manifest_support: ExtensionManifest::has_trace,
                script_path: ExtensionManifest::trace_script,
            },
            ExtensionCapability::Deps => ExtensionCapabilityDescriptor {
                label: "deps",
                has_manifest_support: ExtensionManifest::has_deps,
                script_path: ExtensionManifest::deps_script,
            },
        }
    }

    pub(crate) fn label(self) -> &'static str {
        self.descriptor().label
    }

    pub(crate) fn has_manifest_support(self, manifest: &ExtensionManifest) -> bool {
        (self.descriptor().has_manifest_support)(manifest)
    }

    pub(crate) fn script_path(self, manifest: &ExtensionManifest) -> Option<&str> {
        (self.descriptor().script_path)(manifest)
    }

    pub(crate) fn requires_script(self) -> bool {
        self != ExtensionCapability::Build
    }
}

#[derive(Debug, Clone)]
pub struct ExtensionExecutionContext {
    pub component: Component,
    pub capability: ExtensionCapability,
    pub extension_id: String,
    pub extension_path: PathBuf,
    pub script_path: String,
    pub settings: Vec<(String, serde_json::Value)>,
    /// Setting keys the resolved extension declares it understands (from
    /// the manifest `settings` block). Used to validate `--setting` /
    /// `--setting-json` overrides before a run. Empty means the extension
    /// declares no settings, in which case validation is skipped.
    pub accepted_setting_keys: Vec<String>,
}

pub struct ScenarioRunnerOptions<'a> {
    pub execution_context: &'a ExtensionExecutionContext,
    pub component: &'a Component,
    pub path_override: Option<String>,
    pub settings: &'a [(String, String)],
    pub settings_json: &'a [(String, serde_json::Value)],
    pub run_dir: &'a RunDir,
    pub results_env: Option<(&'a str, PathBuf)>,
    pub scenario_env: Option<(&'a str, &'a str)>,
    pub artifact_env: Option<(&'a str, &'a Path)>,
    pub list_only_env: Option<(&'a str, bool)>,
    pub extra_workloads_env: Option<(&'a str, &'a [PathBuf], &'a str)>,
    pub invocation_requirements: crate::core::engine::invocation::InvocationRequirements,
}

pub fn build_scenario_runner(options: ScenarioRunnerOptions<'_>) -> Result<ExtensionRunner> {
    let mut runner = ExtensionRunner::for_context(options.execution_context.clone())
        .component(options.component.clone())
        .path_override(options.path_override)
        .settings(options.settings)
        .settings_json(options.settings_json)
        .with_run_dir(options.run_dir)
        .invocation_requirements(options.invocation_requirements);

    if let Some((key, path)) = options.results_env {
        runner = runner.env(key, &path.to_string_lossy());
    }
    if let Some((key, value)) = options.scenario_env {
        runner = runner.env(key, value);
    }
    if let Some((key, path)) = options.artifact_env {
        runner = runner.env(key, &path.to_string_lossy());
    }
    if let Some((key, list_only)) = options.list_only_env {
        runner = runner.env(key, if list_only { "1" } else { "0" });
    }
    if let Some((key, paths, error_field)) = options.extra_workloads_env {
        if !paths.is_empty() {
            runner = runner.env(key, &path_list_env_value(error_field, paths)?);
        }
    }

    Ok(runner)
}

pub fn path_list_env_value(error_field: &str, paths: &[PathBuf]) -> Result<String> {
    let path_description = error_field
        .strip_suffix("_workloads")
        .map(|prefix| format!("{prefix} workload path"))
        .unwrap_or_else(|| "workload path".to_string());

    std::env::join_paths(paths)
        .map_err(|e| {
            Error::validation_invalid_argument(
                error_field,
                format!("{path_description} cannot be exported: {e}"),
                None,
                None,
            )
        })
        .map(|joined| joined.to_string_lossy().to_string())
}

fn no_extensions_error(component: &Component) -> Error {
    let mut err = Error::new(
        ErrorCode::ExtensionUnsupported,
        format!(
            "No extension provider configured for component '{}'",
            component.id
        ),
        serde_json::json!({
            "component_id": component.id,
            "problem": "no extensions configured",
        }),
    );

    for hint in extension_guidance_hints(component, None) {
        err = err.with_hint(hint);
    }

    err
}

fn capability_missing_error(component: &Component, capability: ExtensionCapability) -> Error {
    let capability_name = capability.label();
    let mut err = Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has no linked extensions that provide {} support",
            component.id, capability_name
        ),
        None,
        None,
    );

    for hint in extension_guidance_hints(component, Some(capability)) {
        err = err.with_hint(hint);
    }

    err
}

pub(crate) fn extension_guidance_hints(
    component: &Component,
    capability: Option<ExtensionCapability>,
) -> Vec<String> {
    let link_hint = match capability {
        Some(capability) => format!(
            "Link an extension with {} support: homeboy component set {} --extension <extension_id>",
            capability.label(),
            component.id
        ),
        None => format!(
            "Link an extension that provides the needed command support: homeboy component set {} --extension <extension_id>",
            component.id
        ),
    };

    vec![
        link_hint,
        "List installed extensions: homeboy extension list".to_string(),
        "The component path resolved correctly; the requested command needs an extension provider.".to_string(),
        "Use `scripts.build` for component-owned build commands; component-level `build_command` is unsupported.".to_string(),
    ]
}

fn capability_ambiguous_error(
    component: &Component,
    capability: ExtensionCapability,
    matching: &[String],
) -> Error {
    let capability_name = capability.label();
    Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has multiple linked extensions with {} support: {}",
            component.id,
            capability_name,
            matching.join(", ")
        ),
        None,
        None,
    )
    .with_hint(format!(
        "Configure explicit {} extension ownership before running this command",
        capability_name
    ))
}

fn linked_extensions(
    component: &Component,
) -> Result<&HashMap<String, crate::core::component::ScopedExtensionConfig>> {
    component
        .extensions
        .as_ref()
        .ok_or_else(|| no_extensions_error(component))
}

pub fn extract_component_extension_settings(
    component: &Component,
    extension_id: &str,
) -> Vec<(String, serde_json::Value)> {
    component
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(extension_id))
        .map(|extension_config| {
            extension_config
                .settings
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn resolve_extension_for_capability(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<String> {
    let extensions = linked_extensions(component)?;
    if extensions.is_empty() {
        return Err(no_extensions_error(component));
    }

    let mut matching = Vec::new();

    for extension_id in extensions.keys() {
        let manifest = load_extension(extension_id)?;
        if capability.has_manifest_support(&manifest) {
            matching.push(extension_id.clone());
        }
    }

    match matching.len() {
        0 => Err(capability_missing_error(component, capability)),
        1 => Ok(matching.remove(0)),
        _ => Err(capability_ambiguous_error(component, capability, &matching)),
    }
}

pub fn resolve_execution_context(
    component: &Component,
    capability: ExtensionCapability,
) -> Result<ExtensionExecutionContext> {
    let extension_id = resolve_extension_for_capability(component, capability)?;
    let manifest = load_extension(&extension_id)?;
    let script_path = capability
        .script_path(&manifest)
        .map(|s| s.to_string())
        // Build's extension_script is optional (builds can use local scripts or command templates),
        // so we allow an empty script_path for Build. Lint/Test/Bench require it.
        .or_else(|| {
            if capability.requires_script() {
                None
            } else {
                Some(String::new())
            }
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "extension",
                format!(
                    "Extension '{}' does not have {} infrastructure configured",
                    extension_id,
                    capability.label()
                ),
                None,
                None,
            )
        })?;

    let extension_path = extension_path(&extension_id);

    if !extension_path.exists() {
        return Err(Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' not found in ~/.config/homeboy/extensions/",
                extension_id
            ),
            None,
            None,
        ));
    }

    Ok(ExtensionExecutionContext {
        component: component.clone(),
        capability,
        extension_id: extension_id.clone(),
        extension_path,
        script_path,
        settings: extract_component_extension_settings(component, &extension_id),
        accepted_setting_keys: manifest.accepted_setting_keys(),
    })
}
