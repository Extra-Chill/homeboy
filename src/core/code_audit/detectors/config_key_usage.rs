//! Config-key usage correlation for component-owned rule packs.
//!
//! Core is intentionally language/framework agnostic here: configured regexes
//! capture opaque key and symbol strings, then core correlates write/accessor
//! evidence with non-test read evidence.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use crate::core::component::{ConfigKeyUsagePattern, ConfigKeyUsageRule};

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::source_locations::line_of_offset;
use super::walker::is_test_path;

#[derive(Debug, Default)]
struct KeyEvidence {
    writes: Vec<EvidenceSite>,
    accessors: Vec<EvidenceSite>,
    reads: Vec<EvidenceSite>,
    accessor_symbols: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct EvidenceSite {
    file: String,
    line: usize,
}

pub(in crate::core::code_audit) fn run(
    fingerprints: &[&FileFingerprint],
    rules: &[ConfigKeyUsageRule],
) -> Vec<Finding> {
    if rules.is_empty() {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for rule in rules {
        findings.extend(run_rule(fingerprints, rule));
    }
    findings.sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    findings
}

fn run_rule(fingerprints: &[&FileFingerprint], rule: &ConfigKeyUsageRule) -> Vec<Finding> {
    let mut evidence_by_key: BTreeMap<String, KeyEvidence> = BTreeMap::new();
    let eligible = fingerprints
        .iter()
        .copied()
        .filter(|fp| is_eligible_path(&fp.relative_path, rule))
        .collect::<Vec<_>>();

    for fp in &eligible {
        collect_pattern_evidence(fp, &rule.write_patterns, |key, site, _symbol| {
            evidence_by_key.entry(key).or_default().writes.push(site);
        });
        collect_pattern_evidence(fp, &rule.accessor_patterns, |key, site, symbol| {
            let evidence = evidence_by_key.entry(key).or_default();
            evidence.accessors.push(site);
            if let Some(symbol) = symbol {
                evidence.accessor_symbols.insert(symbol);
            }
        });
    }

    for fp in eligible
        .iter()
        .filter(|fp| !is_test_path(&fp.relative_path))
    {
        collect_pattern_evidence(fp, &rule.read_patterns, |key, site, _symbol| {
            evidence_by_key.entry(key).or_default().reads.push(site);
        });
    }

    collect_accessor_symbol_reads(&eligible, rule, &mut evidence_by_key);

    evidence_by_key
        .into_iter()
        .filter(|(_, evidence)| {
            (!evidence.writes.is_empty() || !evidence.accessors.is_empty())
                && evidence.reads.is_empty()
        })
        .filter_map(|(key, evidence)| finding_for(rule, &key, &evidence))
        .collect()
}

fn is_eligible_path(path: &str, rule: &ConfigKeyUsageRule) -> bool {
    !rule
        .exclude_path_contains
        .iter()
        .any(|needle| path.contains(needle))
}

fn collect_pattern_evidence(
    fp: &FileFingerprint,
    patterns: &[ConfigKeyUsagePattern],
    mut record: impl FnMut(String, EvidenceSite, Option<String>),
) {
    for pattern in patterns {
        let Ok(regex) = Regex::new(&pattern.pattern) else {
            continue;
        };
        for captures in regex.captures_iter(&fp.content) {
            let Some(key) = captures.name(&pattern.key_capture) else {
                continue;
            };
            let symbol = pattern
                .symbol_capture
                .as_ref()
                .and_then(|capture| captures.name(capture))
                .map(|value| value.as_str().to_string())
                .filter(|value| !value.trim().is_empty());
            let line = captures
                .get(0)
                .map(|m| line_of_offset(&fp.content, m.start()))
                .unwrap_or(1);
            record(
                key.as_str().to_string(),
                EvidenceSite {
                    file: fp.relative_path.clone(),
                    line,
                },
                symbol,
            );
        }
    }
}

fn collect_accessor_symbol_reads(
    fingerprints: &[&FileFingerprint],
    rule: &ConfigKeyUsageRule,
    evidence_by_key: &mut BTreeMap<String, KeyEvidence>,
) {
    for evidence in evidence_by_key.values_mut() {
        let definition_files = evidence
            .accessors
            .iter()
            .chain(evidence.writes.iter())
            .map(|site| site.file.as_str())
            .collect::<BTreeSet<_>>();
        for symbol in evidence.accessor_symbols.clone() {
            if symbol.len() < 3 {
                continue;
            }
            for fp in fingerprints
                .iter()
                .filter(|fp| !is_test_path(&fp.relative_path))
            {
                if definition_files.contains(fp.relative_path.as_str()) {
                    continue;
                }
                if let Some(offset) = first_accessor_symbol_read_offset(fp, rule, &symbol) {
                    evidence.reads.push(EvidenceSite {
                        file: fp.relative_path.clone(),
                        line: line_of_offset(&fp.content, offset),
                    });
                }
            }
        }
    }
}

fn first_accessor_symbol_read_offset(
    fp: &FileFingerprint,
    rule: &ConfigKeyUsageRule,
    symbol: &str,
) -> Option<usize> {
    if !rule.accessor_symbol_read_patterns.is_empty() {
        let escaped_symbol = regex::escape(symbol);
        for pattern in &rule.accessor_symbol_read_patterns {
            let pattern = pattern.replace("{symbol}", &escaped_symbol);
            let Ok(regex) = Regex::new(&pattern) else {
                continue;
            };
            if let Some(found) = regex.find(&fp.content) {
                return Some(found.start());
            }
        }
        return None;
    }

    first_identifier_reference_offset(&fp.content, symbol)
}

fn first_identifier_reference_offset(content: &str, symbol: &str) -> Option<usize> {
    let mut search_start = 0;
    while let Some(relative_offset) = content[search_start..].find(symbol) {
        let offset = search_start + relative_offset;
        let end = offset + symbol.len();
        if has_identifier_boundaries(content, offset, end) && !is_comment_only_line(content, offset)
        {
            return Some(offset);
        }
        search_start = end;
    }
    None
}

fn has_identifier_boundaries(content: &str, start: usize, end: usize) -> bool {
    !content[..start]
        .chars()
        .next_back()
        .is_some_and(is_identifier_char)
        && !content[end..]
            .chars()
            .next()
            .is_some_and(is_identifier_char)
}

fn is_identifier_char(value: char) -> bool {
    value == '_' || value.is_ascii_alphanumeric()
}

fn is_comment_only_line(content: &str, offset: usize) -> bool {
    let line_start = content[..offset].rfind('\n').map_or(0, |index| index + 1);
    let prefix = content[line_start..offset].trim_start();
    prefix.starts_with("//")
        || prefix.starts_with('#')
        || prefix.starts_with("/*")
        || prefix.starts_with('*')
        || prefix.starts_with("<!--")
}

fn finding_for(rule: &ConfigKeyUsageRule, key: &str, evidence: &KeyEvidence) -> Option<Finding> {
    let primary = evidence
        .writes
        .first()
        .or_else(|| evidence.accessors.first())?;
    let write_count = evidence.writes.len();
    let accessor_count = evidence.accessors.len();
    let first_site = format!("{}:{}", primary.file, primary.line);
    Some(Finding {
        convention: format!("config_key_usage:{}", rule.id),
        severity: Severity::Warning,
        file: primary.file.clone(),
        description: format!(
            "Config key '{}' has {} write/migration site(s) and {} accessor site(s), but no non-test read matched this rule; first evidence at {}",
            key, write_count, accessor_count, first_site
        ),
        suggestion: "Either consume the key in production code or remove the stale write/accessor path".to_string(),
        kind: AuditFinding::WriteOnlyConfigKey,
    })
}

#[cfg(test)]
mod tests {
    use crate::core::component::ConfigKeyUsagePattern;

    use super::*;

    fn fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    fn rule() -> ConfigKeyUsageRule {
        ConfigKeyUsageRule {
            id: "generic-config".to_string(),
            exclude_path_contains: vec!["fixtures/".to_string()],
            write_patterns: vec![ConfigKeyUsagePattern {
                pattern: r#"set_config\(\s*['\"](?P<key>[a-z_]+)['\"]"#.to_string(),
                key_capture: "key".to_string(),
                symbol_capture: None,
            }],
            accessor_patterns: vec![ConfigKeyUsagePattern {
                pattern:
                    r#"fn\s+(?P<symbol>[a-z_]+)\(\).*get_config\(\s*['\"](?P<key>[a-z_]+)['\"]"#
                        .to_string(),
                key_capture: "key".to_string(),
                symbol_capture: Some("symbol".to_string()),
            }],
            read_patterns: vec![ConfigKeyUsagePattern {
                pattern: r#"read_config\(\s*['\"](?P<key>[a-z_]+)['\"]"#.to_string(),
                key_capture: "key".to_string(),
                symbol_capture: None,
            }],
            accessor_symbol_read_patterns: vec![],
        }
    }

    fn rule_with_symbol_read_pattern(pattern: &str) -> ConfigKeyUsageRule {
        ConfigKeyUsageRule {
            accessor_symbol_read_patterns: vec![pattern.to_string()],
            ..rule()
        }
    }

    #[test]
    fn test_run() {
        let files = [
            fp("src/builder.rs", "set_config('enabled_items', values);"),
            fp(
                "src/config.rs",
                "fn enabled_items() { get_config('enabled_items') }",
            ),
            fp("tests/config_test.rs", "read_config('enabled_items');"),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        let findings = run(&refs, &[rule()]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::WriteOnlyConfigKey);
        assert!(findings[0].description.contains("enabled_items"));
    }

    #[test]
    fn test_run_rule() {
        let files = [fp("src/builder.rs", "set_config('enabled_items', values);")];
        let refs = files.iter().collect::<Vec<_>>();

        let findings = run_rule(&refs, &rule());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "src/builder.rs");
    }

    #[test]
    fn test_is_eligible_path() {
        let rule = rule();

        assert!(is_eligible_path("src/builder.rs", &rule));
        assert!(!is_eligible_path("fixtures/builder.rs", &rule));
    }

    #[test]
    fn test_collect_pattern_evidence() {
        let file = fp(
            "src/builder.rs",
            "first\nset_config('enabled_items', values);",
        );
        let mut records = Vec::new();

        collect_pattern_evidence(&file, &rule().write_patterns, |key, site, symbol| {
            records.push((key, site.file, site.line, symbol));
        });

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "enabled_items");
        assert_eq!(records[0].1, "src/builder.rs");
        assert_eq!(records[0].2, 2);
        assert_eq!(records[0].3, None);
    }

    #[test]
    fn test_collect_accessor_symbol_reads() {
        let files = [
            fp(
                "src/config.rs",
                "fn enabled_items() { get_config('enabled_items') }",
            ),
            fp("src/runtime.rs", "let items = enabled_items();"),
        ];
        let refs = files.iter().collect::<Vec<_>>();
        let mut evidence_by_key = BTreeMap::new();
        evidence_by_key.insert(
            "enabled_items".to_string(),
            KeyEvidence {
                accessors: vec![EvidenceSite {
                    file: "src/config.rs".to_string(),
                    line: 1,
                }],
                accessor_symbols: BTreeSet::from(["enabled_items".to_string()]),
                ..Default::default()
            },
        );

        collect_accessor_symbol_reads(&refs, &rule(), &mut evidence_by_key);

        let evidence = evidence_by_key.get("enabled_items").unwrap();
        assert_eq!(evidence.reads.len(), 1);
        assert_eq!(evidence.reads[0].file, "src/runtime.rs");
    }

    #[test]
    fn test_first_accessor_symbol_read_offset() {
        let rule = rule();

        assert_eq!(
            first_accessor_symbol_read_offset(
                &fp("src/runtime.rs", "let items = enabled_items();"),
                &rule,
                "enabled_items",
            ),
            Some(12)
        );
        assert_eq!(
            first_accessor_symbol_read_offset(
                &fp("src/runtime.rs", "let items = disabled_enabled_items_flag;"),
                &rule,
                "enabled_items",
            ),
            None
        );
        assert_eq!(
            first_accessor_symbol_read_offset(
                &fp("src/runtime.rs", "// enabled_items is mentioned in docs"),
                &rule,
                "enabled_items",
            ),
            None
        );
    }

    #[test]
    fn test_first_identifier_reference_offset() {
        assert_eq!(
            first_identifier_reference_offset("prefix_enabled_items", "enabled_items"),
            None
        );
        assert_eq!(
            first_identifier_reference_offset("enabled_items_suffix", "enabled_items"),
            None
        );
        assert_eq!(
            first_identifier_reference_offset("call enabled_items now", "enabled_items"),
            Some(5)
        );
    }

    #[test]
    fn test_has_identifier_boundaries() {
        assert!(has_identifier_boundaries("call enabled_items();", 5, 18));
        assert!(!has_identifier_boundaries(
            "call enabled_items_extra();",
            5,
            18
        ));
        assert!(!has_identifier_boundaries(
            "call get_enabled_items();",
            9,
            22
        ));
    }

    #[test]
    fn test_is_identifier_char() {
        assert!(is_identifier_char('a'));
        assert!(is_identifier_char('7'));
        assert!(is_identifier_char('_'));
        assert!(!is_identifier_char('-'));
    }

    #[test]
    fn test_is_comment_only_line() {
        assert!(is_comment_only_line("// enabled_items", 3));
        assert!(is_comment_only_line("   # enabled_items", 5));
        assert!(!is_comment_only_line("let items = enabled_items();", 12));
    }

    #[test]
    fn test_finding_for() {
        let evidence = KeyEvidence {
            writes: vec![EvidenceSite {
                file: "src/builder.rs".to_string(),
                line: 3,
            }],
            ..Default::default()
        };

        let finding = finding_for(&rule(), "enabled_items", &evidence).unwrap();

        assert_eq!(finding.kind, AuditFinding::WriteOnlyConfigKey);
        assert_eq!(finding.severity, Severity::Warning);
        assert!(finding.description.contains("enabled_items"));
        assert!(finding.description.contains("src/builder.rs:3"));
    }

    #[test]
    fn production_read_satisfies_written_key() {
        let files = [
            fp("src/builder.rs", "set_config('enabled_items', values);"),
            fp(
                "src/runtime.rs",
                "let items = read_config('enabled_items');",
            ),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &[rule()]).is_empty());
    }

    #[test]
    fn production_accessor_symbol_call_satisfies_key() {
        let files = [
            fp(
                "src/config.rs",
                "fn enabled_items() { get_config('enabled_items') }",
            ),
            fp("src/runtime.rs", "let items = enabled_items();"),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &[rule()]).is_empty());
    }

    #[test]
    fn accessor_symbol_comments_do_not_satisfy_key() {
        let files = [
            fp(
                "src/config.rs",
                "fn enabled_items() { get_config('enabled_items') }",
            ),
            fp(
                "src/runtime.rs",
                "// enabled_items() documents the accessor",
            ),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert_eq!(run(&refs, &[rule()]).len(), 1);
    }

    #[test]
    fn accessor_symbol_partial_identifier_does_not_satisfy_key() {
        let files = [
            fp(
                "src/config.rs",
                "fn enabled_items() { get_config('enabled_items') }",
            ),
            fp("src/runtime.rs", "let value = enabled_items_cached();"),
        ];
        let refs = files.iter().collect::<Vec<_>>();

        assert_eq!(run(&refs, &[rule()]).len(), 1);
    }

    #[test]
    fn configured_accessor_symbol_read_pattern_satisfies_key() {
        let files = [
            fp(
                "src/config.rs",
                "fn enabled_items() { get_config('enabled_items') }",
            ),
            fp("src/runtime.rs", "call_accessor(enabled_items);"),
        ];
        let refs = files.iter().collect::<Vec<_>>();
        let rule = rule_with_symbol_read_pattern(r#"call_accessor\(\s*{symbol}\s*\)"#);

        assert!(run(&refs, &[rule]).is_empty());
    }

    #[test]
    fn fixture_only_writes_are_excluded() {
        let files = [fp(
            "fixtures/builder.rs",
            "set_config('enabled_items', values);",
        )];
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &[rule()]).is_empty());
    }
}
