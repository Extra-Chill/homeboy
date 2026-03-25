//! capability — extracted from mod.rs.

use crate::component::Component;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use std::collections::HashMap;
use std::path::PathBuf;
use serde::Serialize;
use crate::core::extension::ExtensionCapability;


pub(crate) fn capability_label(capability: ExtensionCapability) -> &'static str {
    match capability {
        ExtensionCapability::Lint => "lint",
        ExtensionCapability::Test => "test",
        ExtensionCapability::Build => "build",
    }
}

pub(crate) fn capability_missing_error(component: &Component, capability: ExtensionCapability) -> Error {
    let capability_name = capability_label(capability);
    Error::validation_invalid_argument(
        "extension",
        format!(
            "Component '{}' has no linked extensions that provide {} support",
            component.id, capability_name
        ),
        None,
        None,
    )
    .with_hint(format!(
        "Link an extension with {} support: homeboy component set {} --extension <extension_id>",
        capability_name, component.id
    ))
}

pub(crate) fn capability_ambiguous_error(
    component: &Component,
    capability: ExtensionCapability,
    matching: &[String],
) -> Error {
    let capability_name = capability_label(capability);
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
