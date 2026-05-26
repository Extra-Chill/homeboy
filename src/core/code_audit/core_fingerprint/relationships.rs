use std::collections::HashSet;

use crate::core::extension::grammar::Symbol;

/// Extract extends (parent class) from symbols.
pub(super) fn extract_extends(symbols: &[Symbol]) -> Option<String> {
    symbols
        .iter()
        .filter(|s| s.concept == "class" || s.concept == "struct")
        .find_map(|s| {
            s.get("extends").map(|e| {
                // PHP: take last segment of backslash-separated name
                e.split('\\').next_back().unwrap_or(e).to_string()
            })
        })
}

/// Extract implements (traits/interfaces) from symbols.
pub(super) fn extract_implements(symbols: &[Symbol]) -> Vec<String> {
    let mut implements = Vec::new();
    let mut seen = HashSet::new();

    // From impl_block symbols (Rust: impl Trait for Type)
    for s in symbols.iter().filter(|s| s.concept == "impl_block") {
        if let Some(trait_name) = s.get("trait_name") {
            push_unique_implement(
                &mut implements,
                &mut seen,
                trait_name,
                trait_name,
                "::",
                false,
            );
        }
    }

    // From implements pattern (PHP)
    for s in symbols.iter().filter(|s| s.concept == "implements") {
        if let Some(interfaces) = s.get("interfaces") {
            for iface in interfaces.split(',') {
                let iface = iface.trim();
                push_unique_implement(&mut implements, &mut seen, iface, iface, "\\", true);
            }
        }
    }

    // From trait_use pattern (PHP: use SomeTrait;)
    for s in symbols.iter().filter(|s| s.concept == "trait_use") {
        if let Some(name) = s.name() {
            push_unique_implement(&mut implements, &mut seen, name, name, "\\", true);
        }
    }

    implements
}

fn push_unique_implement(
    implements: &mut Vec<String>,
    seen: &mut HashSet<String>,
    seen_name: &str,
    display_name: &str,
    separator: &str,
    seen_by_short_name: bool,
) {
    if seen_name.is_empty() {
        return;
    }

    let short = display_name
        .rsplit(separator)
        .next()
        .unwrap_or(display_name);
    let seen_key = if seen_by_short_name { short } else { seen_name };
    if seen.insert(seen_key.to_string()) {
        implements.push(short.to_string());
    }
}
