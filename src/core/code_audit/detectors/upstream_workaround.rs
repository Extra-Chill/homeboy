//! Upstream-bug workaround detection — flag code that exists because of a
//! tracked upstream bug. Two tiers:
//!
//! - **A (Warning):** marker keyword (`workaround`, `polyfill`, `shim`,
//!   `// Hack`, `until merged`, `legacy fallback`, …) AND a concrete tracker
//!   reference (an issue/PR/ticket URL, or `@see <url>`) co-located in the
//!   same contiguous comment block. Bare `#NNN` does not qualify on its own.
//!   The concrete tracker URL shapes are configured ecosystem defaults, not
//!   hardcoded here.
//! - **B (Info):** `version_compare(<KNOWN_CONSTANT>, '<X>', '<' | '<=')`
//!   guards against a known version constant for an opted-in ecosystem.
//!
//! Per the fix-upstream-first rule (RULES.md): every workaround should be
//! tracked debt with a known upstream cause. Today nothing flags them and
//! they accumulate forever even after the upstream fix lands.
//!
//! Distinct from `LegacyComment`: `LegacyComment` flags any stale phrasing
//! regardless of whether a tracker exists. `UpstreamWorkaround` requires
//! BOTH a marker AND a concrete reference, so findings are actionable —
//! check the linked issue, see if the upstream fix has shipped, then
//! remove the local workaround.
//!
//! Tier C (`function_exists` polyfill body detection) is intentionally
//! deferred from v1; adjacent to `dead_guard.rs`.

use regex::Regex;

use super::comment_blocks;
use super::conventions::{builtin_tracker_reference_regexes, AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use crate::core::component::DetectorProfileConfig;

// ============================================================================
// Tier A — marker + tracker reference catalogues
// ============================================================================

/// Substring markers (lowercased) that indicate a workaround. Includes both
/// keyword-style (`workaround`, `polyfill`, `shim`) and phrase-style
/// (`until merged upstream`, `for version of`, `legacy v1`) entries.
const MARKER_LITERALS: &[&str] = &[
    // Keyword markers
    "workaround",
    "work around",
    "work-around",
    "polyfill",
    "shim",
    "transitional shim",
    "kludge",
    "monkeypatch",
    "monkey patch",
    "backport",
    "backported",
    // Phrase markers
    "until merged",
    "until merged upstream",
    "until landed",
    "until shipped",
    "until fixed",
    "until released",
    "until patched",
    "until core",
    "for version of",
    "prior to",
    "legacy fallback",
    "legacy v1",
    "legacy path",
];

/// Leading-line markers — only matched at the start of a comment block,
/// after the comment chars are stripped. Avoids false positives like
/// "Hackathon" mid-paragraph.
const LEADING_MARKERS: &[&str] = &["hack ", "hack:", "hack to", "hack for"];

const DEFAULT_MARKER_REGEXES: &[&str] = &[
    r"until\s+\S+\s+(?:merged|landed|shipped|fixed|released|patched|merges|lands|ships|fixes|releases|patches|in core)\b",
];

// Tracker-reference regex shapes. Bare `#NNN` is intentionally NOT included —
// Tier A requires a marker AND a concrete URL/ticket, never a bare reference.
// The concrete ecosystem URL literals live in the agnostic conventions home
// (`Language`-adjacent `builtin_tracker_reference_regexes`), not inline here,
// so this detector stays free of hardcoded ecosystem literals.

// ============================================================================
// Tier B — version-compare guard catalogue
// ============================================================================

/// Recognized version-constant names. Easy to grow as new ecosystems land.
const VERSION_CONSTANTS: &[&str] = &[
    "PHP_VERSION",
    "$wp_version",
    "JETPACK__VERSION",
    "WC_VERSION",
    "GUTENBERG_VERSION",
    "AKISMET_VERSION",
];

const DEFAULT_VERSION_GUARD_REGEXES: &[&str] = &[
    r#"version_compare\s*\(\s*([A-Z_][A-Z0-9_]*|\$wp_version|PHP_VERSION)\s*,\s*['"]([^'"]+)['"]\s*,\s*['"]<=?['"]\s*\)"#,
];

// Version-guard language defaults live in the agnostic conventions home
// (`builtin_version_guard_tokens`), not inline here — version-compare syntax is
// ecosystem-specific, so the concrete token set stays out of this detector.
const DEFAULT_VENDORED_PATH_MARKERS: &[&str] =
    &["/vendor/", "vendor/", "/node_modules/", "node_modules/"];

// ============================================================================
// Public entry point
// ============================================================================

/// Run both upstream-workaround tiers across the fingerprint set. Vendored
/// paths (`/vendor/`, `/node_modules/`) are skipped — `LegacyComment` and
/// `TodoMarker` still scan vendor files; only this rule is conservative.
pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    config: &DetectorProfileConfig,
) -> Vec<Finding> {
    let profile = EffectiveDetectorProfile::from_config(config);
    let mut findings = Vec::new();
    for fp in fingerprints {
        if profile.is_vendored_path(&fp.relative_path) {
            continue;
        }
        findings.extend(scan_blocks(fp, &profile));
        findings.extend(scan_version_guards(fp, &profile));
    }
    findings
}

// ============================================================================
// Tier A — marker + reference pass
// ============================================================================

fn scan_blocks(fp: &FileFingerprint, profile: &EffectiveDetectorProfile) -> Vec<Finding> {
    let mut findings = Vec::new();
    for block in comment_blocks::extract(fp) {
        let lower = block.text.to_lowercase();
        if !block_has_marker(&block.text, &lower, profile) {
            continue;
        }
        let reference = match profile.find_tracker_reference(&block.text) {
            Some(m) => m.as_str().to_string(),
            None => continue,
        };
        let first_line = block
            .text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Warning,
            file: fp.relative_path.clone(),
            description: format!(
                "Upstream-bug workaround at lines {}-{}: {}",
                block.start_line,
                block.end_line,
                truncate(first_line)
            ),
            suggestion: format!(
                "Workaround references {}. Check whether the upstream issue/PR is closed or whether the fix has shipped — if so, remove this branch and its comment. Per the fix-upstream-first rule, workarounds should never outlive their cause.",
                reference
            ),
            kind: AuditFinding::UpstreamWorkaround,
        });
    }
    findings
}

fn block_has_marker(raw: &str, lower: &str, profile: &EffectiveDetectorProfile) -> bool {
    if profile
        .workaround_marker_literals
        .iter()
        .any(|m| lower.contains(m))
    {
        return true;
    }
    if profile
        .marker_regexes
        .iter()
        .any(|regex| regex.is_match(lower))
    {
        return true;
    }
    let leading_line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start()
        .to_lowercase();
    profile
        .workaround_leading_markers
        .iter()
        .any(|l| leading_line.starts_with(l))
}

// ============================================================================
// Tier B — version-compare guard pass
// ============================================================================

fn scan_version_guards(fp: &FileFingerprint, profile: &EffectiveDetectorProfile) -> Vec<Finding> {
    if !profile.language_allows_version_guard(&fp.language) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for regex in &profile.version_guard_regexes {
        for caps in regex.captures_iter(&fp.content) {
            let constant = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let version = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            if !profile
                .version_guard_constants
                .iter()
                .any(|c| c == constant)
            {
                continue;
            }
            let m = caps.get(0).unwrap();
            let line_number = fp.content[..m.start()]
                .chars()
                .filter(|c| *c == '\n')
                .count()
                + 1;
            findings.push(Finding {
                convention: "comment_hygiene".to_string(),
                severity: Severity::Info,
                file: fp.relative_path.clone(),
                description: version_guard_description(line_number, constant, version, m.as_str()),
                suggestion: format!(
                    "Branch only fires on {} < {}. If the minimum supported version is now ≥ {}, this branch is dead and can be removed.",
                    constant, version, version
                ),
                kind: AuditFinding::UpstreamWorkaround,
            });
        }
    }
    findings
}

struct EffectiveDetectorProfile {
    workaround_marker_literals: Vec<String>,
    workaround_leading_markers: Vec<String>,
    marker_regexes: Vec<Regex>,
    tracker_reference_regexes: Vec<Regex>,
    version_guard_regexes: Vec<Regex>,
    version_guard_constants: Vec<String>,
    version_guard_languages: Vec<String>,
    vendored_path_markers: Vec<String>,
}

impl EffectiveDetectorProfile {
    fn from_config(config: &DetectorProfileConfig) -> Self {
        let mut profile = Self {
            workaround_marker_literals: Vec::new(),
            workaround_leading_markers: Vec::new(),
            marker_regexes: Vec::new(),
            tracker_reference_regexes: Vec::new(),
            version_guard_regexes: Vec::new(),
            version_guard_constants: Vec::new(),
            version_guard_languages: Vec::new(),
            vendored_path_markers: Vec::new(),
        };

        if config.use_builtin_defaults {
            profile.extend_literals(MARKER_LITERALS, LEADING_MARKERS, VERSION_CONSTANTS);
            profile.extend_regexes(DEFAULT_MARKER_REGEXES, DetectorRegexKind::Marker);
            profile.extend_regexes(
                builtin_tracker_reference_regexes(),
                DetectorRegexKind::TrackerReference,
            );
            profile.extend_regexes(
                DEFAULT_VERSION_GUARD_REGEXES,
                DetectorRegexKind::VersionGuard,
            );
            profile.extend_strings(
                Language::builtin_version_guard_tokens(),
                DetectorProfileField::VersionGuardLanguage,
            );
            profile.extend_strings(
                DEFAULT_VENDORED_PATH_MARKERS,
                DetectorProfileField::VendoredPathMarker,
            );
        }

        profile.extend_owned_strings(
            &config.workaround_marker_literals,
            DetectorProfileField::WorkaroundMarkerLiteral,
        );
        profile.extend_owned_strings(
            &config.workaround_leading_markers,
            DetectorProfileField::WorkaroundLeadingMarker,
        );
        profile.extend_owned_regexes(&config.workaround_marker_regexes, DetectorRegexKind::Marker);
        profile.extend_owned_regexes(
            &config.tracker_reference_regexes,
            DetectorRegexKind::TrackerReference,
        );
        profile.extend_owned_regexes(
            &config.version_guard_regexes,
            DetectorRegexKind::VersionGuard,
        );
        profile.extend_owned_strings(
            &config.version_guard_constants,
            DetectorProfileField::VersionGuardConstant,
        );
        profile.extend_owned_strings(
            &config.version_guard_languages,
            DetectorProfileField::VersionGuardLanguage,
        );
        profile.extend_owned_strings(
            &config.vendored_path_markers,
            DetectorProfileField::VendoredPathMarker,
        );

        profile
    }

    fn find_tracker_reference<'a>(&self, text: &'a str) -> Option<regex::Match<'a>> {
        self.tracker_reference_regexes
            .iter()
            .find_map(|regex| regex.find(text))
    }

    fn is_vendored_path(&self, path: &str) -> bool {
        self.vendored_path_markers
            .iter()
            .any(|marker| path.starts_with(marker) || path.contains(marker))
    }

    fn language_allows_version_guard(&self, language: &Language) -> bool {
        self.version_guard_languages
            .iter()
            .any(|configured| language_matches(configured, language))
    }

    fn extend_literals(&mut self, markers: &[&str], leading: &[&str], constants: &[&str]) {
        self.extend_strings(markers, DetectorProfileField::WorkaroundMarkerLiteral);
        self.extend_strings(leading, DetectorProfileField::WorkaroundLeadingMarker);
        self.extend_strings(constants, DetectorProfileField::VersionGuardConstant);
    }

    fn extend_regexes(&mut self, patterns: &[&str], kind: DetectorRegexKind) {
        for pattern in patterns {
            if let Ok(regex) = Regex::new(pattern) {
                self.push_regex(regex, kind);
            }
        }
    }

    fn extend_owned_regexes(&mut self, patterns: &[String], kind: DetectorRegexKind) {
        for pattern in patterns {
            if let Ok(regex) = Regex::new(pattern) {
                self.push_regex(regex, kind);
            }
        }
    }

    fn extend_strings(&mut self, values: &[&str], field: DetectorProfileField) {
        for value in values {
            self.push_string((*value).to_string(), field);
        }
    }

    fn extend_owned_strings(&mut self, values: &[String], field: DetectorProfileField) {
        for value in values {
            self.push_string(value.to_string(), field);
        }
    }

    fn push_regex(&mut self, regex: Regex, kind: DetectorRegexKind) {
        match kind {
            DetectorRegexKind::Marker => self.marker_regexes.push(regex),
            DetectorRegexKind::TrackerReference => self.tracker_reference_regexes.push(regex),
            DetectorRegexKind::VersionGuard => self.version_guard_regexes.push(regex),
        }
    }

    fn push_string(&mut self, value: String, field: DetectorProfileField) {
        if value.trim().is_empty() {
            return;
        }
        let target = match field {
            DetectorProfileField::WorkaroundMarkerLiteral => &mut self.workaround_marker_literals,
            DetectorProfileField::WorkaroundLeadingMarker => &mut self.workaround_leading_markers,
            DetectorProfileField::VersionGuardConstant => &mut self.version_guard_constants,
            DetectorProfileField::VersionGuardLanguage => &mut self.version_guard_languages,
            DetectorProfileField::VendoredPathMarker => &mut self.vendored_path_markers,
        };
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

#[derive(Clone, Copy)]
enum DetectorRegexKind {
    Marker,
    TrackerReference,
    VersionGuard,
}

#[derive(Clone, Copy)]
enum DetectorProfileField {
    WorkaroundMarkerLiteral,
    WorkaroundLeadingMarker,
    VersionGuardConstant,
    VersionGuardLanguage,
    VendoredPathMarker,
}

fn language_matches(configured: &str, language: &Language) -> bool {
    language.matches_token(configured)
}

fn version_guard_description(
    line_number: usize,
    constant: &str,
    version: &str,
    matched: &str,
) -> String {
    if matched.trim_start().starts_with("version_compare") {
        return format!(
            "Version-compat guard at line {}: version_compare({}, '{}', '<')",
            line_number, constant, version
        );
    }

    format!(
        "Version-compat guard at line {}: {} < {}",
        line_number, constant, version
    )
}

// ============================================================================
// Helpers
// ============================================================================

fn truncate(s: &str) -> String {
    const MAX: usize = 120;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(MAX).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::conventions::Language;
    use crate::core::code_audit::fingerprint::FileFingerprint;

    fn make_fp(path: &str, lang: Language, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: lang,
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn default_config() -> DetectorProfileConfig {
        DetectorProfileConfig::default()
    }

    #[test]
    fn test_run_combines_tier_a_and_tier_b() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n/**\n * transitional shim\n * @see https://github.com/foo/bar/issues/1\n */\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {}\n",
        );
        let findings = run(&[&fp], &default_config());
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(findings.iter().any(|f| f.severity == Severity::Info));
    }

    #[test]
    fn test_marker_plus_github_url() {
        let fp = make_fp(
            "src/Api/WebhookSignatureVerifier.php",
            Language::Php,
            "<?php\n/**\n * Kept only as a transitional shim for older callers.\n *\n * @see https://github.com/Extra-Chill/sample-plugin/issues/1179\n * @deprecated\n */\nclass Verifier {}\n",
        );
        let findings = run(&[&fp], &default_config());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UpstreamWorkaround);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0]
            .suggestion
            .contains("github.com/Extra-Chill/sample-plugin/issues/1179"));
    }

    #[test]
    fn test_bare_see_design_reference_does_not_trigger() {
        let fp = make_fp(
            "src/Api/WebhookAuthResolver.php",
            Language::Php,
            "<?php\n/**\n * Webhook auth config resolver.\n *\n * Produces a canonical verifier config for an HMAC flow.\n *\n * @see https://github.com/Extra-Chill/sample-plugin/issues/1179\n */\nclass WebhookAuthResolver {}\n",
        );

        let findings = run(&[&fp], &default_config());
        assert!(
            findings.is_empty(),
            "bare @see references are design provenance, not workarounds"
        );
    }

    #[test]
    fn test_hack_comment_with_trac_ticket() {
        let fp = make_fp(
            "vendor-src/HtmlConverter.php",
            Language::Php,
            "<?php\n// Hack to load utf-8 HTML\n// see https://core.trac.wordpress.org/ticket/24730\n$x = 1;\n",
        );
        let findings = run(&[&fp], &default_config());
        assert_eq!(findings.len(), 1);
        assert!(findings[0]
            .suggestion
            .contains("core.trac.wordpress.org/ticket/24730"));
    }

    #[test]
    fn test_version_compare_guard_emits_finding() {
        let fp = make_fp(
            "akismet/class.akismet-admin.php",
            Language::Php,
            "<?php\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {\n    Jetpack::load_xml_rpc_client();\n}\n",
        );
        let findings = run(&[&fp], &default_config());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].description.contains("JETPACK__VERSION"));
    }

    #[test]
    fn test_legacy_without_reference_does_not_trigger() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n// legacy: do not remove\nfunction foo() {}\n",
        );
        let findings = run(&[&fp], &default_config());
        assert!(findings.is_empty());
    }

    #[test]
    fn test_vendor_paths_skipped() {
        let fp = make_fp(
            "vendor/league/html-to-markdown/src/HtmlConverter.php",
            Language::Php,
            "<?php\n// Hack to load utf-8 HTML\n// @see https://github.com/league/html-to-markdown/issues/212\n\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {}\n",
        );
        let findings = run(&[&fp], &default_config());
        assert!(findings.is_empty());
    }

    #[test]
    fn test_custom_profile_can_disable_builtin_wordpress_defaults() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n// Hack to load utf-8 HTML\n// see https://core.trac.wordpress.org/ticket/24730\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {}\n",
        );
        let config = DetectorProfileConfig {
            use_builtin_defaults: false,
            ..Default::default()
        };

        let findings = run(&[&fp], &config);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_custom_profile_adds_non_php_version_guard() {
        let fp = make_fp(
            "src/runtime.rs",
            Language::Rust,
            "fn compat() { if runtime_version_less_than(RUNTIME_VERSION, \"2.0\") {} }\n",
        );
        let config = DetectorProfileConfig {
            use_builtin_defaults: false,
            version_guard_regexes: vec![
                r#"runtime_version_less_than\((RUNTIME_VERSION),\s*\"([^\"]+)\"\)"#.to_string(),
            ],
            version_guard_constants: vec!["RUNTIME_VERSION".to_string()],
            version_guard_languages: vec!["rust".to_string()],
            ..Default::default()
        };

        let findings = run(&[&fp], &config);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("RUNTIME_VERSION"));
    }
}
