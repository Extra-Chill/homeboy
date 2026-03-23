//! Auto-fix stale comment findings by removing the offending comment line.
//!
//! Handles `LegacyComment` and `TodoMarker` findings from the comment hygiene
//! audit. Both finding types include the line number in their description, so
//! the fix is a simple `DocLineRemoval`.

use std::path::Path;

use regex::Regex;

use crate::code_audit::{AuditFinding, CodeAuditResult};
use crate::refactor::auto::{Fix, SkippedFile};

use super::insertion;
use crate::refactor::auto::InsertionKind;

/// Generate fixes that remove legacy/stale comments and TODO markers.
///
/// Parses the line number from the finding description and emits a
/// `DocLineRemoval` insertion for each. These are safe to auto-apply
/// because the audit has already identified the comment as stale or
/// actionable.
pub(crate) fn generate_comment_fixes(
    result: &CodeAuditResult,
    _root: &Path,
    fixes: &mut Vec<Fix>,
    skipped: &mut Vec<SkippedFile>,
) {
    // Legacy comment: "Potential legacy/stale comment on line 206: ..."
    let legacy_re =
        Regex::new(r"Potential legacy/stale comment on line (\d+)").expect("regex should compile");

    // TODO marker: "Comment marker 'TODO' found on line 42: ..."
    let todo_re =
        Regex::new(r"Comment marker '[^']+' found on line (\d+)").expect("regex should compile");

    for finding in &result.findings {
        let (line_num, finding_kind) = match finding.kind {
            AuditFinding::LegacyComment => {
                let caps = match legacy_re.captures(&finding.description) {
                    Some(c) => c,
                    None => {
                        skipped.push(SkippedFile {
                            file: finding.file.clone(),
                            reason: format!(
                                "Could not parse line number from legacy comment: {}",
                                finding.description
                            ),
                        });
                        continue;
                    }
                };
                let line: usize = caps[1].parse().unwrap_or(0);
                if line == 0 {
                    continue;
                }
                (line, AuditFinding::LegacyComment)
            }
            AuditFinding::TodoMarker => {
                let caps = match todo_re.captures(&finding.description) {
                    Some(c) => c,
                    None => {
                        skipped.push(SkippedFile {
                            file: finding.file.clone(),
                            reason: format!(
                                "Could not parse line number from TODO marker: {}",
                                finding.description
                            ),
                        });
                        continue;
                    }
                };
                let line: usize = caps[1].parse().unwrap_or(0);
                if line == 0 {
                    continue;
                }
                (line, AuditFinding::TodoMarker)
            }
            _ => continue,
        };

        let ins = insertion(
            InsertionKind::DocLineRemoval { line: line_num },
            finding_kind,
            String::new(),
            format!(
                "Remove stale comment on line {} in {}",
                line_num, finding.file
            ),
        );

        fixes.push(Fix {
            file: finding.file.clone(),
            required_methods: vec![],
            required_registrations: vec![],
            insertions: vec![ins],
            applied: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::test_helpers::empty_result;
    use crate::code_audit::{Finding, Severity};

    #[test]
    fn generates_fix_for_legacy_comment() {
        let mut result = empty_result();
        result.findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Info,
            file: "src/commands/release.rs".to_string(),
            description:
                "Potential legacy/stale comment on line 206: --outdated: only components with unreleased code commits"
                    .to_string(),
            suggestion: "Validate the comment is still accurate".to_string(),
            kind: AuditFinding::LegacyComment,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, Path::new("/tmp"), &mut fixes, &mut skipped);

        assert_eq!(fixes.len(), 1, "Should generate one fix");
        assert_eq!(fixes[0].file, "src/commands/release.rs");
        assert_eq!(fixes[0].insertions.len(), 1);
        assert!(
            matches!(
                fixes[0].insertions[0].kind,
                InsertionKind::DocLineRemoval { line: 206 }
            ),
            "Should remove line 206"
        );
        assert!(skipped.is_empty(), "Should not skip anything");
    }

    #[test]
    fn generates_fix_for_todo_marker() {
        let mut result = empty_result();
        result.findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Info,
            file: "src/lib.rs".to_string(),
            description: "Comment marker 'TODO' found on line 42: implement caching".to_string(),
            suggestion: "Resolve or remove marker comments".to_string(),
            kind: AuditFinding::TodoMarker,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, Path::new("/tmp"), &mut fixes, &mut skipped);

        assert_eq!(fixes.len(), 1);
        assert!(matches!(
            fixes[0].insertions[0].kind,
            InsertionKind::DocLineRemoval { line: 42 }
        ));
    }

    #[test]
    fn skips_unparseable_description() {
        let mut result = empty_result();
        result.findings.push(Finding {
            convention: "comment_hygiene".to_string(),
            severity: Severity::Info,
            file: "src/lib.rs".to_string(),
            description: "Some weird format without line number".to_string(),
            suggestion: "".to_string(),
            kind: AuditFinding::LegacyComment,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, Path::new("/tmp"), &mut fixes, &mut skipped);

        assert!(fixes.is_empty(), "Should not generate a fix");
        assert_eq!(skipped.len(), 1, "Should skip the finding");
    }

    #[test]
    fn ignores_other_finding_kinds() {
        let mut result = empty_result();
        result.findings.push(Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/lib.rs".to_string(),
            description: "Something on line 10".to_string(),
            suggestion: "".to_string(),
            kind: AuditFinding::MissingMethod,
        });

        let mut fixes = Vec::new();
        let mut skipped = Vec::new();
        generate_comment_fixes(&result, Path::new("/tmp"), &mut fixes, &mut skipped);

        assert!(fixes.is_empty());
        assert!(skipped.is_empty());
    }
}
