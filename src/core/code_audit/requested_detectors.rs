//! Extension-owned requested detector rule-pack execution.

use regex::{Captures, Regex};
use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use crate::core::component::{AuditConfig, RequestedDetectorRule, RequestedDetectorRuleBody};

use super::comment_blocks;
use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::source_locations::line_of_offset;

#[derive(Debug, Clone)]
struct DerivedValue {
    value: String,
    label: String,
    file: String,
}

#[derive(Debug, Clone)]
struct DerivedLiteralSite {
    file: String,
    line: usize,
    value: String,
    captures: HashMap<String, String>,
    labels: Vec<String>,
}

pub(super) fn run(fingerprints: &[&FileFingerprint], audit_config: &AuditConfig) -> Vec<Finding> {
    if audit_config.requested_detectors.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for rule in &audit_config.requested_detectors {
        findings.extend(run_rule(rule, fingerprints));
    }
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn run_rule(rule: &RequestedDetectorRule, fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    match &rule.rule {
        RequestedDetectorRuleBody::Regex {
            pattern,
            description,
            suggestion,
        } => run_regex_rule(rule, fingerprints, pattern, description, suggestion),
        RequestedDetectorRuleBody::CommentRegex {
            comment_pattern,
            comment_exclude_pattern,
            pattern,
            description,
            suggestion,
        } => run_comment_regex_rule(
            rule,
            fingerprints,
            comment_pattern,
            comment_exclude_pattern.as_deref(),
            pattern,
            description,
            suggestion,
        ),
        RequestedDetectorRuleBody::DerivedLiteral {
            source_pattern,
            value_capture,
            label,
            literal_pattern,
            exclude_match_context_patterns,
            description,
            suggestion,
        } => run_derived_literal_rule(
            rule,
            fingerprints,
            source_pattern,
            value_capture,
            label,
            literal_pattern,
            exclude_match_context_patterns,
            description,
            suggestion,
        ),
    }
}

fn run_regex_rule(
    rule: &RequestedDetectorRule,
    fingerprints: &[&FileFingerprint],
    pattern: &str,
    description: &str,
    suggestion: &str,
) -> Vec<Finding> {
    let Ok(regex) = Regex::new(pattern) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for fp in eligible_files(rule, fingerprints) {
        for captures in regex.captures_iter(&fp.content) {
            findings.push(finding_from_captures(
                rule,
                fp,
                &captures,
                description,
                suggestion,
            ));
        }
    }
    findings
}

fn run_comment_regex_rule(
    rule: &RequestedDetectorRule,
    fingerprints: &[&FileFingerprint],
    comment_pattern: &str,
    comment_exclude_pattern: Option<&str>,
    pattern: &str,
    description: &str,
    suggestion: &str,
) -> Vec<Finding> {
    let Ok(comment_regex) = Regex::new(comment_pattern) else {
        return Vec::new();
    };
    let comment_exclude_regex =
        comment_exclude_pattern.and_then(|pattern| Regex::new(pattern).ok());
    let Ok(regex) = Regex::new(pattern) else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    for fp in eligible_files(rule, fingerprints) {
        let comment_blocks = comment_blocks::extract(fp);
        if comment_blocks.iter().any(|block| {
            comment_exclude_regex
                .as_ref()
                .is_some_and(|regex| regex.is_match(&block.text))
        }) {
            continue;
        }
        if !comment_blocks
            .iter()
            .any(|block| comment_regex.is_match(&block.text))
        {
            continue;
        }
        for captures in regex.captures_iter(&fp.content) {
            findings.push(finding_from_captures(
                rule,
                fp,
                &captures,
                description,
                suggestion,
            ));
        }
    }
    findings
}

#[allow(clippy::too_many_arguments)]
fn run_derived_literal_rule(
    rule: &RequestedDetectorRule,
    fingerprints: &[&FileFingerprint],
    source_pattern: &str,
    value_capture: &str,
    label: &str,
    literal_pattern: &str,
    exclude_match_context_patterns: &[String],
    description: &str,
    suggestion: &str,
) -> Vec<Finding> {
    let Ok(source_regex) = Regex::new(source_pattern) else {
        return Vec::new();
    };

    let values = collect_derived_values(
        &source_regex,
        eligible_files(rule, fingerprints),
        value_capture,
        label,
    );
    if values.is_empty() {
        return Vec::new();
    }

    let mut sites: BTreeMap<(String, usize, String), DerivedLiteralSite> = BTreeMap::new();
    for value in values {
        let concrete_pattern = render_template(literal_pattern, None, |name| match name {
            "value" => value.value.clone(),
            "label" => value.label.clone(),
            _ => String::new(),
        });
        let Ok(literal_regex) = Regex::new(&concrete_pattern) else {
            continue;
        };
        let exclude_regexes = exclude_match_context_patterns
            .iter()
            .filter_map(|pattern| {
                let concrete_pattern = render_template(pattern, None, |name| match name {
                    "value" => value.value.clone(),
                    "label" => value.label.clone(),
                    _ => String::new(),
                });
                Regex::new(&concrete_pattern).ok()
            })
            .collect::<Vec<_>>();
        for fp in eligible_files(rule, fingerprints) {
            if fp.relative_path == value.file {
                continue;
            }
            for captures in literal_regex.captures_iter(&fp.content) {
                if match_context_is_excluded(&fp.content, &captures, &exclude_regexes) {
                    continue;
                }
                let offset = captures.get(0).map(|m| m.start()).unwrap_or(0);
                let line = line_of_offset(&fp.content, offset);
                let key = (fp.relative_path.clone(), line, value.value.clone());
                let site = sites.entry(key).or_insert_with(|| DerivedLiteralSite {
                    file: fp.relative_path.clone(),
                    line,
                    value: value.value.clone(),
                    captures: capture_values(&literal_regex, &captures),
                    labels: Vec::new(),
                });
                if !site.labels.contains(&value.label) {
                    site.labels.push(value.label.clone());
                }
            }
        }
    }
    sites
        .into_values()
        .map(|site| finding_from_derived_literal_site(rule, &site, description, suggestion))
        .collect()
}

fn match_context_is_excluded(
    content: &str,
    captures: &Captures,
    exclude_regexes: &[Regex],
) -> bool {
    let Some(match_) = captures.get(0) else {
        return false;
    };
    let context = line_at_offset(content, match_.start());
    exclude_regexes.iter().any(|regex| regex.is_match(context))
}

fn collect_derived_values(
    source_regex: &Regex,
    files: Vec<&FileFingerprint>,
    value_capture: &str,
    label_template: &str,
) -> Vec<DerivedValue> {
    let mut values = Vec::new();
    for fp in files {
        for captures in source_regex.captures_iter(&fp.content) {
            let value = capture_value(&captures, value_capture);
            if value.is_empty() {
                continue;
            }
            values.push(DerivedValue {
                label: render_template(label_template, Some(&captures), |_| String::new()),
                value,
                file: fp.relative_path.clone(),
            });
        }
    }
    values
}

fn eligible_files<'a>(
    rule: &RequestedDetectorRule,
    fingerprints: &'a [&'a FileFingerprint],
) -> Vec<&'a FileFingerprint> {
    fingerprints
        .iter()
        .copied()
        .filter(|fp| language_matches(rule, fp))
        .filter(|fp| extension_matches(rule, fp))
        .filter(|fp| {
            !rule
                .exclude_path_contains
                .iter()
                .any(|needle| fp.relative_path.contains(needle))
        })
        .collect()
}

fn extension_matches(rule: &RequestedDetectorRule, fp: &FileFingerprint) -> bool {
    if rule.file_extensions.is_empty() {
        return true;
    }
    let extension = std::path::Path::new(&fp.relative_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    rule.file_extensions
        .iter()
        .any(|expected| expected.trim_start_matches('.') == extension)
}

fn language_matches(rule: &RequestedDetectorRule, fp: &FileFingerprint) -> bool {
    let Some(language) = &rule.language else {
        return true;
    };
    match language.trim().to_ascii_lowercase().as_str() {
        "php" => fp.language == Language::Php,
        "rust" => fp.language == Language::Rust,
        "javascript" | "js" => fp.language == Language::JavaScript,
        "typescript" | "ts" => fp.language == Language::TypeScript,
        "unknown" => fp.language == Language::Unknown,
        _ => false,
    }
}

fn finding_from_captures(
    rule: &RequestedDetectorRule,
    fp: &FileFingerprint,
    captures: &Captures,
    description: &str,
    suggestion: &str,
) -> Finding {
    finding_from_captures_with_extra(rule, fp, captures, description, suggestion, |_| {
        String::new()
    })
}

fn finding_from_captures_with_extra<F>(
    rule: &RequestedDetectorRule,
    fp: &FileFingerprint,
    captures: &Captures,
    description: &str,
    suggestion: &str,
    extra: F,
) -> Finding
where
    F: Fn(&str) -> String,
{
    let offset = captures.get(0).map(|m| m.start()).unwrap_or(0);
    let line = line_of_offset(&fp.content, offset).to_string();
    Finding {
        convention: rule.convention.clone(),
        severity: severity_from_config(&rule.severity),
        file: fp.relative_path.clone(),
        description: render_template(description, Some(captures), |name| match name {
            "line" => line.clone(),
            other => extra(other),
        }),
        suggestion: render_template(suggestion, Some(captures), extra),
        kind: AuditFinding::from_str(&rule.kind).unwrap_or(AuditFinding::LegacyComment),
    }
}

fn finding_from_derived_literal_site(
    rule: &RequestedDetectorRule,
    site: &DerivedLiteralSite,
    description: &str,
    suggestion: &str,
) -> Finding {
    let label = site.labels.join(", ");
    let extra = |name: &str| match name {
        "line" => site.line.to_string(),
        "value" => site.value.clone(),
        "label" => label.clone(),
        _ => String::new(),
    };
    Finding {
        convention: rule.convention.clone(),
        severity: severity_from_config(&rule.severity),
        file: site.file.clone(),
        description: render_template_from_values(description, &site.captures, extra),
        suggestion: render_template_from_values(suggestion, &site.captures, extra),
        kind: AuditFinding::from_str(&rule.kind).unwrap_or(AuditFinding::LegacyComment),
    }
}

fn capture_values(regex: &Regex, captures: &Captures) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for (index, capture) in captures.iter().enumerate() {
        if let Some(capture) = capture {
            values.insert(index.to_string(), capture.as_str().to_string());
        }
    }
    for name in regex.capture_names().flatten() {
        if let Some(capture) = captures.name(name) {
            values.insert(name.to_string(), capture.as_str().to_string());
        }
    }
    values
}

fn render_template_from_values<F>(
    template: &str,
    values: &HashMap<String, String>,
    extra: F,
) -> String
where
    F: Fn(&str) -> String,
{
    let token =
        Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*|[0-9]+)\}").expect("template regex compiles");
    token
        .replace_all(template, |caps: &Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            values
                .get(name)
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| extra(name))
        })
        .to_string()
}

fn severity_from_config(value: &str) -> Severity {
    match value.trim().to_ascii_lowercase().as_str() {
        "info" => Severity::Info,
        _ => Severity::Warning,
    }
}

fn capture_value(captures: &Captures, name: &str) -> String {
    if let Ok(index) = name.parse::<usize>() {
        return captures
            .get(index)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
    }
    captures
        .name(name)
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

fn render_template<F>(template: &str, captures: Option<&Captures>, extra: F) -> String
where
    F: Fn(&str) -> String,
{
    let token =
        Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*|[0-9]+)\}").expect("template regex compiles");
    token
        .replace_all(template, |caps: &Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
            captures
                .map(|c| capture_value(c, name))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| extra(name))
        })
        .to_string()
}

fn line_at_offset(content: &str, offset: usize) -> &str {
    let offset = offset.min(content.len());
    let start = content[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = content[offset..]
        .find('\n')
        .map_or(content.len(), |index| offset + index);
    &content[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn php_fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            language: Language::Php,
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn config(rule: RequestedDetectorRule) -> AuditConfig {
        AuditConfig {
            requested_detectors: vec![rule],
            ..Default::default()
        }
    }

    #[test]
    fn test_run() {
        let fp = php_fp("src/storage.php", "<?php noop();");

        assert!(run(&[&fp], &AuditConfig::default()).is_empty());
    }

    #[test]
    fn regex_rules_render_capture_templates() {
        let fp = php_fp(
            "src/storage.php",
            r#"<?php
query( "SELECT * FROM table WHERE data LIKE '%\"status\":\"processing\"%'" );
"#,
        );
        let rule = RequestedDetectorRule {
            id: "json-like".to_string(),
            kind: "json_like_exact_match".to_string(),
            severity: "warning".to_string(),
            convention: "requested_detectors".to_string(),
            language: Some("php".to_string()),
            file_extensions: vec!["php".to_string()],
            exclude_path_contains: vec!["/vendor/".to_string(), "vendor/".to_string()],
            rule: RequestedDetectorRuleBody::Regex {
                pattern: r#"(?is)\b(?P<column>data)\b\s+LIKE\s+[^;\n]*(?:\\?['\"]|%)(?:\\?\")(?P<field>[A-Za-z_][A-Za-z0-9_\-]*)(?:\\?\")\s*:"#.to_string(),
                description: "SQL LIKE exact-match against JSON blob `{column}` at line {line} for key `{field}`".to_string(),
                suggestion: "Promote `{field}` out of `{column}` before exact matching.".to_string(),
            },
        };

        let findings = run(&[&fp], &config(rule));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::JsonLikeExactMatch);
        assert!(findings[0].description.contains("data"));
        assert!(findings[0].description.contains("status"));
    }

    #[test]
    fn derived_literal_rules_collect_values_and_skip_source_file() {
        let constants = php_fp(
            "src/categories.php",
            r#"<?php
final class Categories {
    public const CONTENT = 'content-item';
    public function value() { return 'content-item'; }
}
"#,
        );
        let caller = php_fp(
            "src/caller.php",
            r#"<?php
register_item( array( 'category' => 'content-item' ) );
"#,
        );
        let rule = RequestedDetectorRule {
            id: "slug-constant".to_string(),
            kind: "constant_backed_slug_literal".to_string(),
            severity: "info".to_string(),
            convention: "requested_detectors".to_string(),
            language: Some("php".to_string()),
            file_extensions: vec!["php".to_string()],
            exclude_path_contains: vec![],
            rule: RequestedDetectorRuleBody::DerivedLiteral {
                source_pattern: r#"(?s)\b(?:final\s+|abstract\s+)?class\s+(?P<class>[A-Za-z_][A-Za-z0-9_]*)\b.*?\b(?:(?:public|protected|private)\s+)?const\s+(?P<const>[A-Z][A-Z0-9_]*)\s*=\s*['\"](?P<value>[a-z][a-z0-9]*(?:[-_/:][a-z0-9]+)+)['\"]"#.to_string(),
                value_capture: "value".to_string(),
                label: "{class}::{const}".to_string(),
                literal_pattern: r#"['\"]{value}['\"]"#.to_string(),
                exclude_match_context_patterns: vec![],
                description: "Raw slug literal `{value}` at line {line} duplicates constant {label}".to_string(),
                suggestion: "Use {label} instead.".to_string(),
            },
        };

        let findings = run(&[&constants, &caller], &config(rule));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::ConstantBackedSlugLiteral);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].description.contains("Categories::CONTENT"));
    }

    #[test]
    fn derived_literal_rules_drop_excluded_match_contexts() {
        let constants = php_fp(
            "src/categories.php",
            r#"<?php
final class Categories {
    public const CONTENT = 'content-item';
}
"#,
        );
        let caller = php_fp(
            "src/caller.php",
            r#"<?php
$items = array( 'content-item' => true );
if ( $slug === 'content-item' ) { return true; }
"#,
        );
        let rule = RequestedDetectorRule {
            id: "slug-constant".to_string(),
            kind: "constant_backed_slug_literal".to_string(),
            severity: "info".to_string(),
            convention: "requested_detectors".to_string(),
            language: Some("php".to_string()),
            file_extensions: vec!["php".to_string()],
            exclude_path_contains: vec![],
            rule: RequestedDetectorRuleBody::DerivedLiteral {
                source_pattern: r#"(?s)\b(?:final\s+|abstract\s+)?class\s+(?P<class>[A-Za-z_][A-Za-z0-9_]*)\b.*?\b(?:(?:public|protected|private)\s+)?const\s+(?P<const>[A-Z][A-Z0-9_]*)\s*=\s*['\"](?P<value>[a-z][a-z0-9]*(?:[-_/:][a-z0-9]+)+)['\"]"#.to_string(),
                value_capture: "value".to_string(),
                label: "{class}::{const}".to_string(),
                literal_pattern: r#"['\"]{value}['\"]"#.to_string(),
                exclude_match_context_patterns: vec![r#"['\"]{value}['\"]\s*=>"#.to_string()],
                description: "Raw slug literal `{value}` at line {line} duplicates constant {label}".to_string(),
                suggestion: "Use {label} instead.".to_string(),
            },
        };

        let findings = run(&[&constants, &caller], &config(rule));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("line 3"));
    }

    #[test]
    fn derived_literal_rules_dedupe_same_site_and_value_with_multiple_labels() {
        let constants = php_fp(
            "src/categories.php",
            r#"<?php
final class Categories {
    public const CONTENT = 'content-item';
}
final class CategoryAliases {
    public const CONTENT = 'content-item';
}
"#,
        );
        let caller = php_fp(
            "src/caller.php",
            r#"<?php
register_item( array( 'category' => 'content-item' ) );
"#,
        );
        let rule = RequestedDetectorRule {
            id: "slug-constant".to_string(),
            kind: "constant_backed_slug_literal".to_string(),
            severity: "info".to_string(),
            convention: "requested_detectors".to_string(),
            language: Some("php".to_string()),
            file_extensions: vec!["php".to_string()],
            exclude_path_contains: vec![],
            rule: RequestedDetectorRuleBody::DerivedLiteral {
                source_pattern: r#"(?s)\b(?:final\s+|abstract\s+)?class\s+(?P<class>[A-Za-z_][A-Za-z0-9_]*)\b.*?\b(?:(?:public|protected|private)\s+)?const\s+(?P<const>[A-Z][A-Z0-9_]*)\s*=\s*['\"](?P<value>[a-z][a-z0-9]*(?:[-_/:][a-z0-9]+)+)['\"]"#.to_string(),
                value_capture: "value".to_string(),
                label: "{class}::{const}".to_string(),
                literal_pattern: r#"['\"]{value}['\"]"#.to_string(),
                exclude_match_context_patterns: vec![],
                description: "Raw slug literal `{value}` at line {line} duplicates constant(s) {label}".to_string(),
                suggestion: "Use one of: {label}.".to_string(),
            },
        };

        let findings = run(&[&constants, &caller], &config(rule));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/caller.php");
        assert!(findings[0].description.contains("Categories::CONTENT"));
        assert!(findings[0].description.contains("CategoryAliases::CONTENT"));
        assert!(findings[0].suggestion.contains("Categories::CONTENT"));
        assert!(findings[0].suggestion.contains("CategoryAliases::CONTENT"));
    }

    #[test]
    fn comment_regex_rules_require_matching_comment_block() {
        let matching = php_fp(
            "src/shared_storage.php",
            r#"<?php
/** Tokens are stored in shared storage across tenants. */
write_local_option( 'external_tokens', array() );
"#,
        );
        let explicit_single_site = php_fp(
            "src/local_storage.php",
            r#"<?php
/** This value is intentionally local-only storage. */
write_local_option( 'local_setting', true );
"#,
        );
        let rule = RequestedDetectorRule {
            id: "option-scope".to_string(),
            kind: "option_scope_drift".to_string(),
            severity: "warning".to_string(),
            convention: "requested_detectors".to_string(),
            language: Some("php".to_string()),
            file_extensions: vec!["php".to_string()],
            exclude_path_contains: vec![],
            rule: RequestedDetectorRuleBody::CommentRegex {
                comment_pattern: r#"(?i)\b(shared storage|shared across tenants|shared across sites)\b"#.to_string(),
                comment_exclude_pattern: Some(r#"(?i)\b(local-only storage|single-tenant storage)\b"#.to_string()),
                pattern: r#"\b(?P<call>read_local_option|write_local_option|delete_local_option)\s*\("#.to_string(),
                description: "Storage scope drift at line {line}: docs mention shared storage but implementation calls `{call}`".to_string(),
                suggestion: "Use the matching shared-storage call or update the storage contract.".to_string(),
            },
        };

        let findings = run(&[&matching, &explicit_single_site], &config(rule));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::OptionScopeDrift);
        assert!(findings[0].description.contains("write_local_option"));
    }
}
