//! Case utilities — capitalization, pluralization, word splitting, and
//! cross-separator join functions used to generate naming-convention variants.

// ============================================================================
// Case utilities
// ============================================================================

pub(super) fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

pub(super) fn pluralize(s: &str) -> String {
    if s.ends_with('s') || s.ends_with('x') || s.ends_with("sh") || s.ends_with("ch") {
        format!("{}es", s)
    } else if s.ends_with('y') && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("ay") {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{}s", s)
    }
}

// ============================================================================
// Word splitting — decompose any naming convention into constituent words
// ============================================================================

/// Split a term into its constituent words, regardless of naming convention.
///
/// Handles:
/// - `kebab-case` → `["kebab", "case"]`
/// - `snake_case` → `["snake", "case"]`
/// - `camelCase` → `["camel", "case"]`
/// - `PascalCase` → `["pascal", "case"]`
/// - `UPPER_SNAKE` → `["upper", "snake"]`
/// - `WPAgent` → `["wp", "agent"]` (consecutive uppercase → separate word)
/// - `XMLParser` → `["xml", "parser"]`
/// - `sample-plugin-agent` → `["sample", "plugin", "agent"]`
/// - Mixed: `my_WPAgent-thing` → `["my", "wp", "agent", "thing"]`
///
/// All returned words are lowercase.
pub(super) fn split_words(term: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = term.chars().collect();
    let len = chars.len();

    for i in 0..len {
        let c = chars[i];

        // Separators: hyphens, underscores, spaces, dots
        if c == '-' || c == '_' || c == ' ' || c == '.' {
            if !current.is_empty() {
                words.push(current.to_lowercase());
                current.clear();
            }
            continue;
        }

        if c.is_uppercase() && !current.is_empty() {
            let prev = chars[i - 1];
            // Split on camelCase boundary (lowercase/digit → uppercase)
            // or consecutive-uppercase boundary (uppercase → uppercase+lowercase)
            let is_camel_boundary = prev.is_lowercase() || prev.is_ascii_digit();
            let is_acronym_boundary =
                prev.is_uppercase() && i + 1 < len && chars[i + 1].is_lowercase();

            if is_camel_boundary || is_acronym_boundary {
                words.push(current.to_lowercase());
                current.clear();
            }
        }

        current.push(c);
    }

    if !current.is_empty() {
        words.push(current.to_lowercase());
    }

    words
}

// ============================================================================
// Cross-separator join functions
// ============================================================================

/// Join words as kebab-case: `["sample", "plugin", "agent"]` → `"sample-plugin-agent"`
pub(super) fn join_kebab(words: &[String]) -> String {
    words.join("-")
}

/// Join words as snake_case: `["sample", "plugin", "agent"]` → `"sample_plugin_agent"`
pub(super) fn join_snake(words: &[String]) -> String {
    words.join("_")
}

/// Join words as UPPER_SNAKE: `["sample", "plugin", "agent"]` → `"SAMPLE_PLUGIN_AGENT"`
pub(super) fn join_upper_snake(words: &[String]) -> String {
    words
        .iter()
        .map(|w| w.to_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// Join words as PascalCase: `["sample", "plugin", "agent"]` → `"SamplePluginAgent"`
pub(super) fn join_pascal(words: &[String]) -> String {
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join("")
}

/// Join words as camelCase: `["sample", "plugin", "agent"]` → `"samplePluginAgent"`
pub(super) fn join_camel(words: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            parts.push(w.to_lowercase());
        } else {
            parts.push(capitalize(w));
        }
    }
    parts.join("")
}

/// Join words as display name: `["sample", "plugin", "agent"]` → `"Sample Plugin Agent"`
pub(super) fn join_display(words: &[String]) -> String {
    words
        .iter()
        .map(|w| capitalize(w))
        .collect::<Vec<_>>()
        .join(" ")
}
