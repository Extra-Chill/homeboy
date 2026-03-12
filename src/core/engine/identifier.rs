//! Shared identifier normalization and validation primitives.

use crate::error::Error;
use crate::Result;

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
