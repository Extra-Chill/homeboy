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

use crate::error::{Error, Result};
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

    // ========================================================================
    // Parsing
    // ========================================================================

    #[test]
    fn parse_exact() {
        let c = VersionConstraint::parse("1.2.3").unwrap();
        assert_eq!(c, VersionConstraint::Exact(v("1.2.3")));
    }

    #[test]
    fn parse_wildcard() {
        let c = VersionConstraint::parse("*").unwrap();
        assert_eq!(c, VersionConstraint::Any);
    }

    #[test]
    fn parse_caret() {
        let c = VersionConstraint::parse("^1.2.3").unwrap();
        assert_eq!(c, VersionConstraint::Caret(v("1.2.3")));
    }

    #[test]
    fn parse_caret_partial() {
        let c = VersionConstraint::parse("^2.0").unwrap();
        assert_eq!(c, VersionConstraint::Caret(v("2.0.0")));
    }

    #[test]
    fn parse_tilde() {
        let c = VersionConstraint::parse("~1.5").unwrap();
        assert_eq!(c, VersionConstraint::Tilde(v("1.5.0")));
    }

    #[test]
    fn parse_gte() {
        let c = VersionConstraint::parse(">=1.0.0").unwrap();
        assert_eq!(c, VersionConstraint::GreaterEqual(v("1.0.0")));
    }

    #[test]
    fn parse_gt() {
        let c = VersionConstraint::parse(">2.0.0").unwrap();
        assert_eq!(c, VersionConstraint::Greater(v("2.0.0")));
    }

    #[test]
    fn parse_lte() {
        let c = VersionConstraint::parse("<=3.0.0").unwrap();
        assert_eq!(c, VersionConstraint::LessEqual(v("3.0.0")));
    }

    #[test]
    fn parse_lt() {
        let c = VersionConstraint::parse("<1.0.0").unwrap();
        assert_eq!(c, VersionConstraint::Less(v("1.0.0")));
    }

    #[test]
    fn parse_with_spaces() {
        let c = VersionConstraint::parse(">= 1.0.0 ").unwrap();
        assert_eq!(c, VersionConstraint::GreaterEqual(v("1.0.0")));
    }

    #[test]
    fn parse_single_number() {
        let c = VersionConstraint::parse("^2").unwrap();
        assert_eq!(c, VersionConstraint::Caret(v("2.0.0")));
    }

    #[test]
    fn parse_invalid_version() {
        assert!(VersionConstraint::parse(">=abc").is_err());
    }

    // ========================================================================
    // Matching — Exact
    // ========================================================================

    #[test]
    fn exact_matches_same() {
        let c = VersionConstraint::Exact(v("1.2.3"));
        assert!(c.matches(&v("1.2.3")));
    }

    #[test]
    fn exact_rejects_different() {
        let c = VersionConstraint::Exact(v("1.2.3"));
        assert!(!c.matches(&v("1.2.4")));
        assert!(!c.matches(&v("1.3.0")));
        assert!(!c.matches(&v("2.0.0")));
    }

    // ========================================================================
    // Matching — Wildcard
    // ========================================================================

    #[test]
    fn any_matches_everything() {
        let c = VersionConstraint::Any;
        assert!(c.matches(&v("0.0.0")));
        assert!(c.matches(&v("999.999.999")));
    }

    // ========================================================================
    // Matching — Caret
    // ========================================================================

    #[test]
    fn caret_major_nonzero() {
        let c = VersionConstraint::Caret(v("1.2.3"));
        assert!(c.matches(&v("1.2.3")));
        assert!(c.matches(&v("1.2.4")));
        assert!(c.matches(&v("1.3.0")));
        assert!(c.matches(&v("1.99.99")));
        assert!(!c.matches(&v("2.0.0")));
        assert!(!c.matches(&v("1.2.2")));
        assert!(!c.matches(&v("0.9.0")));
    }

    #[test]
    fn caret_minor_nonzero() {
        let c = VersionConstraint::Caret(v("0.2.3"));
        assert!(c.matches(&v("0.2.3")));
        assert!(c.matches(&v("0.2.4")));
        assert!(c.matches(&v("0.2.99")));
        assert!(!c.matches(&v("0.3.0")));
        assert!(!c.matches(&v("0.2.2")));
        assert!(!c.matches(&v("1.0.0")));
    }

    #[test]
    fn caret_all_zero() {
        let c = VersionConstraint::Caret(v("0.0.3"));
        assert!(c.matches(&v("0.0.3")));
        assert!(!c.matches(&v("0.0.4")));
        assert!(!c.matches(&v("0.0.2")));
        assert!(!c.matches(&v("0.1.0")));
    }

    // ========================================================================
    // Matching — Tilde
    // ========================================================================

    #[test]
    fn tilde_allows_patch() {
        let c = VersionConstraint::Tilde(v("1.2.3"));
        assert!(c.matches(&v("1.2.3")));
        assert!(c.matches(&v("1.2.4")));
        assert!(c.matches(&v("1.2.99")));
        assert!(!c.matches(&v("1.3.0")));
        assert!(!c.matches(&v("1.2.2")));
        assert!(!c.matches(&v("2.0.0")));
    }

    // ========================================================================
    // Matching — Comparison operators
    // ========================================================================

    #[test]
    fn gte_matches() {
        let c = VersionConstraint::GreaterEqual(v("1.0.0"));
        assert!(c.matches(&v("1.0.0")));
        assert!(c.matches(&v("1.0.1")));
        assert!(c.matches(&v("2.0.0")));
        assert!(!c.matches(&v("0.9.9")));
    }

    #[test]
    fn gt_matches() {
        let c = VersionConstraint::Greater(v("1.0.0"));
        assert!(!c.matches(&v("1.0.0")));
        assert!(c.matches(&v("1.0.1")));
    }

    #[test]
    fn lte_matches() {
        let c = VersionConstraint::LessEqual(v("1.0.0"));
        assert!(c.matches(&v("1.0.0")));
        assert!(c.matches(&v("0.9.9")));
        assert!(!c.matches(&v("1.0.1")));
    }

    #[test]
    fn lt_matches() {
        let c = VersionConstraint::Less(v("1.0.0"));
        assert!(!c.matches(&v("1.0.0")));
        assert!(c.matches(&v("0.9.9")));
    }

    // ========================================================================
    // Display
    // ========================================================================

    #[test]
    fn display_roundtrip() {
        let cases = vec![
            "*", "1.2.3", ">=1.0.0", ">1.0.0", "<=1.0.0", "<1.0.0", "^1.2.3", "~1.2.3",
        ];
        for case in cases {
            let c = VersionConstraint::parse(case).unwrap();
            let displayed = c.to_string();
            let reparsed = VersionConstraint::parse(&displayed).unwrap();
            assert_eq!(c, reparsed, "roundtrip failed for '{}'", case);
        }
    }

    // ========================================================================
    // Extension version parsing
    // ========================================================================

    #[test]
    fn parse_extension_version_valid() {
        let v = parse_extension_version("1.2.3", "test-extension").unwrap();
        assert_eq!(v, Version::new(1, 2, 3));
    }

    #[test]
    fn parse_extension_version_invalid() {
        let err = parse_extension_version("not-a-version", "test-extension").unwrap_err();
        assert!(err.message.contains("test-extension"));
        assert!(err.message.contains("not-a-version"));
    }
}
