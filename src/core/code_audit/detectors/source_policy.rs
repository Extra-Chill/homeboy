//! Generic source-policy checks for component-owned architecture boundaries.

use regex::{Captures, Regex};
use std::collections::HashMap;
use std::str::FromStr;

use crate::core::component::{
    SourcePolicyMatchMode, SourcePolicyRule, SourcePolicyRuleBody, SourcePolicyTerm,
};

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    rules: &[SourcePolicyRule],
) -> Vec<Finding> {
    if rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for rule in rules {
        findings.extend(run_rule(rule, fingerprints));
    }
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn run_rule(rule: &SourcePolicyRule, fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    match &rule.rule {
        SourcePolicyRuleBody::ForbiddenTerms {
            terms,
            default_match,
            case_insensitive,
        } => run_forbidden_terms(rule, fingerprints, terms, default_match, *case_insensitive),
    }
}

fn run_forbidden_terms(
    rule: &SourcePolicyRule,
    fingerprints: &[&FileFingerprint],
    terms: &[SourcePolicyTerm],
    default_match: &SourcePolicyMatchMode,
    case_insensitive: bool,
) -> Vec<Finding> {
    let matchers = terms
        .iter()
        .filter_map(|term| TermMatcher::new(term, default_match, case_insensitive))
        .collect::<Vec<_>>();
    if matchers.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for fp in eligible_files(rule, fingerprints) {
        for (index, line) in fp.content.lines().enumerate() {
            let trimmed = line.trim_start();
            if rule
                .ignore_after_line_equals
                .iter()
                .any(|marker| line.trim() == marker)
            {
                break;
            }
            if rule
                .allow_line_contains
                .iter()
                .any(|marker| line.contains(marker))
                || rule
                    .ignore_line_prefixes
                    .iter()
                    .any(|prefix| trimmed.starts_with(prefix))
            {
                continue;
            }

            for matcher in &matchers {
                if matcher.matches(line) {
                    findings.push(finding_for_match(rule, fp, index, &matcher.label));
                }
            }
        }
    }
    findings
}

fn eligible_files<'a>(
    rule: &SourcePolicyRule,
    fingerprints: &'a [&FileFingerprint],
) -> Vec<&'a FileFingerprint> {
    fingerprints
        .iter()
        .filter(|fp| {
            if !rule.include_path_contains.is_empty()
                && !path_matches(&fp.relative_path, &rule.include_path_contains)
            {
                return false;
            }
            if path_matches(&fp.relative_path, &rule.exclude_path_contains) {
                return false;
            }
            if !rule.file_extensions.is_empty()
                && !rule.file_extensions.iter().any(|extension| {
                    fp.relative_path
                        .rsplit_once('.')
                        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
                })
            {
                return false;
            }
            if let Some(language) = &rule.language {
                if language_from_config(language).is_some_and(|language| fp.language != language) {
                    return false;
                }
            }
            true
        })
        .copied()
        .collect()
}

fn finding_for_match(
    rule: &SourcePolicyRule,
    fp: &FileFingerprint,
    line_index: usize,
    term: &str,
) -> Finding {
    let classification = if path_matches(&fp.relative_path, &rule.example_path_contains) {
        rule.example_classification
            .as_deref()
            .unwrap_or("example-only")
    } else {
        "behavioral"
    };
    let context = enclosing_context(&fp.content, line_index).unwrap_or("top-level");
    let line = (line_index + 1).to_string();
    let values = HashMap::from([
        ("term", term.to_string()),
        ("line", line),
        ("classification", classification.to_string()),
        ("context", context.to_string()),
    ]);

    Finding {
        convention: rule.convention.clone(),
        severity: severity_from_config(&rule.severity),
        file: fp.relative_path.clone(),
        description: render_template(&rule.description, &values),
        suggestion: render_template(&rule.suggestion, &values),
        kind: AuditFinding::from_str(&rule.kind).unwrap_or(AuditFinding::SourcePolicyViolation),
    }
}

struct TermMatcher {
    label: String,
    regex: Regex,
}

impl TermMatcher {
    fn new(
        term: &SourcePolicyTerm,
        default_match: &SourcePolicyMatchMode,
        case_insensitive: bool,
    ) -> Option<Self> {
        let value = term.value.trim();
        if value.is_empty() {
            return None;
        }
        let mode = term.match_mode.as_ref().unwrap_or(default_match);
        let pattern = match mode {
            SourcePolicyMatchMode::Token => token_pattern(value, case_insensitive),
            SourcePolicyMatchMode::Literal => literal_pattern(value, case_insensitive),
            SourcePolicyMatchMode::Regex => configured_regex_pattern(value, case_insensitive),
        };

        Regex::new(&pattern).ok().map(|regex| Self {
            label: term.label.clone().unwrap_or_else(|| value.to_string()),
            regex,
        })
    }

    fn matches(&self, line: &str) -> bool {
        self.regex.is_match(line)
    }
}

fn token_pattern(value: &str, case_insensitive: bool) -> String {
    format!(
        "{}(^|[^A-Za-z0-9_]){}([^A-Za-z0-9_]|$)",
        case_prefix(case_insensitive),
        regex::escape(value)
    )
}

fn literal_pattern(value: &str, case_insensitive: bool) -> String {
    format!("{}{}", case_prefix(case_insensitive), regex::escape(value))
}

fn configured_regex_pattern(value: &str, case_insensitive: bool) -> String {
    format!("{}{}", case_prefix(case_insensitive), value)
}

fn case_prefix(case_insensitive: bool) -> &'static str {
    if case_insensitive {
        "(?i)"
    } else {
        ""
    }
}

fn path_matches(path: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| path.contains(needle))
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

fn render_template(template: &str, values: &HashMap<&str, String>) -> String {
    let token = Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").expect("template regex compiles");
    token
        .replace_all(template, |caps: &Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            values.get(name).cloned().unwrap_or_default()
        })
        .to_string()
}

fn severity_from_config(value: &str) -> Severity {
    match value.trim().to_ascii_lowercase().as_str() {
        "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

fn language_from_config(value: &str) -> Option<Language> {
    let normalized = value.trim().to_ascii_lowercase();
    serde_json::from_value::<Language>(serde_json::Value::String(normalized.clone()))
        .ok()
        .or_else(|| {
            let language = Language::from_extension(&normalized);
            (language != Language::Unknown).then_some(language)
        })
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

    fn rule() -> SourcePolicyRule {
        SourcePolicyRule {
            id: "synthetic-source-boundary".to_string(),
            kind: "source_policy_violation".to_string(),
            severity: "warning".to_string(),
            convention: "source_policy".to_string(),
            language: Some("rust".to_string()),
            file_extensions: vec!["rs".to_string()],
            include_path_contains: vec!["src/core/".to_string()],
            exclude_path_contains: vec!["src/core/generated/".to_string()],
            allow_line_contains: vec!["homeboy-audit: allow-source-policy".to_string()],
            ignore_line_prefixes: vec!["//".to_string()],
            ignore_after_line_equals: vec!["#[cfg(test)]".to_string()],
            example_path_contains: vec!["/fixtures/".to_string()],
            example_classification: None,
            description:
                "Source policy term `{term}` appears at line {line} in {classification} context `{context}`"
                    .to_string(),
            suggestion: "Move `{term}` into component-owned policy.".to_string(),
            rule: SourcePolicyRuleBody::ForbiddenTerms {
                terms: vec![
                    SourcePolicyTerm {
                        value: "florpstack".to_string(),
                        label: None,
                        match_mode: None,
                    },
                    SourcePolicyTerm {
                        value: "florp-run".to_string(),
                        label: None,
                        match_mode: Some(SourcePolicyMatchMode::Literal),
                    },
                ],
                default_match: SourcePolicyMatchMode::Token,
                case_insensitive: true,
            },
        }
    }

    #[test]
    fn reports_configured_synthetic_terms_in_scoped_source() {
        let fp = rust_fp(
            "src/core/engine.rs",
            r#"fn dispatch() {
    run_tool("florp-run");
}
"#,
        );

        let findings = run(&[&fp], &[rule()]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::SourcePolicyViolation);
        assert!(findings[0].description.contains("florp-run"));
        assert!(findings[0].description.contains("behavioral"));
        assert!(findings[0].description.contains("dispatch"));
    }

    #[test]
    fn token_matching_avoids_substrings() {
        let fp = rust_fp("src/core/engine.rs", r#"let name = "florpstacked";"#);

        assert!(run(&[&fp], &[rule()]).is_empty());
    }

    #[test]
    fn skips_excluded_allowed_comment_and_test_lines() {
        let excluded = rust_fp(
            "src/core/generated/sample.rs",
            r#"let name = "florpstack";"#,
        );
        let allowed = rust_fp(
            "src/core/allowed.rs",
            r#"// homeboy-audit: allow-source-policy florpstack
let safe = "florpstacked";
"#,
        );
        let comment = rust_fp("src/core/comment.rs", r#"// florpstack example"#);
        let test_module = rust_fp(
            "src/core/tests.rs",
            r#"#[cfg(test)]
mod tests { const SAMPLE: &str = "florpstack"; }
"#,
        );

        assert!(run(&[&excluded, &allowed, &comment, &test_module], &[rule()]).is_empty());
    }

    #[test]
    fn marks_unallowlisted_fixture_matches_as_example_only() {
        let fp = rust_fp("src/core/fixtures/sample.rs", r#"let name = "florpstack";"#);

        let findings = run(&[&fp], &[rule()]);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("example-only"));
    }

    #[test]
    fn supports_regex_terms() {
        let mut rule = rule();
        rule.rule = SourcePolicyRuleBody::ForbiddenTerms {
            terms: vec![SourcePolicyTerm {
                value: r#"widget_[0-9]+"#.to_string(),
                label: Some("numbered widget".to_string()),
                match_mode: Some(SourcePolicyMatchMode::Regex),
            }],
            default_match: SourcePolicyMatchMode::Token,
            case_insensitive: false,
        };
        let fp = rust_fp("src/core/engine.rs", r#"let marker = "widget_42";"#);

        let findings = run(&[&fp], &[rule]);

        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("numbered widget"));
    }

    #[test]
    fn can_express_core_to_command_layer_dependency_boundary() {
        let mut rule = rule();
        rule.id = "core-layer-boundary".to_string();
        rule.include_path_contains = vec!["src/core/".to_string()];
        rule.ignore_line_prefixes = vec!["//".to_string(), "///".to_string(), "//!".to_string()];
        rule.rule = SourcePolicyRuleBody::ForbiddenTerms {
            terms: vec![SourcePolicyTerm {
                value: "crate::commands::".to_string(),
                label: Some("command-layer dependency".to_string()),
                match_mode: Some(SourcePolicyMatchMode::Literal),
            }],
            default_match: SourcePolicyMatchMode::Literal,
            case_insensitive: false,
        };
        let clean_comment = rust_fp(
            "src/core/comment.rs",
            "// crate::commands:: is documentation only",
        );
        let violation = rust_fp("src/core/engine.rs", "use crate::commands::audit;");

        let findings = run(&[&clean_comment, &violation], &[rule]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/core/engine.rs");
        assert!(findings[0].description.contains("command-layer dependency"));
    }

    #[test]
    fn can_express_detector_domain_agnostic_policy() {
        let mut rule = rule();
        rule.id = "detector-domain-agnostic".to_string();
        rule.include_path_contains = vec!["src/core/code_audit/detectors/".to_string()];
        rule.ignore_after_line_equals = vec!["#[cfg(test)]".to_string()];
        rule.rule = SourcePolicyRuleBody::ForbiddenTerms {
            terms: vec![
                SourcePolicyTerm {
                    value: "WidgetLang".to_string(),
                    label: None,
                    match_mode: Some(SourcePolicyMatchMode::Token),
                },
                SourcePolicyTerm {
                    value: "widget-package.json".to_string(),
                    label: None,
                    match_mode: Some(SourcePolicyMatchMode::Literal),
                },
            ],
            default_match: SourcePolicyMatchMode::Token,
            case_insensitive: true,
        };
        let production = rust_fp(
            "src/core/code_audit/detectors/generic.rs",
            "let marker = \"WidgetLang\";",
        );
        let tests = rust_fp(
            "src/core/code_audit/detectors/generic_test.rs",
            r#"#[cfg(test)]
mod tests { const PACKAGE: &str = "widget-package.json"; }
"#,
        );

        let findings = run(&[&production, &tests], &[rule]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/core/code_audit/detectors/generic.rs");
        assert!(findings[0].description.contains("WidgetLang"));
    }
}
