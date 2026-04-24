//! Deprecation-age detection — flag `@deprecated X.Y.Z` docblocks that
//! are significantly older than the component's current version.
//!
//! Walks file fingerprints, scans `content` for docblock `@deprecated`
//! tags, compares the tagged version against the component's current
//! version (from a plugin header or `composer.json`), and emits
//! `Info`-severity findings when the deprecation exceeds the age
//! threshold. Each finding is annotated with a count of remaining call
//! sites (scanned from `internal_calls` and `call_sites` across all
//! fingerprints) so reviewers can judge removal safety at a glance.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use semver::Version;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

/// Default age threshold: flag when the current minor is more than
/// this many minors ahead of the deprecated version on the same major,
/// or when the current major is strictly greater than the deprecated
/// major.
const MINOR_THRESHOLD: u64 = 2;

/// Match an `@deprecated` docblock tag and capture the first
/// semver-shaped token that follows (optionally after the word `since`).
///
/// Tolerates:
/// - `@deprecated 0.31.1`
/// - `@deprecated since 0.31.1`
/// - `@deprecated 0.31.1 Use X instead.`
/// - `* @deprecated   0.31.1   trailing prose`
static DEPRECATED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)@deprecated(?:\s+since)?\s+(\d+\.\d+\.\d+)").expect("valid regex")
});

/// Match the nearest symbol declaration following a docblock: PHP
/// class/trait/interface/function/method, Rust fn/struct/enum/trait,
/// JS/TS function/class. Captures the symbol name.
static SYMBOL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        ^\s*
        (?:
            (?:public|protected|private|static|final|abstract|pub|async|export|default)\s+
        )*
        (?:
            (?:function|fn|class|trait|interface|struct|enum)\s+
            (?P<name>[A-Za-z_][A-Za-z0-9_]*)
        )
        ",
    )
    .expect("valid regex")
});

pub(super) fn run(fingerprints: &[&FileFingerprint], root: &Path) -> Vec<Finding> {
    let Some(current) = detect_current_version(root) else {
        return Vec::new();
    };

    // Pre-compute cross-file call-site reference counts, keyed by symbol name.
    // Keys borrow from the fingerprints so we avoid cloning thousands of
    // short identifier strings in larger codebases.
    let reference_counts = build_reference_counts(fingerprints);

    let mut findings = Vec::new();
    for fp in fingerprints {
        if !is_supported_language(&fp.language) {
            continue;
        }
        collect_findings(fp, &current, &reference_counts, &mut findings);
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn is_supported_language(lang: &Language) -> bool {
    matches!(
        lang,
        Language::Php | Language::Rust | Language::JavaScript | Language::TypeScript
    )
}

fn collect_findings(
    fp: &FileFingerprint,
    current: &Version,
    reference_counts: &HashMap<&str, usize>,
    findings: &mut Vec<Finding>,
) {
    for cap in DEPRECATED_RE.captures_iter(&fp.content) {
        let Some(version_match) = cap.get(1) else {
            continue;
        };
        let Ok(deprecated) = Version::parse(version_match.as_str()) else {
            continue;
        };

        if !exceeds_threshold(current, &deprecated) {
            continue;
        }

        let tag_offset = cap.get(0).map(|m| m.start()).unwrap_or(0);
        let line_number = line_number_at(&fp.content, tag_offset);
        let symbol = find_following_symbol(&fp.content, tag_offset);

        let call_site_count = symbol
            .as_deref()
            .and_then(|name| reference_counts.get(name).copied())
            .unwrap_or(0);

        let symbol_label = symbol
            .as_deref()
            .map(|s| format!("`{}`", s))
            .unwrap_or_else(|| "symbol".to_string());

        let description = format!(
            "Deprecation on line {} ({}) tagged @deprecated {} is older than current {} ({} remaining call site(s))",
            line_number, symbol_label, deprecated, current, call_site_count
        );

        let suggestion = if call_site_count == 0 {
            "No remaining call sites — safe to remove the deprecated symbol.".to_string()
        } else {
            format!(
                "Review the {} remaining call site(s) and migrate them before removing the deprecated symbol.",
                call_site_count
            )
        };

        findings.push(Finding {
            convention: "deprecation_age".to_string(),
            severity: Severity::Info,
            file: fp.relative_path.clone(),
            description,
            suggestion,
            kind: AuditFinding::DeprecationAge,
        });
    }
}

/// Return true when the deprecated version is older than the current
/// version by more than the configured threshold.
fn exceeds_threshold(current: &Version, deprecated: &Version) -> bool {
    if current.major > deprecated.major {
        return true;
    }
    if current.major == deprecated.major
        && current.minor.saturating_sub(deprecated.minor) > MINOR_THRESHOLD
    {
        return true;
    }
    false
}

/// Determine the 1-indexed line number at a byte offset in `content`.
fn line_number_at(content: &str, byte_offset: usize) -> usize {
    content[..byte_offset.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

/// Walk forward from the docblock tag to find the first symbol
/// declaration that follows.
///
/// Skips blank lines, comment lines, and bookkeeping lines (PHP
/// `namespace`/`use`, Rust `use`/attributes, JS/TS `import`/decorators)
/// that commonly sit between a file-level docblock and the class it
/// documents.
fn find_following_symbol(content: &str, tag_offset: usize) -> Option<String> {
    let tail = content.get(tag_offset..)?;
    for line in tail.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('*')
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*/")
        {
            continue;
        }
        // Skip bookkeeping lines that can appear between a docblock and
        // the symbol it documents (file-level docblocks, attributes).
        if trimmed.starts_with("namespace ")
            || trimmed.starts_with("use ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with('@')
            || trimmed.starts_with("#[")
            || trimmed.starts_with("#![")
        {
            continue;
        }
        if let Some(caps) = SYMBOL_RE.captures(line) {
            if let Some(name) = caps.name("name") {
                return Some(name.as_str().to_string());
            }
        }
        // First meaningful line that isn't a recognizable declaration —
        // stop searching so we don't cross into unrelated code.
        break;
    }
    None
}

/// Build a map of symbol-name → count of references across all
/// fingerprints, using both `internal_calls` (function call names) and
/// `call_sites[].target` (call-site call targets).
///
/// Keys borrow from the fingerprints to avoid cloning identifier
/// strings for every call site in the codebase.
fn build_reference_counts<'a>(fingerprints: &'a [&FileFingerprint]) -> HashMap<&'a str, usize> {
    let mut counts: HashMap<&'a str, usize> = HashMap::new();
    for fp in fingerprints {
        for name in &fp.internal_calls {
            *counts.entry(name.as_str()).or_insert(0) += 1;
        }
        for site in &fp.call_sites {
            *counts.entry(site.target.as_str()).or_insert(0) += 1;
        }
    }
    counts
}

/// Read the current version of the component under `root`.
///
/// Tries in order:
/// 1. Plugin header `Version:` in any `*.php` file directly under `root`.
/// 2. `composer.json` top-level `version` field.
///
/// Returns `None` when neither source yields a parseable semver.
fn detect_current_version(root: &Path) -> Option<Version> {
    if let Some(v) = plugin_header_version(root) {
        return Some(v);
    }
    composer_json_version(root)
}

fn plugin_header_version(root: &Path) -> Option<Version> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("php") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(v) = parse_plugin_header_version(&content) {
            return Some(v);
        }
    }
    None
}

fn parse_plugin_header_version(content: &str) -> Option<Version> {
    static HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?mi)^\s*\*?\s*Version\s*:\s*(\d+\.\d+\.\d+)").expect("valid regex")
    });
    HEADER_RE
        .captures(content)
        .and_then(|c| c.get(1))
        .and_then(|m| Version::parse(m.as_str()).ok())
}

fn composer_json_version(root: &Path) -> Option<Version> {
    let path = root.join("composer.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let raw = value.get("version")?.as_str()?;
    Version::parse(raw).ok()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::CallSite;

    fn make_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn ancient_deprecation_is_flagged() {
        let content = r#"<?php
class OAuthProvider {
    /**
     * Refresh token helper.
     *
     * @deprecated 0.31.1 Use get_valid_access_token() instead.
     */
    public function refresh_token() {}
}
"#;
        let fp = make_fp("inc/OAuth.php", content);
        let current = Version::parse("0.78.0").unwrap();
        let refs = std::collections::HashMap::new();

        let mut findings = Vec::new();
        collect_findings(&fp, &current, &refs, &mut findings);

        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.kind, AuditFinding::DeprecationAge);
        assert_eq!(f.severity, Severity::Info);
        assert!(
            f.description.contains("refresh_token"),
            "expected symbol name in description, got: {}",
            f.description
        );
        assert!(f.description.contains("0.31.1"));
        assert!(f.description.contains("0.78.0"));
    }

    #[test]
    fn recent_deprecation_is_ignored() {
        let content = r#"<?php
/**
 * @deprecated 0.77.0 Use new_api() instead.
 */
function old_api() {}
"#;
        let fp = make_fp("inc/Api.php", content);
        let current = Version::parse("0.78.0").unwrap();
        let refs = std::collections::HashMap::new();

        let mut findings = Vec::new();
        collect_findings(&fp, &current, &refs, &mut findings);

        assert!(
            findings.is_empty(),
            "deprecation within threshold should not fire"
        );
    }

    #[test]
    fn deprecated_without_version_is_ignored() {
        let content = r#"<?php
/**
 * @deprecated Use get_all_tools() instead.
 */
function get_tools() {}
"#;
        let fp = make_fp("inc/Tools.php", content);
        let current = Version::parse("0.78.0").unwrap();
        let refs = std::collections::HashMap::new();

        let mut findings = Vec::new();
        collect_findings(&fp, &current, &refs, &mut findings);

        assert!(
            findings.is_empty(),
            "malformed @deprecated without version must be ignored"
        );
    }

    #[test]
    fn threshold_is_exclusive_at_two_minors() {
        // current 0.78.0 vs deprecated 0.75.0 → delta 3 > 2 → fires
        let current = Version::parse("0.78.0").unwrap();
        assert!(exceeds_threshold(
            &current,
            &Version::parse("0.75.0").unwrap()
        ));
        // delta 2 → does NOT fire (strictly greater than threshold)
        assert!(!exceeds_threshold(
            &current,
            &Version::parse("0.76.0").unwrap()
        ));
        // same minor
        assert!(!exceeds_threshold(
            &current,
            &Version::parse("0.78.0").unwrap()
        ));
    }

    #[test]
    fn major_bump_always_fires() {
        let current = Version::parse("1.0.0").unwrap();
        assert!(exceeds_threshold(
            &current,
            &Version::parse("0.99.0").unwrap()
        ));
    }

    #[test]
    fn call_site_count_reflects_remaining_references() {
        let content = r#"<?php
class Legacy {
    /**
     * @deprecated 0.31.1
     */
    public function old_method() {}
}
"#;
        let fp = make_fp("inc/Legacy.php", content);
        let current = Version::parse("0.78.0").unwrap();

        let mut refs: HashMap<&str, usize> = HashMap::new();
        refs.insert("old_method", 3);

        let mut findings = Vec::new();
        collect_findings(&fp, &current, &refs, &mut findings);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("3 remaining call site"));
        assert!(findings[0].suggestion.contains("3 remaining call site"));
    }

    #[test]
    fn deprecated_since_variant_parses() {
        let content = r#"<?php
/**
 * @deprecated since 0.31.1 Use modern_api() instead.
 */
function legacy_api() {}
"#;
        let fp = make_fp("inc/Api.php", content);
        let current = Version::parse("0.78.0").unwrap();
        let refs = std::collections::HashMap::new();

        let mut findings = Vec::new();
        collect_findings(&fp, &current, &refs, &mut findings);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("legacy_api"));
    }

    #[test]
    fn plugin_header_version_is_parsed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("plugin.php"),
            "<?php\n/**\n * Plugin Name: Test\n * Version:           0.78.0\n */\n",
        )
        .unwrap();

        let v = detect_current_version(tmp.path()).unwrap();
        assert_eq!(v, Version::parse("0.78.0").unwrap());
    }

    #[test]
    fn composer_json_version_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("composer.json"),
            r#"{"name":"x/y","version":"2.3.4"}"#,
        )
        .unwrap();

        let v = detect_current_version(tmp.path()).unwrap();
        assert_eq!(v, Version::parse("2.3.4").unwrap());
    }

    #[test]
    fn no_version_source_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(detect_current_version(tmp.path()).is_none());
    }

    #[test]
    fn build_reference_counts_merges_internal_calls_and_call_sites() {
        let fp1 = FileFingerprint {
            relative_path: "a.php".to_string(),
            language: Language::Php,
            internal_calls: vec!["old_method".to_string(), "helper".to_string()],
            ..Default::default()
        };
        let fp2 = FileFingerprint {
            relative_path: "b.php".to_string(),
            language: Language::Php,
            call_sites: vec![CallSite {
                target: "old_method".to_string(),
                line: 10,
                arg_count: 0,
            }],
            ..Default::default()
        };
        let fps = [&fp1, &fp2];
        let refs = build_reference_counts(&fps);
        assert_eq!(refs.get("old_method"), Some(&2));
        assert_eq!(refs.get("helper"), Some(&1));
    }

    #[test]
    fn find_following_symbol_skips_blank_comment_lines() {
        let content = "/**\n * @deprecated 0.31.1\n *\n */\npublic function foo() {}\n";
        let offset = content.find("@deprecated").unwrap();
        let symbol = find_following_symbol(content, offset);
        assert_eq!(symbol.as_deref(), Some("foo"));
    }

    #[test]
    fn find_following_symbol_skips_namespace_and_use_lines() {
        // File-level docblock above a class — common in PHP plugins where
        // the docblock sits above `namespace` and `use` declarations.
        let content = r#"<?php
/**
 * @deprecated 0.48.0 Context has moved.
 */

namespace DataMachine\Core\WordPress;

use DataMachine\Core\Baz;

class SiteContext {}
"#;
        let offset = content.find("@deprecated").unwrap();
        let symbol = find_following_symbol(content, offset);
        assert_eq!(symbol.as_deref(), Some("SiteContext"));
    }

    #[test]
    fn run_returns_empty_when_no_version_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fp = make_fp("x.php", "/** @deprecated 0.31.1 */\nfunction a() {}");
        let findings = run(&[&fp], tmp.path());
        assert!(findings.is_empty());
    }
}
