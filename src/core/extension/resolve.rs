//! resolve — extracted from mod.rs.

use crate::component::Component;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use std::collections::HashMap;
use std::path::PathBuf;
use serde::Serialize;
use crate::core::extension::ExtensionExecutionContext;
use crate::core::extension::ExtensionCapability;


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
        if manifest_has_capability(&manifest, capability) {
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
    let script_path = match capability {
        ExtensionCapability::Lint => manifest.lint_script(),
        ExtensionCapability::Test => manifest.test_script(),
        ExtensionCapability::Build => manifest.build_script(),
    }
    .map(|s| s.to_string())
    // Build's extension_script is optional (builds can use local scripts or command templates),
    // so we allow an empty script_path for Build. Lint/Test require it.
    .or_else(|| {
        if capability == ExtensionCapability::Build {
            Some(String::new())
        } else {
            None
        }
    })
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' does not have {} infrastructure configured",
                extension_id,
                capability_label(capability)
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
    })
}
