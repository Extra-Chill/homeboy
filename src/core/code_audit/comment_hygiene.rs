//! Comment hygiene detection — identify stale/legacy comment markers.

use std::sync::LazyLock;

use regex::Regex;

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

const TODO_MARKERS: &[&str] = &["TODO", "FIXME", "HACK", "XXX"];
const LEGACY_MARKERS: &[&str] = &[
    "temporary",
    "workaround",
    "remove after",
    "legacy:",
    "outdated",
];

// ============================================================================
// Upstream workaround detection (Tier A: marker + tracker reference)
// ============================================================================

const WORKAROUND_MARKERS: &[&str] = &[
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
];

/// Phrase patterns checked as substrings (lowercased). Catches phrasing like
/// "until merged upstream", "for version of Jetpack prior to 7.7", "legacy v1".
const WORKAROUND_PHRASES: &[&str] = &[
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

/// Leading-line markers — only matched when they appear at the start of a
/// comment block (after the comment chars are stripped). Avoids false
/// positives like the word "Hackathon" mid-paragraph.
const WORKAROUND_LEADING: &[&str] = &["hack ", "hack:", "hack to", "hack for"];

/// Regex variant of "until X merged/landed/..." that also catches phrasing
/// like "until #1117 merges" where the verb tense varies.
static UNTIL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"until\s+\S+\s+(?:merged|landed|shipped|fixed|released|patched|merges|lands|ships|fixes|releases|patches|in core)\b")
        .unwrap()
});

static GITHUB_ISSUE_PR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https?://github\.com/[\w\-.]+/[\w\-.]+/(?:issues|pull)/(\d+)").unwrap()
});

static TRAC_TICKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"core\.trac\.wordpress\.org/ticket/(\d+)").unwrap());

static SEE_ISSUE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@see\s+(https?://[^\s)]+)").unwrap());

// ============================================================================
// Version-compare guard detection (Tier B)
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

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = analyze_comment_hygiene(fingerprints);
    findings.extend(find_upstream_workarounds(fingerprints));
    findings.extend(find_version_compat_guards(fingerprints));
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn analyze_comment_hygiene(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        for (line_number, comment) in extract_comments(fp) {
            if let Some(marker) = TODO_MARKERS.iter().find(|m| has_todo_marker(comment, m)) {
                findings.push(Finding {
                    convention: "comment_hygiene".to_string(),
                    severity: Severity::Info,
                    file: fp.relative_path.clone(),
                    description: format!(
                        "Comment marker '{}' found on line {}: {}",
                        marker,
                        line_number,
                        truncate_comment(comment)
                    ),
                    suggestion:
                        "Resolve or remove marker comments, or convert to a tracked issue reference"
                            .to_string(),
                    kind: AuditFinding::TodoMarker,
                });
            }

            if LEGACY_MARKERS.iter().any(|m| has_legacy_marker(comment, m)) {
                findings.push(Finding {
                    convention: "comment_hygiene".to_string(),
                    severity: Severity::Info,
                    file: fp.relative_path.clone(),
                    description: format!(
                        "Potential legacy/stale comment on line {}: {}",
                        line_number,
                        truncate_comment(comment)
                    ),
                    suggestion:
                        "Validate the comment is still accurate; remove or update stale implementation notes"
                            .to_string(),
                    kind: AuditFinding::LegacyComment,
                });
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

// ============================================================================
// Upstream workaround pass
// ============================================================================

/// A contiguous block of comment lines, joined for phrase scanning.
///
/// Block grouping matters because workaround markers (`// Hack to load utf-8 HTML`)
/// and tracker references (`@see https://...`) frequently sit on different lines
/// — sometimes 15 lines apart in a PHPDoc block. Per-line scanning would miss
/// the pair. The text field has comment markers (`//`, `*`, `#`) stripped per
/// line so substring matching is clean.
#[derive(Debug)]
struct CommentBlock {
    start_line: usize,
    end_line: usize,
    text: String,
}

fn find_upstream_workarounds(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        // Vendor exclusion is conservative for this rule only — LegacyComment
        // and TodoMarker still scan vendor files.
        if is_vendored_path(&fp.relative_path) {
            continue;
        }

        for block in extract_comment_blocks(fp) {
            let lower = block.text.to_lowercase();

            // Must have a marker AND a reference.
            let has_marker = block_has_workaround_marker(&block.text, &lower);
            if !has_marker {
                continue;
            }

            let reference_url = first_reference_url(&block.text);
            let reference_url = match reference_url {
                Some(url) => url,
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
                    truncate_comment(first_line)
                ),
                suggestion: format!(
                    "Workaround references {}. Check whether the upstream issue/PR is closed or whether the fix has shipped — if so, remove this branch and its comment. Per the fix-upstream-first rule, workarounds should never outlive their cause.",
                    reference_url
                ),
                kind: AuditFinding::UpstreamWorkaround,
            });
        }
    }

    findings
}

fn block_has_workaround_marker(raw: &str, lower: &str) -> bool {
    if WORKAROUND_MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    if WORKAROUND_PHRASES.iter().any(|p| lower.contains(p)) {
        return true;
    }
    if UNTIL_PATTERN.is_match(lower) {
        return true;
    }
    // Leading "Hack..." check on the first non-empty line, lowercased.
    let leading_line = raw
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start()
        .to_lowercase();
    if WORKAROUND_LEADING
        .iter()
        .any(|l| leading_line.starts_with(l))
    {
        return true;
    }
    false
}

/// Return the first GitHub issue/PR URL, Trac ticket URL, or `@see <url>`
/// reference found anywhere in the block, preferring concrete URLs over
/// bare references. Bare `#NNN` is intentionally NOT counted on its own —
/// callers require a marker AND a URL/ticket to emit a finding.
fn first_reference_url(raw: &str) -> Option<String> {
    if let Some(m) = GITHUB_ISSUE_PR.find(raw) {
        return Some(m.as_str().to_string());
    }
    if let Some(m) = TRAC_TICKET.find(raw) {
        return Some(m.as_str().to_string());
    }
    if let Some(caps) = SEE_ISSUE_URL.captures(raw) {
        return caps.get(1).map(|m| m.as_str().to_string());
    }
    None
}

fn is_vendored_path(path: &str) -> bool {
    path.contains("/vendor/")
        || path.starts_with("vendor/")
        || path.contains("/node_modules/")
        || path.starts_with("node_modules/")
}

// ============================================================================
// Comment block extraction
// ============================================================================

fn extract_comment_blocks(fp: &FileFingerprint) -> Vec<CommentBlock> {
    match fp.language {
        Language::Php | Language::Rust | Language::JavaScript | Language::TypeScript => {
            extract_blocks_generic(&fp.content, fp.language.clone())
        }
        _ => Vec::new(),
    }
}

/// State machine over file lines — recognizes:
/// - Contiguous `//` lines (any supported language) → one block.
/// - PHPDoc / JSDoc `/** … */` → one block.
/// - C-style `/* … */` → one block.
/// - `#` lines in PHP → one block.
/// - Adjacent comment regions separated by blank lines / code → separate blocks.
fn extract_blocks_generic(content: &str, lang: Language) -> Vec<CommentBlock> {
    let mut blocks = Vec::new();
    let allow_hash = matches!(lang, Language::Php);

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let raw = lines[i];
        let trimmed = raw.trim_start();

        // Block-style comment: /* ... */ or /** ... */
        if trimmed.starts_with("/*") {
            let start_line = i + 1;
            let mut text_lines: Vec<String> = Vec::new();
            let mut end_line = start_line;
            // First line: strip leading "/*" or "/**" and optional trailing "*/".
            let mut first = trimmed.trim_start_matches('/').trim_start_matches('*');
            let mut closed_on_first = false;
            if let Some(idx) = first.find("*/") {
                first = &first[..idx];
                closed_on_first = true;
            }
            text_lines.push(strip_block_line(first).to_string());

            if !closed_on_first {
                let mut j = i + 1;
                while j < lines.len() {
                    end_line = j + 1;
                    let l = lines[j];
                    if let Some(idx) = l.find("*/") {
                        let before = &l[..idx];
                        text_lines.push(strip_block_line(before).to_string());
                        break;
                    } else {
                        text_lines.push(strip_block_line(l).to_string());
                    }
                    j += 1;
                }
                i = j + 1;
            } else {
                i += 1;
            }

            blocks.push(CommentBlock {
                start_line,
                end_line,
                text: text_lines.join("\n"),
            });
            continue;
        }

        // Line-style: // (any language) or # (PHP only)
        let is_line_comment =
            trimmed.starts_with("//") && !trimmed.starts_with("///") && !trimmed.starts_with("//!")
                || (allow_hash && trimmed.starts_with('#') && !trimmed.starts_with("#!"));

        if is_line_comment {
            let start_line = i + 1;
            let mut text_lines: Vec<String> = Vec::new();
            let mut end_line = start_line;
            let mut j = i;
            while j < lines.len() {
                let lt = lines[j].trim_start();
                let is_cont =
                    lt.starts_with("//") && !lt.starts_with("///") && !lt.starts_with("//!")
                        || (allow_hash && lt.starts_with('#') && !lt.starts_with("#!"));
                if !is_cont {
                    break;
                }
                let stripped = lt
                    .trim_start_matches('/')
                    .trim_start_matches('/')
                    .trim_start_matches('#')
                    .trim();
                text_lines.push(stripped.to_string());
                end_line = j + 1;
                j += 1;
            }
            blocks.push(CommentBlock {
                start_line,
                end_line,
                text: text_lines.join("\n"),
            });
            i = j;
            continue;
        }

        i += 1;
    }

    blocks
}

/// Strip leading `*`, whitespace from one line of a `/* */` block so phrase
/// matching sees clean text.
fn strip_block_line(line: &str) -> &str {
    line.trim().trim_start_matches('*').trim()
}

// ============================================================================
// Version-compare guard pass (Tier B)
// ============================================================================

fn find_version_compat_guards(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        if !matches!(fp.language, Language::Php) {
            continue;
        }
        if is_vendored_path(&fp.relative_path) {
            continue;
        }

        for caps in VERSION_COMPARE_RE.captures_iter(&fp.content) {
            let constant = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let version = caps.get(2).map(|m| m.as_str()).unwrap_or("");

            // Only flag known constants — generic `version_compare` is too noisy.
            if !VERSION_CONSTANTS.contains(&constant) {
                continue;
            }

            // Locate the line of this match for a stable description.
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
    }

    findings
}

// ============================================================================
// Tier C — function_exists polyfill body detection (DEFERRED)
// ============================================================================
// TODO(upstream_workaround): tier C polyfill detection — see
// https://github.com/Extra-Chill/homeboy/issues/<n> for design discussion.
// Adjacent to dead_guard.rs and intentionally deferred from v1 to keep this
// PR scoped to the high-value tiers (A: marker+reference, B: version_compare).

// ============================================================================

fn extract_comments(fp: &FileFingerprint) -> Vec<(usize, &str)> {
    match fp.language {
        Language::Rust | Language::JavaScript | Language::TypeScript => fp
            .content
            .lines()
            .enumerate()
            .filter_map(|(idx, line)| {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//")
                    && !trimmed.starts_with("///")
                    && !trimmed.starts_with("//!")
                {
                    Some((idx + 1, trimmed.trim_start_matches('/').trim()))
                } else {
                    None
                }
            })
            .collect(),
        Language::Php => fp
            .content
            .lines()
            .enumerate()
            .filter_map(|(idx, line)| {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('#') {
                    Some((
                        idx + 1,
                        trimmed
                            .trim_start_matches('/')
                            .trim_start_matches('#')
                            .trim(),
                    ))
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn truncate_comment(comment: &str) -> String {
    const MAX_CHARS: usize = 120;
    let char_count = comment.chars().count();
    if char_count <= MAX_CHARS {
        comment.to_string()
    } else {
        let truncated: String = comment.chars().take(MAX_CHARS).collect();
        format!("{}...", truncated)
    }
}

fn has_todo_marker(comment: &str, marker: &str) -> bool {
    let normalized = normalized_comment(comment);
    let upper = normalized.to_uppercase();

    upper == marker
        || upper.starts_with(&format!("{}:", marker))
        || upper.starts_with(&format!("{} ", marker))
}

fn has_legacy_marker(comment: &str, marker: &str) -> bool {
    let normalized = normalized_comment(comment);
    let lower = normalized.to_lowercase();

    lower.starts_with(marker)
}

fn normalized_comment(comment: &str) -> &str {
    comment.trim_start_matches(['-', '*', ' ']).trim()
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
    fn test_analyze_comment_hygiene() {
        let fp = make_fp(
            "src/example.rs",
            Language::Rust,
            "// TODO: clean this up\n// temporary workaround for old API\nfn x() {}",
        );

        let findings = analyze_comment_hygiene(&[&fp]);
        assert!(findings.iter().any(|f| f.kind == AuditFinding::TodoMarker));
        assert!(findings
            .iter()
            .any(|f| f.kind == AuditFinding::LegacyComment));
    }

    #[test]
    fn test_extract_comments() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n# FIXME: later\n// HACK: now\n$ok = true;",
        );

        let comments = extract_comments(&fp);
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].0, 2);
        assert!(comments[0].1.contains("FIXME"));
    }

    #[test]
    fn test_truncate_comment_handles_multibyte() {
        let comment = format!("Phase 1 {}", "─".repeat(200));
        let truncated = truncate_comment(&comment);
        assert!(truncated.ends_with("..."));
        assert!(truncated.chars().count() <= 123);
    }

    #[test]
    fn test_truncate_comment() {
        let comment = "a".repeat(200);
        let truncated = truncate_comment(&comment);
        assert!(truncated.ends_with("..."));
        assert!(truncated.chars().count() <= 123);
    }

    #[test]
    fn test_has_todo_marker() {
        assert!(has_todo_marker("TODO: fix this", "TODO"));
        assert!(!has_todo_marker("documentation TODO section", "TODO"));
    }

    #[test]
    fn test_has_legacy_marker() {
        assert!(has_legacy_marker("temporary workaround", "temporary"));
        assert!(!has_legacy_marker("non temporary text", "temporary"));
        assert!(!has_legacy_marker(
            "Legacy hook fields are merged during deserialization",
            "legacy:"
        ));
    }

    #[test]
    fn test_normalized_comment() {
        assert_eq!(normalized_comment("// TODO: check"), "// TODO: check");
        assert_eq!(normalized_comment("- TODO: check"), "TODO: check");
        assert_eq!(normalized_comment("  * legacy note"), "legacy note");
    }

    // ========================================================================
    // Upstream workaround tests
    // ========================================================================

    #[test]
    fn test_upstream_workaround_marker_plus_github_url() {
        // PHPDoc block: a `transitional shim` marker plus an `@see <github URL>`
        // on a different line — the comment-block grouping is what makes the pair
        // matchable. Per-line scanning would miss this entirely.
        let fp = make_fp(
            "src/Api/WebhookSignatureVerifier.php",
            Language::Php,
            "<?php\n/**\n * Kept only as a transitional shim for older callers.\n *\n * @see https://github.com/Extra-Chill/data-machine/issues/1179\n * @deprecated\n */\nclass Verifier {}\n",
        );

        let findings = find_upstream_workarounds(&[&fp]);
        assert_eq!(findings.len(), 1, "expected exactly one finding");
        let f = &findings[0];
        assert_eq!(f.kind, AuditFinding::UpstreamWorkaround);
        assert_eq!(f.severity, Severity::Warning);
        assert!(
            f.suggestion
                .contains("github.com/Extra-Chill/data-machine/issues/1179"),
            "suggestion should surface the issue URL: {}",
            f.suggestion
        );
    }

    #[test]
    fn test_upstream_workaround_hack_comment_with_trac_ticket() {
        // Two adjacent `//` lines must be grouped into a single comment block.
        // If grouping is broken we'll see two findings; the assertion guards that.
        let fp = make_fp(
            "vendor-src/HtmlConverter.php",
            Language::Php,
            "<?php\n// Hack to load utf-8 HTML\n// see https://core.trac.wordpress.org/ticket/24730\n$x = 1;\n",
        );

        let findings = find_upstream_workarounds(&[&fp]);
        assert_eq!(
            findings.len(),
            1,
            "adjacent // lines should be grouped into one block, expected one finding, got {:?}",
            findings.iter().map(|f| &f.description).collect::<Vec<_>>()
        );
        assert_eq!(findings[0].kind, AuditFinding::UpstreamWorkaround);
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

        let findings = find_version_compat_guards(&[&fp]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UpstreamWorkaround);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].description.contains("JETPACK__VERSION"));
        assert!(findings[0].description.contains("7.7"));
    }

    #[test]
    fn test_legacy_comment_without_reference_does_not_trigger_workaround() {
        // Critical false-positive guard: a "legacy" comment with no URL/ticket
        // must NOT emit UpstreamWorkaround. It still emits LegacyComment via
        // analyze_comment_hygiene; the workaround pass is the one being conservative.
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n// legacy: do not remove\nfunction foo() {}\n",
        );

        let workaround_findings = find_upstream_workarounds(&[&fp]);
        assert!(
            workaround_findings.is_empty(),
            "expected no UpstreamWorkaround findings, got: {:?}",
            workaround_findings
                .iter()
                .map(|f| &f.description)
                .collect::<Vec<_>>()
        );

        // Sanity: the legacy-comment pass still flags this.
        let legacy_findings = analyze_comment_hygiene(&[&fp]);
        assert!(legacy_findings
            .iter()
            .any(|f| f.kind == AuditFinding::LegacyComment));
    }

    #[test]
    fn test_vendor_paths_skipped_by_default() {
        let fp = make_fp(
            "vendor/league/html-to-markdown/src/HtmlConverter.php",
            Language::Php,
            "<?php\n// Hack to load utf-8 HTML\n// @see https://github.com/league/html-to-markdown/issues/212\n$x = 1;\n",
        );

        let workaround_findings = find_upstream_workarounds(&[&fp]);
        assert!(
            workaround_findings.is_empty(),
            "vendor paths should be skipped by the upstream_workaround pass"
        );

        let version_findings = find_version_compat_guards(&[&make_fp(
            "vendor/foo/bar.php",
            Language::Php,
            "<?php\nif ( version_compare( JETPACK__VERSION, '7.7', '<' ) ) {}\n",
        )]);
        assert!(
            version_findings.is_empty(),
            "vendor paths should be skipped by the version_compare pass"
        );
    }

    #[test]
    fn test_extract_comment_blocks_groups_contiguous_lines() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n// line one\n// line two\n\n// separate block\n$x = 1;\n",
        );
        let blocks = extract_comment_blocks(&fp);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].start_line, 2);
        assert_eq!(blocks[0].end_line, 3);
        assert!(blocks[0].text.contains("line one"));
        assert!(blocks[0].text.contains("line two"));
        assert_eq!(blocks[1].start_line, 5);
    }

    #[test]
    fn test_extract_comment_blocks_phpdoc() {
        let fp = make_fp(
            "src/example.php",
            Language::Php,
            "<?php\n/**\n * Some doc\n * @see https://example.com/issues/1\n */\nclass A {}\n",
        );
        let blocks = extract_comment_blocks(&fp);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("Some doc"));
        assert!(blocks[0].text.contains("@see"));
    }
}
