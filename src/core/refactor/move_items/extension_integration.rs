//! extension_integration — extracted from move_items.rs.

use std::path::Path;

use crate::core::scaffold::load_extension_grammar;
use crate::extension::{self, grammar_items, ExtensionManifest, ParsedItem};

/// Find a refactor-capable extension for a file based on its extension.
pub(crate) fn find_refactor_extension(file_path: &str) -> Option<ExtensionManifest> {
    let ext = Path::new(file_path).extension().and_then(|e| e.to_str())?;
    extension::find_extension_for_file_ext(ext, "refactor")
}

/// Try parsing items using the core grammar engine (no extension script needed).
pub(crate) fn core_parse_items(ext: &ExtensionManifest, content: &str) -> Option<Vec<ParsedItem>> {
    let ext_path = ext.extension_path.as_deref()?;
    let file_ext = ext.provided_file_extensions().first()?.clone();
    let grammar = load_extension_grammar(Path::new(ext_path), &file_ext)?;
    let items = grammar_items::parse_items(content, &grammar);
    if items.is_empty() {
        return None;
    }
    Some(items.into_iter().map(ParsedItem::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_refactor_extension_default_path() {

        let _result = find_refactor_extension();
    }

    #[test]
    fn test_core_parse_items_default_path() {

        let _result = core_parse_items();
    }

    #[test]
    fn test_core_parse_items_default_path_2() {

        let _result = core_parse_items();
    }

    #[test]
    fn test_core_parse_items_items_is_empty() {

        let result = core_parse_items();
        assert!(result.is_none(), "expected None for: items.is_empty()");
    }

    #[test]
    fn test_core_parse_items_items_is_empty_2() {

        let result = core_parse_items();
        assert!(result.is_some(), "expected Some for: items.is_empty()");
    }

}
