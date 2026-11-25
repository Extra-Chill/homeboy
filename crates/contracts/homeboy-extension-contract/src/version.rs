//! Version constraint parsing and matching for extension/extension versioning.
//!
//! Supports semver version constraints used in component configs to declare
//! required extension versions:
//!
//! - Exact: `1.2.3` — must match exactly
//! - Caret: `^1.2.3` — compatible updates (>=1.2.3, <2.0.0)
//! - Tilde: `~1.2.3` — patch-level updates (>=1.2.3, <1.3.0)
//! - Greater/equal: `>=1.2.3`
//! - Greater: `>1.2.3`
//! - Less/equal: `<=1.2.3`
//! - Less: `<1.2.3`
//! - Wildcard: `*` — any version

use homeboy_error::{Error, Result};
use semver::Version;

/// A parsed version constraint that can check whether a given version satisfies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionConstraint {
    /// Any version matches.
    Any,
    /// Must match exactly.
    Exact(Version),
    /// `^major.minor.patch` — compatible updates.
    /// ^1.2.3 means >=1.2.3, <2.0.0
    /// ^0.2.3 means >=0.2.3, <0.3.0
    /// ^0.0.3 means >=0.0.3, <0.0.4
    Caret(Version),
    /// `~major.minor.patch` — patch-level updates.
    /// ~1.2.3 means >=1.2.3, <1.3.0
    Tilde(Version),
    /// `>=version`
    GreaterEqual(Version),
    /// `>version`
    Greater(Version),
    /// `<=version`
    LessEqual(Version),
    /// `<version`
    Less(Version),
    /// Every comma-separated constraint must match.
    All(Vec<VersionConstraint>),
}

impl VersionConstraint {
    /// Parse a version constraint string.
    ///
    /// Examples: `">=1.0.0"`, `"^2.0"`, `"~1.5"`, `"1.2.3"`, `"*"`
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();

        if input == "*" {
            return Ok(VersionConstraint::Any);
        }

        if input.contains(',') {
            return input
                .split(',')
                .map(str::trim)
                .map(VersionConstraint::parse)
                .collect::<Result<Vec<_>>>()
                .map(VersionConstraint::All);
        }

        if let Some(rest) = input.strip_prefix(">=") {
            let v = parse_version(rest.trim(), input)?;
            return Ok(VersionConstraint::GreaterEqual(v));
        }

        if let Some(rest) = input.strip_prefix('>') {
            let v = parse_version(rest.trim(), input)?;
            return Ok(VersionConstraint::Greater(v));
        }

        if let Some(rest) = input.strip_prefix("<=") {
            let v = parse_version(rest.trim(), input)?;
            return Ok(VersionConstraint::LessEqual(v));
        }

        if let Some(rest) = input.strip_prefix('<') {
            let v = parse_version(rest.trim(), input)?;
            return Ok(VersionConstraint::Less(v));
        }

        if let Some(rest) = input.strip_prefix('^') {
            let v = parse_version(rest.trim(), input)?;
            return Ok(VersionConstraint::Caret(v));
        }

        if let Some(rest) = input.strip_prefix('~') {
            let v = parse_version(rest.trim(), input)?;
            return Ok(VersionConstraint::Tilde(v));
        }

        // No prefix — exact match
        let v = parse_version(input, input)?;
        Ok(VersionConstraint::Exact(v))
    }

    /// Check if a version satisfies this constraint.
    pub fn matches(&self, version: &Version) -> bool {
        match self {
            VersionConstraint::Any => true,
            VersionConstraint::Exact(v) => version == v,
            VersionConstraint::GreaterEqual(v) => version >= v,
            VersionConstraint::Greater(v) => version > v,
            VersionConstraint::LessEqual(v) => version <= v,
            VersionConstraint::Less(v) => version < v,
            VersionConstraint::Caret(v) => caret_matches(v, version),
            VersionConstraint::Tilde(v) => tilde_matches(v, version),
            VersionConstraint::All(constraints) => constraints
                .iter()
                .all(|constraint| constraint.matches(version)),
        }
    }
}

impl std::fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionConstraint::Any => write!(f, "*"),
            VersionConstraint::Exact(v) => write!(f, "{}", v),
            VersionConstraint::GreaterEqual(v) => write!(f, ">={}", v),
            VersionConstraint::Greater(v) => write!(f, ">{}", v),
            VersionConstraint::LessEqual(v) => write!(f, "<={}", v),
            VersionConstraint::Less(v) => write!(f, "<{}", v),
            VersionConstraint::Caret(v) => write!(f, "^{}", v),
            VersionConstraint::Tilde(v) => write!(f, "~{}", v),
            VersionConstraint::All(constraints) => {
                let mut constraints = constraints.iter();
                if let Some(first) = constraints.next() {
                    write!(f, "{first}")?;
                }
                for constraint in constraints {
                    write!(f, ", {constraint}")?;
                }
                Ok(())
            }
        }
    }
}

/// Parse a potentially partial version string (e.g. "1", "1.2", "1.2.3").
fn parse_version(s: &str, original_input: &str) -> Result<Version> {
    let s = s.trim();

    // Try parsing as-is first
    if let Ok(v) = Version::parse(s) {
        return Ok(v);
    }

    // Try padding partial versions: "1" -> "1.0.0", "1.2" -> "1.2.0"
    let parts: Vec<&str> = s.split('.').collect();
    let padded = match parts.len() {
        1 => format!("{}.0.0", s),
        2 => format!("{}.0", s),
        _ => s.to_string(),
    };

    Version::parse(&padded).map_err(|e| {
        Error::validation_invalid_argument(
            "version_constraint",
            format!("Invalid version in constraint '{}': {}", original_input, e),
            Some(original_input.to_string()),
            None,
        )
    })
}

/// Caret matching: ^X.Y.Z
/// - ^1.2.3 := >=1.2.3, <2.0.0
/// - ^0.2.3 := >=0.2.3, <0.3.0
/// - ^0.0.3 := >=0.0.3, <0.0.4
fn caret_matches(constraint: &Version, version: &Version) -> bool {
    if version < constraint {
        return false;
    }

    if constraint.major != 0 {
        // ^1.2.3: same major, anything else goes
        version.major == constraint.major
    } else if constraint.minor != 0 {
        // ^0.2.3: same major + minor
        version.major == constraint.major && version.minor == constraint.minor
    } else {
        // ^0.0.3: exact match on all three
        version.major == constraint.major
            && version.minor == constraint.minor
            && version.patch == constraint.patch
    }
}

/// Tilde matching: ~X.Y.Z
/// - ~1.2.3 := >=1.2.3, <1.3.0
fn tilde_matches(constraint: &Version, version: &Version) -> bool {
    if version < constraint {
        return false;
    }
    version.major == constraint.major && version.minor == constraint.minor
}

/// Parse a extension's version string, returning a useful error if invalid.
pub fn parse_extension_version(version_str: &str, extension_id: &str) -> Result<Version> {
    Version::parse(version_str).map_err(|e| {
        Error::validation_invalid_argument(
            "version",
            format!(
                "Extension '{}' has invalid semver version '{}': {}",
                extension_id, version_str, e
            ),
            Some(version_str.to_string()),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parse_recognizes_operators_padding_and_whitespace() {
        for (input, expected) in [
            ("*", VersionConstraint::Any),
            ("1.2.3", VersionConstraint::Exact(v("1.2.3"))),
            ("^1.2.3", VersionConstraint::Caret(v("1.2.3"))),
            ("^2.0", VersionConstraint::Caret(v("2.0.0"))),
            ("^2", VersionConstraint::Caret(v("2.0.0"))),
            ("~1.5", VersionConstraint::Tilde(v("1.5.0"))),
            (">=1.0.0", VersionConstraint::GreaterEqual(v("1.0.0"))),
            (">= 1.0.0 ", VersionConstraint::GreaterEqual(v("1.0.0"))),
            (">2.0.0", VersionConstraint::Greater(v("2.0.0"))),
            ("<=3.0.0", VersionConstraint::LessEqual(v("3.0.0"))),
            ("<1.0.0", VersionConstraint::Less(v("1.0.0"))),
        ] {
            assert_eq!(
                VersionConstraint::parse(input).unwrap(),
                expected,
                "{input}"
            );
        }
        assert!(VersionConstraint::parse(">=abc").is_err());
    }

    #[test]
    fn constraints_match_their_documented_ranges() {
        for (constraint, version, expected) in [
            ("1.2.3", "1.2.3", true),
            ("1.2.3", "1.2.4", false),
            ("*", "0.0.0", true),
            ("^1.2.3", "1.99.99", true),
            ("^1.2.3", "1.2.2", false),
            ("^1.2.3", "2.0.0", false),
            ("^0.2.3", "0.2.99", true),
            ("^0.2.3", "0.3.0", false),
            ("^0.0.3", "0.0.3", true),
            ("^0.0.3", "0.0.4", false),
            ("~1.2.3", "1.2.99", true),
            ("~1.2.3", "1.2.2", false),
            ("~1.2.3", "1.3.0", false),
            (">=1.0.0", "1.0.0", true),
            (">1.0.0", "1.0.0", false),
            ("<=1.0.0", "1.0.0", true),
            ("<1.0.0", "1.0.0", false),
            (">=2.0.0, <3.0.0", "2.1.0", true),
            (">=2.0.0, <3.0.0", "3.0.0", false),
        ] {
            assert_eq!(
                VersionConstraint::parse(constraint)
                    .unwrap()
                    .matches(&v(version)),
                expected,
                "{constraint} vs {version}"
            );
        }
    }

    #[test]
    fn display_round_trips_through_parse() {
        for case in [
            "*",
            "1.2.3",
            ">=1.0.0",
            ">1.0.0",
            "<=1.0.0",
            "<1.0.0",
            "^1.2.3",
            "~1.2.3",
            ">=2.0.0, <3.0.0",
        ] {
            let constraint = VersionConstraint::parse(case).unwrap();
            assert_eq!(constraint.to_string(), case);
            assert_eq!(
                VersionConstraint::parse(&constraint.to_string()).unwrap(),
                constraint
            );
        }
    }

    #[test]
    fn invalid_extension_version_error_names_extension_and_value() {
        let err = parse_extension_version("not-a-version", "test-extension").unwrap_err();
        assert!(err.message.contains("test-extension"));
        assert!(err.message.contains("not-a-version"));
    }
}
