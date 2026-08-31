use homeboy_engine_primitives::grammar::{self, items::GrammarItem};

/// Parse source with an installed extension's declared grammar without exposing
/// its manifest or installation path to callers.
pub fn parse_items_with_extension_grammar(
    extension_id: &str,
    file_extension: &str,
    content: &str,
) -> Option<Vec<GrammarItem>> {
    let grammar = extension_grammar(extension_id, file_extension)?;
    let items = grammar::items::parse_items(content, &grammar);
    (!items.is_empty()).then_some(items)
}

/// Load an installed extension's declared grammar without exposing its manifest.
pub fn extension_grammar(
    extension_id: &str,
    file_extension: &str,
) -> Option<homeboy_engine_primitives::grammar::Grammar> {
    let extension = super::catalog::load_extension(extension_id).ok()?;
    let extension_path = extension.extension_path.as_deref()?;
    grammar::load_for_extension_path(std::path::Path::new(extension_path), file_extension)
}
