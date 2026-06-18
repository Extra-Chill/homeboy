//! Deprecation-age detection — flag `@deprecated X.Y.Z` docblocks that
//! are significantly older than the component's current version.
//!
//! Walks file fingerprints, scans `content` for docblock `@deprecated`
//! tags, compares the tagged version against the component's current
//! version (from a config-declared version source), and emits
//! `Info`-severity findings when the deprecation exceeds the age
//! threshold. Each finding is annotated with a count of remaining call
//! sites (scanned from `internal_calls` and `call_sites` across all
//! fingerprints) so reviewers can judge removal safety at a glance.
//!
//! The set of languages the detector applies to and the version sources used
//! to resolve the component's current version are declared via
//! [`DetectorProfileConfig`]; core keeps no hardcoded ecosystem manifest or
//! header conventions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use semver::Version;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use crate::core::component::{DetectorProfileConfig, VersionSource};

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

/// Match the nearest symbol declaration following a docblock across the
/// supported declaration keywords (class/trait/interface/function/method/
/// fn/struct/enum). Captures the symbol name.
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

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    root: &Path,
    config: &DetectorProfileConfig,
) -> Vec<Finding> {
    // Languages the detector applies to are declared via config; when a
    // component opts into builtin defaults but declares no explicit set, fall
    // back to the agnostic catalogue of classifiable languages.
    let language_tokens: Vec<String> = if !config.deprecation_languages.is_empty() {
        config.deprecation_languages.clone()
    } else if config.use_builtin_defaults {
        Language::builtin_extension_tokens()
            .iter()
            .map(|token| (*token).to_string())
            .collect()
    } else {
        Vec::new()
    };
    if language_tokens.is_empty() {
        return Vec::new();
    }

    // Version sources are component-declared; core ships none. Without a
    // declared source the detector cannot anchor "current version" and stays
    // inert.
    if config.version_sources.is_empty() {
        return Vec::new();
    }
    let Some(current) = detect_current_version(root, &config.version_sources) else {
        return Vec::new();
    };

    // Pre-compute cross-file call-site reference counts, keyed by symbol name.
    // Keys borrow from the fingerprints so we avoid cloning thousands of
    // short identifier strings in larger codebases.
    let reference_counts = build_reference_counts(fingerprints);

    let mut findings = Vec::new();
    for fp in fingerprints {
        if !fp.language.matches_any_token(&language_tokens) {
            continue;
        }
        collect_findings(fp, &current, &reference_counts, &mut findings);
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
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
/// Skips blank lines, comment lines, and bookkeeping lines
/// (`namespace`/`use`/`import` statements, attributes, decorators) that
/// commonly sit between a file-level docblock and the symbol it documents.
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
/// Tries the component-declared version sources in order and returns `None`
/// when none yield a parseable semver. Core owns only the generic resolution
/// mechanics (scan files of an extension for a regex match; read a JSON
/// manifest key); the concrete manifest/header conventions are declared by the
/// component/extension profile.
fn detect_current_version(root: &Path, sources: &[VersionSource]) -> Option<Version> {
    sources
        .iter()
        .find_map(|source| resolve_version_source(root, source))
}

fn resolve_version_source(root: &Path, source: &VersionSource) -> Option<Version> {
    match source {
        VersionSource::HeaderRegex {
            file_extension,
            pattern,
        } => header_regex_version(root, file_extension, pattern),
        VersionSource::JsonManifest { file, key } => json_manifest_version(root, file, key),
    }
}

/// Scan files of the given extension directly under `root` for the first match
/// of `pattern` (single capture group → semver).
fn header_regex_version(root: &Path, file_extension: &str, pattern: &str) -> Option<Version> {
    let regex = Regex::new(pattern).ok()?;
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(file_extension) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(v) = regex
            .captures(&content)
            .and_then(|c| c.get(1))
            .and_then(|m| Version::parse(m.as_str()).ok())
        {
            return Some(v);
        }
    }
    None
}

/// Read a top-level string `key` from a JSON manifest at `root/file`.
fn json_manifest_version(root: &Path, file: &str, key: &str) -> Option<Version> {
    let path = root.join(file);
    let content = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let raw = value.get(key)?.as_str()?;
    Version::parse(raw).ok()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::CallSite;

    fn make_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    /// Representative version sources mirroring what a PHP/WordPress extension
    /// profile would declare.
    fn test_version_sources() -> Vec<VersionSource> {
        vec![
            VersionSource::HeaderRegex {
                file_extension: "php".to_string(),
                pattern: r"(?mi)^\s*\*?\s*Version\s*:\s*(\d+\.\d+\.\d+)".to_string(),
            },
            VersionSource::JsonManifest {
                file: "composer.json".to_string(),
                key: "version".to_string(),
            },
        ]
    }

    /// A profile with builtin defaults plus representative version sources, for
    /// exercising the full `run` path.
    fn test_profile() -> DetectorProfileConfig {
        DetectorProfileConfig {
            version_sources: test_version_sources(),
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

        let v = detect_current_version(tmp.path(), &test_version_sources()).unwrap();
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

        let v = detect_current_version(tmp.path(), &test_version_sources()).unwrap();
        assert_eq!(v, Version::parse("2.3.4").unwrap());
    }

    #[test]
    fn no_version_source_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(detect_current_version(tmp.path(), &test_version_sources()).is_none());
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

namespace SamplePlugin\Core\WordPress;

use SamplePlugin\Core\Baz;

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
        // Profile declares no version sources → detector stays inert.
        let findings = run(&[&fp], tmp.path(), &DetectorProfileConfig::default());
        assert!(findings.is_empty());
    }

    #[test]
    fn run_flags_stale_deprecation_with_declared_version_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("plugin.php"),
            "<?php\n/**\n * Version: 0.78.0\n */\n",
        )
        .unwrap();
        let fp = make_fp(
            "inc/Legacy.php",
            "<?php\n/**\n * @deprecated 0.31.1 Use modern() instead.\n */\nfunction legacy() {}\n",
        );
        let findings = run(&[&fp], tmp.path(), &test_profile());
        assert_eq!(
            findings.len(),
            1,
            "stale deprecation should fire: {findings:?}"
        );
        assert!(findings[0].description.contains("legacy"));
    }

    #[test]
    fn run_is_inert_when_builtin_defaults_disabled_and_no_languages_declared() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("plugin.php"),
            "<?php\n/**\n * Version: 0.78.0\n */\n",
        )
        .unwrap();
        let fp = make_fp(
            "inc/Legacy.php",
            "<?php\n/**\n * @deprecated 0.31.1\n */\nfunction legacy() {}\n",
        );
        let config = DetectorProfileConfig {
            use_builtin_defaults: false,
            version_sources: test_version_sources(),
            ..Default::default()
        };
        let findings = run(&[&fp], tmp.path(), &config);
        assert!(
            findings.is_empty(),
            "no language tokens declared and builtin defaults off → inert"
        );
    }
}
