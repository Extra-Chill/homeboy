//! Comment hygiene detection — identify stale/legacy comment markers.
//!
//! The upstream-bug-workaround tier (marker + tracker reference, plus
//! `version_compare` guards) lives in the sibling `upstream_workaround`
//! module so this file stays focused on the simple per-line passes.

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::upstream_workaround;

const TODO_MARKERS: &[&str] = &["TODO", "FIXME", "HACK", "XXX"];
const LEGACY_MARKERS: &[&str] = &[
    "temporary",
    "workaround",
    "remove after",
    "legacy:",
    "outdated",
];

pub(super) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = analyze_comment_hygiene(fingerprints);
    findings.extend(upstream_workaround::run(fingerprints));
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn analyze_comment_hygiene(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        for (line_number, comment) in extract_comments(fp) {
            if let Some(marker) = TODO_MARKERS.iter().find(|m| has_todo_marker(comment, m)) {
                findings.push(make_finding(
                    fp,
                    AuditFinding::TodoMarker,
                    Severity::Info,
                    format!(
                        "Comment marker '{}' found on line {}: {}",
                        marker,
                        line_number,
                        truncate_comment(comment)
                    ),
                    "Resolve or remove marker comments, or convert to a tracked issue reference"
                        .to_string(),
                ));
            }

            if LEGACY_MARKERS.iter().any(|m| has_legacy_marker(comment, m)) {
                findings.push(make_finding(
                    fp,
                    AuditFinding::LegacyComment,
                    Severity::Info,
                    format!(
                        "Potential legacy/stale comment on line {}: {}",
                        line_number,
                        truncate_comment(comment)
                    ),
                    "Validate the comment is still accurate; remove or update stale implementation notes"
                        .to_string(),
                ));
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

/// Build a `Finding` with the comment-hygiene convention prefilled. Centralizes
/// the `Finding { ... }` construction shape so per-rule call sites stay short
/// and don't repeat the same five fields.
fn make_finding(
    fp: &FileFingerprint,
    kind: AuditFinding,
    severity: Severity,
    description: String,
    suggestion: String,
) -> Finding {
    Finding {
        convention: "comment_hygiene".to_string(),
        severity,
        file: fp.relative_path.clone(),
        description,
        suggestion,
        kind,
    }
}

fn extract_comments(fp: &FileFingerprint) -> Vec<(usize, &str)> {
    let allow_hash = matches!(fp.language, Language::Php);
    if !matches!(
        fp.language,
        Language::Php | Language::Rust | Language::JavaScript | Language::TypeScript
    ) {
        return Vec::new();
    }

    fp.content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let trimmed = line.trim_start();
            let is_slash = trimmed.starts_with("//")
                && !trimmed.starts_with("///")
                && !trimmed.starts_with("//!");
            let is_hash = allow_hash && trimmed.starts_with('#') && !trimmed.starts_with("#!");
            if is_slash || is_hash {
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
        .collect()
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
    fn test_run() {
        // Smoke test for the public entry: ensure run() composes both
        // analyze_comment_hygiene + upstream_workaround. Real coverage of
        // each tier lives in their respective focused tests.
        let fp = make_fp(
            "src/example.rs",
            Language::Rust,
            "// TODO: fix later\nfn x() {}\n",
        );
        let findings = run(&[&fp]);
        assert!(findings.iter().any(|f| f.kind == AuditFinding::TodoMarker));
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
}
