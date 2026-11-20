//! Shared identifier normalization and validation primitives.

use homeboy_error::{Error, Result};

/// Identifier families have existing, intentionally different separator rules.
#[derive(Clone, Copy)]
pub enum SlugStyle {
    /// Drop punctuation; collapse whitespace, underscores and dashes.
    Identifier,
    /// Lowercase, preserve underscores, replace each punctuation character.
    CookBatch,
    /// Lowercase and collapse all non-alphanumeric runs to a dash.
    Branch,
    /// Preserve case and underscores for existing materialized cache paths.
    Path,
}

/// Normalize a slug using the caller's established identifier contract.
pub fn slugify(value: &str, style: SlugStyle) -> String {
    let (separator, collapse) = match style {
        SlugStyle::Identifier | SlugStyle::Branch => ('-', true),
        SlugStyle::CookBatch => ('-', false),
        SlugStyle::Path => ('_', false),
    };
    let mut out = String::new();
    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(if matches!(style, SlugStyle::Path) {
                ch
            } else {
                ch.to_ascii_lowercase()
            })
        } else {
            match style {
                SlugStyle::Identifier if ch.is_whitespace() || ch == '_' || ch == '-' => Some('-'),
                SlugStyle::Identifier => None,
                SlugStyle::CookBatch | SlugStyle::Path if ch == '_' || ch == '-' => Some(ch),
                _ => Some(separator),
            }
        };
        if let Some(ch) = normalized {
            if collapse && ch == separator && (out.is_empty() || out.ends_with(separator)) {
                continue;
            }
            out.push(ch);
        }
    }
    out.trim_matches(separator).to_string()
}

#[cfg(test)]
mod slug_tests {
    use super::*;

    #[test]
    fn slug_styles_preserve_existing_contracts() {
        for (style, input, expected) in [
            (
                SlugStyle::Identifier,
                " Hello_ World! / Again ",
                "hello-world-again",
            ),
            (SlugStyle::Identifier, "a/b", "ab"),
            (SlugStyle::CookBatch, "_Hello//World_", "_hello--world_"),
            (SlugStyle::Branch, "_Hello//World_", "hello-world"),
            (SlugStyle::Path, " /Hello//World/ ", "Hello__World"),
            (SlugStyle::Branch, "🦀中文", ""),
        ] {
            assert_eq!(slugify(input, style), expected);
        }
        assert!(slugify_id("!!!", "name").is_err());
        assert!(slugify_id(" ", "name").is_err());
    }
}

pub fn slugify_id(value: &str, field_name: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            field_name,
            format!("{} cannot be empty", capitalize(field_name)),
            None,
            None,
        ));
    }

    let out = slugify(trimmed, SlugStyle::Identifier);

    if out.is_empty() {
        return Err(Error::validation_invalid_argument(
            field_name,
            format!(
                "{} must contain at least one letter or number",
                capitalize(field_name)
            ),
            None,
            None,
        ));
    }

    Ok(out)
}

/// Repository selectors use the same filesystem-safe slug as component IDs.
/// Accept a name, checkout path, HTTPS remote, or SCP-style Git remote.
/// Dots are deliberately removed, preserving existing component registrations.
pub fn repository_id(value: &str) -> Result<String> {
    let name = value
        .trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or_default();
    let name = name.strip_suffix(".git").unwrap_or(name);
    slugify_id(name, "component_id")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

pub fn validate_component_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(Error::validation_invalid_argument(
            "component_id",
            "Component ID cannot be empty",
            None,
            None,
        ));
    }

    if id.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        return Err(Error::validation_invalid_argument(
            "component_id",
            "Component ID contains invalid characters",
            Some(id.to_string()),
            None,
        ));
    }

    Ok(())
}
