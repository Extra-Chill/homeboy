//! Configurable detector for ecosystem terms leaking into core-owned source.

use regex::Regex;

use crate::core::component::CoreBoundaryLeakConfig;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    config: &CoreBoundaryLeakConfig,
) -> Vec<Finding> {
    if config.terms.is_empty() || config.scan_path_contains.is_empty() {
        return Vec::new();
    }

    let terms = config
        .terms
        .iter()
        .filter_map(|term| TermMatcher::new(term))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in fingerprints {
        if !path_matches(&fp.relative_path, &config.scan_path_contains)
            || path_matches(&fp.relative_path, &config.allow_path_contains)
        {
            continue;
        }

        for (index, line) in fp.content.lines().enumerate() {
            if line_matches(line, &config.allow_line_contains) {
                continue;
            }

            for term in &terms {
                if term.matches(line) {
                    let line_number = index + 1;
                    let classification =
                        if path_matches(&fp.relative_path, &config.example_path_contains) {
                            "example-only"
                        } else {
                            "behavioral"
                        };
                    let context = enclosing_context(&fp.content, index).unwrap_or("top-level");
                    findings.push(Finding {
                        convention: "core_boundary_leak".to_string(),
                        severity: Severity::Warning,
                        file: fp.relative_path.clone(),
                        description: format!(
                            "Core boundary leak: configured ecosystem term `{}` appears at line {} in {} context `{}`",
                            term.label, line_number, classification, context
                        ),
                        suggestion: "Move ecosystem-specific behavior into extension metadata/rules, or add an explicit audit allowlist for intentional examples.".to_string(),
                        kind: AuditFinding::CoreBoundaryLeak,
                    });
                }
            }
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

struct TermMatcher {
    label: String,
    regex: Regex,
}

impl TermMatcher {
    fn new(term: &str) -> Option<Self> {
        let label = term.trim();
        if label.is_empty() {
            return None;
        }

        let pattern = if label
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            format!(
                r"(?i)(^|[^A-Za-z0-9_]){}([^A-Za-z0-9_]|$)",
                regex::escape(label)
            )
        } else {
            format!(r"(?i){}", regex::escape(label))
        };

        Regex::new(&pattern).ok().map(|regex| Self {
            label: label.to_string(),
            regex,
        })
    }

    fn matches(&self, line: &str) -> bool {
        self.regex.is_match(line)
    }
}

fn path_matches(path: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| path.contains(needle))
}

fn line_matches(line: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| line.contains(needle))
}

fn enclosing_context(content: &str, line_index: usize) -> Option<&str> {
    let fn_regex = Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)").expect("fn regex compiles");
    let lines = content.lines().take(line_index + 1).collect::<Vec<_>>();
    lines
        .into_iter()
        .rev()
        .filter_map(|line| fn_regex.captures(line))
        .find_map(|captures| captures.get(1).map(|name| name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::Language;

    fn rust_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Rust,
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn config() -> CoreBoundaryLeakConfig {
        CoreBoundaryLeakConfig {
            terms: vec!["florpstack".to_string(), "florp-run".to_string()],
            scan_path_contains: vec!["src/core/".to_string()],
            allow_path_contains: vec!["src/core/fixtures/allowed".to_string()],
            allow_line_contains: vec!["homeboy-audit: allow-core-boundary-example".to_string()],
            example_path_contains: vec!["/fixtures/".to_string(), "/examples/".to_string()],
        }
    }

    #[test]
    fn test_run() {
        let fp = rust_fp("src/core/engine.rs", "fn dispatch() {}");

        assert!(run(&[&fp], &CoreBoundaryLeakConfig::default()).is_empty());
    }

    #[test]
    fn reports_configured_synthetic_ecosystem_terms_in_core_source() {
        let fp = rust_fp(
            "src/core/engine.rs",
            r#"fn dispatch() {
    run_tool("florp-run");
}
"#,
        );

        let findings = run(&[&fp], &config());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::CoreBoundaryLeak);
        assert!(findings[0].description.contains("florp-run"));
        assert!(findings[0].description.contains("behavioral"));
        assert!(findings[0].description.contains("dispatch"));
    }

    #[test]
    fn reports_unallowlisted_fixture_references_as_example_only() {
        let fp = rust_fp(
            "src/core/fixtures/leaky.rs",
            r#"const SAMPLE: &str = "florpstack";"#,
        );

        let findings = run(&[&fp], &config());

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("example-only"));
    }

    #[test]
    fn skips_explicit_path_and_line_allowlists() {
        let path_allowed = rust_fp(
            "src/core/fixtures/allowed/sample.rs",
            r#"const SAMPLE: &str = "florpstack";"#,
        );
        let line_allowed = rust_fp(
            "src/core/sample.rs",
            r#"// homeboy-audit: allow-core-boundary-example florpstack"#,
        );

        let findings = run(&[&path_allowed, &line_allowed], &config());

        assert!(findings.is_empty());
    }
}
