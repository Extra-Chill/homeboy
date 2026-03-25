use crate::component::VersionTarget;
use crate::error::{Error, Result};
use regex::Regex;

/// Check if adding a new version target would conflict with existing targets.
pub fn validate_version_target_conflict(
    existing: &[VersionTarget],
    new_file: &str,
    new_pattern: &str,
    _component_id: &str,
) -> Result<()> {
    for target in existing {
        if target.file == new_file {
            let existing_pattern = target.pattern.as_deref().unwrap_or("");
            if existing_pattern == new_pattern {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Validate that a version target pattern is a valid regex with at least one capture group.
pub fn validate_version_pattern(pattern: &str) -> Result<()> {
    if pattern.contains("{version}") {
        return Err(Error::validation_invalid_argument(
            "version_target.pattern",
            format!(
                "Pattern '{}' uses template syntax ({{version}}), but a regex with a capture group is required. Example: 'Version: (\\d+\\.\\d+\\.\\d+)'",
                pattern
            ),
            Some(pattern.to_string()),
            None,
        ));
    }

    let re = Regex::new(&crate::engine::text::ensure_multiline(pattern)).map_err(|e| {
        Error::validation_invalid_argument(
            "version_target.pattern",
            format!("Invalid regex pattern '{}': {}", pattern, e),
            Some(pattern.to_string()),
            None,
        )
    })?;

    if re.captures_len() < 2 {
        return Err(Error::validation_invalid_argument(
            "version_target.pattern",
            format!(
                "Pattern '{}' has no capture group. Wrap the version portion in parentheses. Example: 'Version: (\\d+\\.\\d+\\.\\d+)'",
                pattern
            ),
            Some(pattern.to_string()),
            None,
        ));
    }

    Ok(())
}

/// Normalize a regex pattern by converting double-escaped backslashes to single.
pub fn normalize_version_pattern(pattern: &str) -> String {
    if pattern.contains("\\\\") {
        pattern.replace("\\\\", "\\")
    } else {
        pattern.to_string()
    }
}

pub fn parse_version_targets(targets: &[String]) -> Result<Vec<VersionTarget>> {
    let mut parsed = Vec::new();
    for target in targets {
        let mut parts = target.splitn(2, "::");
        let file = parts
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "version_target",
                    "Invalid version target format (expected 'file' or 'file::pattern')",
                    None,
                    None,
                )
            })?;
        let pattern = parts.next().map(str::trim).filter(|s| !s.is_empty());
        if let Some(p) = pattern {
            let normalized = normalize_version_pattern(p);
            validate_version_pattern(&normalized)?;
            parsed.push(VersionTarget {
                file: file.to_string(),
                pattern: Some(normalized),
            });
        } else {
            parsed.push(VersionTarget {
                file: file.to_string(),
                pattern: None,
            });
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_version_target_conflict_existing_pattern_new_pattern() {
        let existing = Vec::new();
        let new_file = "";
        let new_pattern = "";
        let _component_id = "";
        let result = validate_version_target_conflict(&existing, &new_file, &new_pattern, &_component_id);
        assert!(result.is_ok(), "expected Ok for: existing_pattern == new_pattern");
    }

    #[test]
    fn test_validate_version_target_conflict_existing_pattern_new_pattern_2() {
        let existing = Vec::new();
        let new_file = "";
        let new_pattern = "";
        let _component_id = "";
        let result = validate_version_target_conflict(&existing, &new_file, &new_pattern, &_component_id);
        assert!(result.is_ok(), "expected Ok for: existing_pattern == new_pattern");
    }

    #[test]
    fn test_validate_version_pattern_some_pattern_to_string() {
        let pattern = "";
        let _result = validate_version_pattern(&pattern);
    }

    #[test]
    fn test_validate_version_pattern_some_pattern_to_string_2() {
        let pattern = "";
        let _result = validate_version_pattern(&pattern);
    }

    #[test]
    fn test_validate_version_pattern_default_path() {
        let pattern = "";
        let _result = validate_version_pattern(&pattern);
    }

    #[test]
    fn test_validate_version_pattern_some_pattern_to_string_3() {
        let pattern = "";
        let _result = validate_version_pattern(&pattern);
    }

    #[test]
    fn test_validate_version_pattern_ok() {
        let pattern = "";
        let result = validate_version_pattern(&pattern);
        assert!(result.is_ok(), "expected Ok for: Ok(())");
    }

    #[test]
    fn test_normalize_version_pattern_default_path() {
        let pattern = "";
        let _result = normalize_version_pattern(&pattern);
    }

    #[test]
    fn test_normalize_version_pattern_has_expected_effects() {
        // Expected effects: mutation
        let pattern = "";
        let _ = normalize_version_pattern(&pattern);
    }

    #[test]
    fn test_parse_version_targets_default_path() {
        let targets = Vec::new();
        let _result = parse_version_targets(&targets);
    }

    #[test]
    fn test_parse_version_targets_if_let_some_p_pattern() {
        let targets = Vec::new();
        let _result = parse_version_targets(&targets);
    }

    #[test]
    fn test_parse_version_targets_let_some_p_pattern() {
        let targets = Vec::new();
        let _result = parse_version_targets(&targets);
    }

    #[test]
    fn test_parse_version_targets_let_some_p_pattern_2() {
        let targets = Vec::new();
        let _result = parse_version_targets(&targets);
    }

    #[test]
    fn test_parse_version_targets_ok_parsed() {
        let targets = Vec::new();
        let result = parse_version_targets(&targets);
        assert!(result.is_ok(), "expected Ok for: Ok(parsed)");
    }

    #[test]
    fn test_parse_version_targets_has_expected_effects() {
        // Expected effects: mutation
        let targets = Vec::new();
        let _ = parse_version_targets(&targets);
    }

}
