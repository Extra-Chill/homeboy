//! Compatibility wrapper for ecosystem terms leaking into core-owned source.

use crate::core::component::{
    CoreBoundaryLeakConfig, SourcePolicyMatchMode, SourcePolicyRule, SourcePolicyRuleBody,
    SourcePolicyTerm,
};

use super::findings::Finding;
use super::fingerprint::FileFingerprint;
use super::source_policy;

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    config: &CoreBoundaryLeakConfig,
) -> Vec<Finding> {
    if config.terms.is_empty() || config.scan_path_contains.is_empty() {
        return Vec::new();
    }

    let rule = SourcePolicyRule {
        id: "core-boundary-leak".to_string(),
        kind: "core_boundary_leak".to_string(),
        severity: "warning".to_string(),
        convention: "core_boundary_leak".to_string(),
        language: None,
        file_extensions: Vec::new(),
        include_path_contains: config.scan_path_contains.clone(),
        exclude_path_contains: config.allow_path_contains.clone(),
        allow_line_contains: config.allow_line_contains.clone(),
        ignore_line_prefixes: Vec::new(),
        ignore_after_line_equals: Vec::new(),
        example_path_contains: config.example_path_contains.clone(),
        example_classification: None,
        description:
            "Core boundary leak: configured ecosystem term `{term}` appears at line {line} in {classification} context `{context}`"
                .to_string(),
        suggestion: "Move ecosystem-specific behavior into extension metadata/rules, or add an explicit audit allowlist for intentional examples.".to_string(),
        rule: SourcePolicyRuleBody::ForbiddenTerms {
            terms: config
                .terms
                .iter()
                .filter_map(|term| source_policy_term(term))
                .collect(),
            default_match: SourcePolicyMatchMode::Literal,
            case_insensitive: true,
        },
    };

    source_policy::run(fingerprints, &[rule])
}

fn source_policy_term(term: &str) -> Option<SourcePolicyTerm> {
    let value = term.trim();
    if value.is_empty() {
        return None;
    }
    let match_mode = if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        SourcePolicyMatchMode::Token
    } else {
        SourcePolicyMatchMode::Literal
    };
    Some(SourcePolicyTerm {
        value: value.to_string(),
        label: None,
        match_mode: Some(match_mode),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::AuditFinding;
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
