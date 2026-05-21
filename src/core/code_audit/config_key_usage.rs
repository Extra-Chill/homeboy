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

pub(super) fn run(fingerprints: &[&FileFingerprint], rules: &[ConfigKeyUsageRule]) -> Vec<Finding> {
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

    collect_accessor_symbol_reads(&eligible, &mut evidence_by_key);

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
                if let Some(offset) = fp.content.find(&symbol) {
                    evidence.reads.push(EvidenceSite {
                        file: fp.relative_path.clone(),
                        line: line_of_offset(&fp.content, offset),
                    });
                }
            }
        }
    }
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
        }
    }

    #[test]
    fn flags_written_accessor_backed_key_without_production_read() {
        let files = vec![
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
    fn production_read_satisfies_written_key() {
        let files = vec![
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
        let files = vec![
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
    fn fixture_only_writes_are_excluded() {
        let files = vec![fp(
            "fixtures/builder.rs",
            "set_config('enabled_items', values);",
        )];
        let refs = files.iter().collect::<Vec<_>>();

        assert!(run(&refs, &[rule()]).is_empty());
    }
}
