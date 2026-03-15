use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{Fix, InsertionKind};
use std::path::Path;

use super::insertion;

pub(crate) fn extract_suggested_path(suggestion: &str) -> Option<String> {
    // Support both suggestion formats:
    //   "Replace with `new/path`"
    //   "Did you mean `new/path`?"
    let needles = ["Replace with `", "Did you mean `"];
    for needle in needles {
        if let Some(pos) = suggestion.find(needle) {
            let start = pos + needle.len();
            let rest = &suggestion[start..];
            let end = rest.find('`')?;
            return Some(rest[..end].to_string());
        }
    }
    None
}

pub(crate) fn should_remove_broken_doc_line(line: &str, dead_path: &str) -> bool {
    let trimmed = line.trim();
    let bullet_like = trimmed.starts_with('-') || trimmed.starts_with('*');
    bullet_like && trimmed.contains(dead_path)
}

pub(crate) fn is_actionable_comment_finding(kind: &AuditFinding) -> bool {
    matches!(kind, AuditFinding::TodoMarker | AuditFinding::LegacyComment)
}

pub(crate) fn extract_stale_ref_path(description: &str) -> Option<String> {
    let start = description.find('`')? + 1;
    let rest = &description[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

pub(crate) fn extract_line_number(description: &str) -> Option<usize> {
    let start = description.find("(line ")? + "(line ".len();
    let rest = &description[start..];
    let end = rest.find(')')?;
    rest[..end].parse().ok()
}

pub(super) fn apply_stale_doc_reference_fixes(result: &CodeAuditResult, fixes: &mut Vec<Fix>) {
    for finding in &result.findings {
        if finding.kind != AuditFinding::StaleDocReference {
            continue;
        }

        let Some(new_path) = extract_suggested_path(&finding.suggestion) else {
            continue;
        };
        let Some(old_path) = extract_stale_ref_path(&finding.description) else {
            continue;
        };

        let line_num = extract_line_number(&finding.description).unwrap_or(0);
        if line_num == 0 {
            continue;
        }

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![insertion(
                InsertionKind::DocReferenceUpdate {
                    line: line_num,
                    old_ref: old_path.clone(),
                    new_ref: new_path.clone(),
                },
                AuditFinding::StaleDocReference,
                format!("{} → {}", old_path, new_path),
                format!(
                    "Update stale reference: `{}` → `{}` (line {})",
                    old_path, new_path, line_num
                ),
            )],
            applied: false,
        });
    }
}

pub(super) fn apply_broken_doc_reference_fixes(
    result: &CodeAuditResult,
    root: &Path,
    fixes: &mut Vec<Fix>,
) {
    for finding in &result.findings {
        if finding.kind != AuditFinding::BrokenDocReference {
            continue;
        }

        let Some(dead_path) = extract_stale_ref_path(&finding.description) else {
            continue;
        };
        let Some(line_num) = extract_line_number(&finding.description) else {
            continue;
        };

        let abs_path = root.join(&finding.file);
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            continue;
        };
        let Some(line) = content.lines().nth(line_num.saturating_sub(1)) else {
            continue;
        };

        if !should_remove_broken_doc_line(line, &dead_path) {
            continue;
        }

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![insertion(
                InsertionKind::DocLineRemoval { line: line_num },
                AuditFinding::BrokenDocReference,
                dead_path.clone(),
                format!(
                    "Remove dead documentation reference line for `{}` (line {})",
                    dead_path, line_num
                ),
            )],
            applied: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_suggested_path_replace_with_format() {
        let suggestion = "Replace with `src/core/engine/shell.rs`";
        assert_eq!(
            extract_suggested_path(suggestion),
            Some("src/core/engine/shell.rs".to_string())
        );
    }

    #[test]
    fn extract_suggested_path_did_you_mean_format() {
        let suggestion = "Did you mean `src/core/engine/shell.rs`? File 'src/utils/shell.rs' no longer exists at the documented path.";
        assert_eq!(
            extract_suggested_path(suggestion),
            Some("src/core/engine/shell.rs".to_string())
        );
    }

    #[test]
    fn extract_suggested_path_did_you_mean_directory() {
        let suggestion = "Did you mean `src/core/release/changelog/`? Directory 'src/core/changelog/' no longer exists at the documented path.";
        assert_eq!(
            extract_suggested_path(suggestion),
            Some("src/core/release/changelog/".to_string())
        );
    }

    #[test]
    fn extract_suggested_path_no_match() {
        let suggestion = "File no longer exists. Update or remove this reference.";
        assert_eq!(extract_suggested_path(suggestion), None);
    }

    #[test]
    fn should_remove_broken_doc_line_bullet_with_path() {
        assert!(should_remove_broken_doc_line(
            "- **Location:** `src/core/ssh/`",
            "src/core/ssh/"
        ));
    }

    #[test]
    fn should_remove_broken_doc_line_prose_not_bullet() {
        assert!(!should_remove_broken_doc_line(
            "For example, in a project containing `src/widget/widget.rs`",
            "src/widget/widget.rs"
        ));
    }

    #[test]
    fn extract_stale_ref_path_from_description() {
        let desc = "Stale file reference `src/utils/shell.rs` (line 87) — target has moved";
        assert_eq!(
            extract_stale_ref_path(desc),
            Some("src/utils/shell.rs".to_string())
        );
    }

    #[test]
    fn extract_line_number_from_description() {
        let desc = "Stale file reference `src/utils/shell.rs` (line 87) — target has moved";
        assert_eq!(extract_line_number(desc), Some(87));
    }
}
