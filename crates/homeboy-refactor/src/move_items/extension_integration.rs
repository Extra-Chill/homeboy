//! extension_integration — extracted from move_items.rs.

use std::path::Path;

use crate::refactor_provider::{find_refactor_provider, RefactorProvider};

/// Find a refactor-capable extension for a file based on its extension.
pub(crate) fn find_refactor_extension(root: &Path, file_path: &str) -> Option<RefactorProvider> {
    let ext = Path::new(file_path).extension().and_then(|e| e.to_str())?;
    find_refactor_extension_for_extension(root, ext)
}

pub(crate) fn find_refactor_extension_for_extension(
    root: &Path,
    file_extension: &str,
) -> Option<RefactorProvider> {
    find_refactor_provider(root, file_extension)
}

pub(crate) fn core_parse_items(
    provider: &RefactorProvider,
    file_extension: &str,
    content: &str,
) -> Option<Vec<crate::refactor_provider::ParsedItem>> {
    if !provider.handles_file_extension(file_extension) {
        return None;
    }
    homeboy_core::extension::grammar::parse_items_with_extension_grammar(
        &provider.extension_id,
        file_extension,
        content,
    )
}
