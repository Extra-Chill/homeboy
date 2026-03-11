use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::core::refactor::auto::{Fix, InsertionKind};
use std::path::Path;

use super::insertion;

pub(crate) fn extract_suggested_path(suggestion: &str) -> Option<String> {
    let needle = "Replace with `";
    let start = suggestion.find(needle)? + needle.len();
    let rest = &suggestion[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
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
