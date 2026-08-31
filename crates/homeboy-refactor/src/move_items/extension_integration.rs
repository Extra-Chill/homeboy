//! extension_integration — extracted from move_items.rs.

use std::path::Path;

use homeboy_core::{self, extension::ParsedItem};
use homeboy_engine_primitives::grammar::{self, items as grammar_items};
use homeboy_extension_contract::api::v1::REFACTOR_FILE_CAPABILITY_PREFIX;
use homeboy_extension_contract::ExtensionManifest;

/// Find a refactor-capable extension for a file based on its extension.
pub(crate) fn find_refactor_extension(file_path: &str) -> Option<ExtensionManifest> {
    let ext = Path::new(file_path).extension().and_then(|e| e.to_str())?;
    find_refactor_extension_for_extension(ext)
}

pub(crate) fn find_refactor_extension_for_extension(
    file_extension: &str,
) -> Option<ExtensionManifest> {
    let capability_id = format!("{REFACTOR_FILE_CAPABILITY_PREFIX}{file_extension}");
    let extension_id =
        homeboy_core::extension::resolve::find_installed_capability_provider(&capability_id)?;
    homeboy_core::extension::catalog::load_extension(&extension_id).ok()
}

/// Try parsing items using the core grammar engine (no extension script needed).
pub(crate) fn core_parse_items(ext: &ExtensionManifest, content: &str) -> Option<Vec<ParsedItem>> {
    let ext_path = ext.extension_path.as_deref()?;
    let file_ext = ext.provided_file_extensions().first()?.clone();
    let grammar = grammar::load_for_extension_path(Path::new(ext_path), &file_ext)?;
    let items = grammar_items::parse_items(content, &grammar);
    if items.is_empty() {
        return None;
    }
    Some(items.into_iter().map(ParsedItem::from).collect())
}
