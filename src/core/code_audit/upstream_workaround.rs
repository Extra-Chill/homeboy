//! Upstream-bug workaround detection — flag code that exists because of a
//! tracked upstream bug. Two tiers:
//!
//! - **A (Warning):** marker keyword (`workaround`, `polyfill`, `shim`,
//!   `// Hack`, `until merged`, `legacy fallback`, …) AND a concrete tracker
//!   reference (`github.com/.../issues|pull/N`, `core.trac.wordpress.org/ticket/N`,
//!   or `@see <url>`) co-located in the same contiguous comment block. Bare
//!   `#NNN` does not qualify on its own.
//! - **B (Info):** `version_compare(<KNOWN_CONSTANT>, '<X>', '<' | '<=')`
//!   guards against a known plugin/PHP/WP constant.
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

use std::sync::LazyLock;

use regex::Regex;

use super::comment_blocks;
use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

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

/// Regex variant of "until X merged/landed/..." — catches phrasing like
/// "until #1117 merges" where the verb tense varies.
static UNTIL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"until\s+\S+\s+(?:merged|landed|shipped|fixed|released|patched|merges|lands|ships|fixes|releases|patches|in core)\b")
        .unwrap()
});

/// Single alternation regex covering all tracker-reference shapes. Bare `#NNN`
/// is intentionally NOT included — Tier A requires a marker AND a concrete
/// URL/ticket, never a bare reference.
static REFERENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(https?://github\.com/[\w\-.]+/[\w\-.]+/(?:issues|pull)/\d+|core\.trac\.wordpress\.org/ticket/\d+|@see\s+https?://[^\s)]+)",
    )
    .unwrap()
});

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

static VERSION_COMPARE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"version_compare\s*\(\s*([A-Z_][A-Z0-9_]*|\$wp_version|PHP_VERSION)\s*,\s*['"]([^'"]+)['"]\s*,\s*['"]<=?['"]\s*\)"#,
    )
    .unwrap()
});

// ============================================================================
// Public entry point
// ============================================================================

/// Run both upstream-workaround tiers across the fingerprint set. Vendored
/// paths (`/vendor/`, `/node_modules/`) are skipped — `LegacyComment` and
/// `TodoMarker` still scan vendor files; only this rule is conservative.
pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for fp in fingerprints {
        if is_vendored_path(&fp.relative_path) {
            continue;
        }
        findings.extend(scan_blocks(fp));
        findings.extend(scan_version_guards(fp));
    }
    findings
}

fn is_vendored_path(path: &str) -> bool {
    path.contains("/vendor/")
        || path.starts_with("vendor/")
        || path.contains("/node_modules/")
        || path.starts_with("node_modules/")
}

// ============================================================================
// Tier A — marker + reference pass
// ============================================================================

fn scan_blocks(fp: &FileFingerprint) -> Vec<Finding> {
    let mut findings = Vec::new();
    for block in comment_blocks::extract(fp) {
        let lower = block.text.to_lowercase();
        if !block_has_marker(&block.text, &lower) {
            continue;
        }
        let reference = match REFERENCE_RE.find(&block.text) {
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

fn block_has_marker(raw: &str, lower: &str) -> bool {
    if MARKER_LITERALS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    if UNTIL_PATTERN.is_match(lower) {
        return true;
    }
    let leading_line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start()
        .to_lowercase();
    LEADING_MARKERS.iter().any(|l| leading_line.starts_with(l))
}

// ============================================================================
// Tier B — version-compare guard pass
// ============================================================================

fn scan_version_guards(fp: &FileFingerprint) -> Vec<Finding> {
    if !matches!(fp.language, Language::Php) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for caps in VERSION_COMPARE_RE.captures_iter(&fp.content) {
        let constant = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let version = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        if !VERSION_CONSTANTS.contains(&constant) {
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
            description: format!(
                "Version-compat guard at line {}: version_compare({}, '{}', '<')",
                line_number, constant, version
            ),
            suggestion: format!(
                "Branch only fires on {} < {}. If the minimum supported version is now ≥ {}, this branch is dead and can be removed.",
                constant, version, version
            ),
            kind: AuditFinding::UpstreamWorkaround,
        });
    }
    findings
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
    use crate::code_audit::conventions::Language;
    use crate::code_audit::fingerprint::FileFingerprint;

    fn make_fp(path: &str, lang: Language, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: lang,
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_run_combines_tier_a_and_tier_b() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n/**\n * transitional shim\n * @see https://github.com/foo/bar/issues/1\n */\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {}\n",
        );
        let findings = run(&[&fp]);
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().any(|f| f.severity == Severity::Warning));
        assert!(findings.iter().any(|f| f.severity == Severity::Info));
    }

    #[test]
    fn test_marker_plus_github_url() {
        let fp = make_fp(
            "src/Api/WebhookSignatureVerifier.php",
            Language::Php,
            "<?php\n/**\n * Kept only as a transitional shim for older callers.\n *\n * @see https://github.com/Extra-Chill/data-machine/issues/1179\n * @deprecated\n */\nclass Verifier {}\n",
        );
        let findings = run(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UpstreamWorkaround);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0]
            .suggestion
            .contains("github.com/Extra-Chill/data-machine/issues/1179"));
    }

    #[test]
    fn test_bare_see_design_reference_does_not_trigger() {
        let fp = make_fp(
            "src/Api/WebhookAuthResolver.php",
            Language::Php,
            "<?php\n/**\n * Webhook auth config resolver.\n *\n * Produces a canonical verifier config for an HMAC flow.\n *\n * @see https://github.com/Extra-Chill/data-machine/issues/1179\n */\nclass WebhookAuthResolver {}\n",
        );

        let findings = run(&[&fp]);
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
        let findings = run(&[&fp]);
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
        let findings = run(&[&fp]);
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
        let findings = run(&[&fp]);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_vendor_paths_skipped() {
        let fp = make_fp(
            "vendor/league/html-to-markdown/src/HtmlConverter.php",
            Language::Php,
            "<?php\n// Hack to load utf-8 HTML\n// @see https://github.com/league/html-to-markdown/issues/212\n\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {}\n",
        );
        let findings = run(&[&fp]);
        assert!(findings.is_empty());
    }
}
