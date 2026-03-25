//! extension — extracted from mod.rs.

use crate::component::Component;
use crate::config;
use crate::error::Result;
use crate::paths;
use crate::error::Error;
use crate::output::MergeOutput;
use std::collections::HashMap;
use std::path::PathBuf;
use serde::Serialize;
use crate::core::extension::extension_path;


pub fn load_extension(id: &str) -> Result<ExtensionManifest> {
    let mut manifest = config::load::<ExtensionManifest>(id)?;
    let extension_dir = paths::extension(id)?;
    manifest.extension_path = Some(extension_dir.to_string_lossy().to_string());
    Ok(manifest)
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
