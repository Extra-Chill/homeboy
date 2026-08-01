use crate::component::VersionTarget;
use crate::error::{Error, Result};
use regex::Regex;
use std::collections::BTreeMap;

/// Parse repeatable `--capability-extension <surface>=<extension>` pairs into
/// the `capability_extensions` map.
///
/// `capability_extensions` is the only way to resolve a contested ownership
/// surface, but before #11120 it had no CLI flag at all — the sole route was
/// `--json '{"capability_extensions":{...}}'`, which the ambiguity error did
/// not mention either. Keyed by a `BTreeMap` so repeated flags serialize in a
/// stable order.
///
/// The surface is any string key: the seven `ExtensionCapability` labels
/// (`build`, `lint`, `test`, `bench`, `fuzz`, `trace`, `deps`) plus the
/// non-enum surfaces (`remote_path`, `since_tag`, `provides.file_extensions`).
/// Core deliberately does not validate the surface name here — the manifest
/// declares far more contested surfaces than the enum knows about, and an
/// unknown key is inert rather than harmful.
pub fn parse_capability_extensions(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();

    for pair in pairs {
        let (surface, extension_id) = pair.split_once('=').ok_or_else(|| {
            Error::validation_invalid_argument(
                "capability_extension",
                format!(
                    "Invalid capability extension '{}' (expected '<capability>=<extension>')",
                    pair
                ),
                Some(pair.clone()),
                Some(vec![
                    "Example: --capability-extension build=wordpress".to_string()
                ]),
            )
        })?;

        let surface = surface.trim();
        let extension_id = extension_id.trim();

        if surface.is_empty() || extension_id.is_empty() {
            return Err(Error::validation_invalid_argument(
                "capability_extension",
                format!(
                    "Invalid capability extension '{}': both capability and extension are required",
                    pair
                ),
                Some(pair.clone()),
                Some(vec![
                    "Example: --capability-extension build=wordpress".to_string()
                ]),
            ));
        }

        if let Some(existing) = parsed.get(surface) {
            if existing != extension_id {
                return Err(Error::validation_invalid_argument(
                    "capability_extension",
                    format!(
                        "Conflicting owners for capability '{}': '{}' and '{}'",
                        surface, existing, extension_id
                    ),
                    Some(pair.clone()),
                    Some(vec![format!(
                        "Pass --capability-extension {}=<extension> once",
                        surface
                    )]),
                ));
            }
            continue;
        }

        parsed.insert(surface.to_string(), extension_id.to_string());
    }

    Ok(parsed)
}

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
                artifact_path: None,
            });
        } else {
            parsed.push(VersionTarget {
                file: file.to_string(),
                pattern: None,
                artifact_path: None,
            });
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod capability_extension_tests {
    use super::parse_capability_extensions;

    #[test]
    fn parses_capability_and_non_capability_surfaces() {
        let parsed = parse_capability_extensions(&[
            "build=wordpress".to_string(),
            "remote_path=wordpress".to_string(),
            "provides.file_extensions=nodejs".to_string(),
        ])
        .expect("valid pairs");

        assert_eq!(parsed.get("build").map(String::as_str), Some("wordpress"));
        assert_eq!(
            parsed.get("remote_path").map(String::as_str),
            Some("wordpress")
        );
        assert_eq!(
            parsed.get("provides.file_extensions").map(String::as_str),
            Some("nodejs")
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let parsed =
            parse_capability_extensions(&[" build = wordpress ".to_string()]).expect("valid pair");
        assert_eq!(parsed.get("build").map(String::as_str), Some("wordpress"));
    }

    #[test]
    fn rejects_pair_without_equals() {
        let err = parse_capability_extensions(&["build".to_string()])
            .expect_err("missing '=' must be rejected");
        assert!(err.to_string().contains("<capability>=<extension>"));
    }

    #[test]
    fn rejects_empty_halves() {
        assert!(parse_capability_extensions(&["=wordpress".to_string()]).is_err());
        assert!(parse_capability_extensions(&["build=".to_string()]).is_err());
    }

    #[test]
    fn repeating_the_same_owner_is_accepted() {
        let parsed = parse_capability_extensions(&[
            "build=wordpress".to_string(),
            "build=wordpress".to_string(),
        ])
        .expect("identical repeats agree");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn rejects_conflicting_owners_for_one_capability() {
        let err = parse_capability_extensions(&[
            "build=wordpress".to_string(),
            "build=nodejs".to_string(),
        ])
        .expect_err("conflicting owners must be rejected");
        assert!(err.to_string().contains("Conflicting owners"));
    }
}
