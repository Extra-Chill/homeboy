//! String slug generation utilities.

use crate::error::Error;
use crate::Result;

/// Convert a string to a URL-friendly slug.
///
/// - Converts to lowercase
/// - Replaces whitespace, underscores, and dashes with single dashes
/// - Removes special characters
/// - Trims leading/trailing dashes
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

    let mut out = String::new();
    let mut prev_was_dash = false;

    for ch in trimmed.chars() {
        let normalized = match ch {
            'a'..='z' | '0'..='9' => Some(ch),
            'A'..='Z' => Some(ch.to_ascii_lowercase()),
            _ if ch.is_whitespace() || ch == '_' || ch == '-' => Some('-'),
            _ => None,
        };

        if let Some(c) = normalized {
            if c == '-' {
                if out.is_empty() || prev_was_dash {
                    continue;
                }
                out.push('-');
                prev_was_dash = true;
            } else {
                out.push(c);
                prev_was_dash = false;
            }
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

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

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Validate a component ID for use in configuration.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic_name() {
        assert_eq!(slugify_id("My Component", "name").unwrap(), "my-component");
    }

    #[test]
    fn slugify_preserves_numbers() {
        assert_eq!(slugify_id("Plugin v2", "name").unwrap(), "plugin-v2");
    }

    #[test]
    fn slugify_trims_whitespace() {
        assert_eq!(slugify_id("  spaced  ", "name").unwrap(), "spaced");
    }

    #[test]
    fn slugify_collapses_dashes() {
        assert_eq!(slugify_id("foo--bar__baz", "name").unwrap(), "foo-bar-baz");
    }

    #[test]
    fn slugify_strips_special_chars() {
        assert_eq!(slugify_id("Hello! @World#", "name").unwrap(), "hello-world");
    }

    #[test]
    fn slugify_empty_fails() {
        assert!(slugify_id("", "name").is_err());
    }

    #[test]
    fn slugify_only_special_fails() {
        assert!(slugify_id("!@#$%", "name").is_err());
    }

    #[test]
    fn slugify_whitespace_only_fails() {
        assert!(slugify_id("   ", "name").is_err());
    }

    #[test]
    fn validate_component_id_empty_fails() {
        assert!(validate_component_id("").is_err());
    }

    #[test]
    fn validate_component_id_with_slash_fails() {
        assert!(validate_component_id("foo/bar").is_err());
    }

    #[test]
    fn validate_component_id_valid() {
        assert!(validate_component_id("extrachill-api").is_ok());
        assert!(validate_component_id("my_plugin").is_ok());
        assert!(validate_component_id("Plugin123").is_ok());
    }
}
