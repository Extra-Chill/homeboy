//! extensions_error — extracted from mod.rs.

use crate::component::Component;
use crate::error::Error;
use crate::error::Result;
use std::collections::HashMap;
use crate::output::MergeOutput;
use std::path::PathBuf;
use serde::Serialize;


pub(crate) fn no_extensions_error(component: &Component) -> Error {
    Error::validation_invalid_argument(
        "component",
        format!("Component '{}' has no extensions configured", component.id),
        None,
        None,
    )
    .with_hint(format!(
        "Add a extension: homeboy component set {} --extension <extension_id>",
        component.id
    ))
}

pub(crate) fn linked_extensions(
    component: &Component,
) -> Result<&HashMap<String, crate::component::ScopedExtensionConfig>> {
    component
        .extensions
        .as_ref()
        .ok_or_else(|| no_extensions_error(component))
}
