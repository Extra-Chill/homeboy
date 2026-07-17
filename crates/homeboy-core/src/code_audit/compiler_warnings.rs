//! Surface compiler warnings (dead code, unused imports, unused variables) as audit findings.
//!
//! Runs extension-owned compiler/checker scripts and maps their structured output
//! into audit findings.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/636

use std::path::Path;

use super::compiler_warning_provider::AuditCompilerWarning;
use super::{AuditFinding, Finding, Severity};

/// Run compiler checks and return findings for any warnings detected.
pub fn run(root: &Path) -> Vec<Finding> {
    warnings_to_findings(super::compiler_warning_provider::compiler_warnings_for_root(root))
}

/// Map the raw compiler warnings from the provider into audit findings, dropping
/// warnings with empty or absolute file paths (which can't be attributed to a
/// component-relative source file).
fn warnings_to_findings(warnings: Vec<AuditCompilerWarning>) -> Vec<Finding> {
    warnings
        .into_iter()
        .filter(|warning| !warning.file.is_empty() && !warning.file.starts_with('/'))
        .map(|warning| Finding {
            file: warning.file.clone(),
            kind: AuditFinding::CompilerWarning,
            severity: Severity::Warning,
            convention: "compiler".to_string(),
            description: format!("[{}] {}", warning.code, warning.message),
            suggestion: warning
                .suggestion
                .unwrap_or_else(|| format!("Address compiler warning: {}", warning.code)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warning(
        code: &str,
        message: &str,
        file: &str,
        suggestion: Option<&str>,
    ) -> AuditCompilerWarning {
        AuditCompilerWarning {
            code: code.to_string(),
            message: message.to_string(),
            file: file.to_string(),
            suggestion: suggestion.map(str::to_string),
        }
    }

    #[test]
    fn maps_provider_warnings_into_findings() {
        let findings = warnings_to_findings(vec![warning(
            "unused_imports",
            "unused import",
            "src/lib.rs",
            Some("Remove import"),
        )]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/lib.rs");
        assert_eq!(findings[0].kind, AuditFinding::CompilerWarning);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].description, "[unused_imports] unused import");
        assert_eq!(findings[0].suggestion, "Remove import");
    }

    #[test]
    fn drops_absolute_and_empty_paths_and_defaults_suggestion() {
        let findings = warnings_to_findings(vec![
            warning("abs", "absolute path", "/etc/passwd", None),
            warning("empty", "empty path", "", None),
            warning("dead_code", "never used", "src/x.rs", None),
        ]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/x.rs");
        assert_eq!(
            findings[0].suggestion,
            "Address compiler warning: dead_code"
        );
    }
}
