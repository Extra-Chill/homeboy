use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use crate::core::component::RequestedDetectorRule;

use super::super::conventions::AuditFinding;
use super::super::findings::Finding;
use super::super::fingerprint::FileFingerprint;
use super::super::source_locations::line_of_offset;
use super::{capture_value, eligible_files, render_template, severity_from_config};

#[derive(Debug, Clone)]
struct ConfigKeySite {
    file: String,
    line: usize,
}

#[derive(Debug, Default)]
struct ConfigRoundtripKeySets {
    exported: BTreeMap<String, Vec<ConfigKeySite>>,
    imported: BTreeMap<String, Vec<ConfigKeySite>>,
    copied: BTreeMap<String, Vec<ConfigKeySite>>,
    copy_enabled: bool,
    behavior: BTreeMap<String, Vec<ConfigKeySite>>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_config_roundtrip_keys_rule(
    rule: &RequestedDetectorRule,
    fingerprints: &[&FileFingerprint],
    object: &str,
    export_pattern: &str,
    import_pattern: &str,
    copy_patterns: &[String],
    behavior_pattern: &str,
    key_capture: &str,
    exclude_key_patterns: &[String],
    description: &str,
    suggestion: &str,
) -> Vec<Finding> {
    let Ok(export_regex) = Regex::new(export_pattern) else {
        return Vec::new();
    };
    let Ok(import_regex) = Regex::new(import_pattern) else {
        return Vec::new();
    };
    let Ok(behavior_regex) = Regex::new(behavior_pattern) else {
        return Vec::new();
    };
    let copy_regexes = copy_patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect::<Vec<_>>();
    let exclude_regexes = exclude_key_patterns
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect::<Vec<_>>();

    let files = eligible_files(rule, fingerprints);
    let key_sets = ConfigRoundtripKeySets {
        exported: collect_config_key_sites(&files, &export_regex, key_capture, &exclude_regexes),
        imported: collect_config_key_sites(&files, &import_regex, key_capture, &exclude_regexes),
        copied: collect_config_key_sites_from_many(
            &files,
            &copy_regexes,
            key_capture,
            &exclude_regexes,
        ),
        copy_enabled: !copy_regexes.is_empty(),
        behavior: collect_config_key_sites(&files, &behavior_regex, key_capture, &exclude_regexes),
    };

    config_roundtrip_findings(rule, object, &key_sets, description, suggestion)
}

fn collect_config_key_sites(
    files: &[&FileFingerprint],
    regex: &Regex,
    key_capture: &str,
    exclude_regexes: &[Regex],
) -> BTreeMap<String, Vec<ConfigKeySite>> {
    let mut sites: BTreeMap<String, Vec<ConfigKeySite>> = BTreeMap::new();
    for fp in files {
        for captures in regex.captures_iter(&fp.content) {
            let key = capture_value(&captures, key_capture);
            if key.is_empty() || exclude_regexes.iter().any(|regex| regex.is_match(&key)) {
                continue;
            }
            let offset = captures.get(0).map(|m| m.start()).unwrap_or(0);
            sites.entry(key).or_default().push(ConfigKeySite {
                file: fp.relative_path.clone(),
                line: line_of_offset(&fp.content, offset),
            });
        }
    }
    sites
}

fn config_roundtrip_findings(
    rule: &RequestedDetectorRule,
    object: &str,
    key_sets: &ConfigRoundtripKeySets,
    description: &str,
    suggestion: &str,
) -> Vec<Finding> {
    let mut candidate_keys = BTreeSet::new();
    candidate_keys.extend(key_sets.behavior.keys().cloned());
    candidate_keys.extend(key_sets.exported.keys().cloned());
    candidate_keys.extend(key_sets.imported.keys().cloned());
    candidate_keys.extend(key_sets.copied.keys().cloned());

    let mut findings = Vec::new();
    for key in candidate_keys {
        let missing = missing_roundtrip_sides(&key, key_sets);
        if missing.is_empty() {
            continue;
        }

        let Some(site) = representative_config_key_site(&key, key_sets) else {
            continue;
        };
        let missing_text = missing.join(", ");
        let render_value = |name: &str| match name {
            "object" => object.to_string(),
            "key" => key.clone(),
            "missing" => missing_text.clone(),
            "line" => site.line.to_string(),
            "export_count" => key_sets.exported.get(&key).map_or(0, Vec::len).to_string(),
            "import_count" => key_sets.imported.get(&key).map_or(0, Vec::len).to_string(),
            "behavior_count" => key_sets.behavior.get(&key).map_or(0, Vec::len).to_string(),
            "copy_count" => key_sets.copied.get(&key).map_or(0, Vec::len).to_string(),
            _ => String::new(),
        };
        findings.push(Finding {
            convention: rule.convention.clone(),
            severity: severity_from_config(&rule.severity),
            file: site.file.clone(),
            description: render_template(description, None, render_value),
            suggestion: render_template(suggestion, None, render_value),
            kind: AuditFinding::from_str(&rule.kind).unwrap_or(AuditFinding::LegacyComment),
        });
    }

    findings
}

fn missing_roundtrip_sides(key: &str, key_sets: &ConfigRoundtripKeySets) -> Vec<&'static str> {
    let behavior_bearing = key_sets.behavior.contains_key(key);
    let exported = key_sets.exported.contains_key(key);
    let imported = key_sets.imported.contains_key(key);
    let copied = key_sets.copied.contains_key(key);
    let mut missing = Vec::new();

    if behavior_bearing
        || exported != imported
        || (key_sets.copy_enabled && (copied != exported || copied != imported))
    {
        if !exported {
            missing.push("export");
        }
        if !imported {
            missing.push("import");
        }
        if key_sets.copy_enabled && !copied {
            missing.push("copy");
        }
    }

    missing
}

fn representative_config_key_site<'a>(
    key: &str,
    key_sets: &'a ConfigRoundtripKeySets,
) -> Option<&'a ConfigKeySite> {
    key_sets
        .behavior
        .get(key)
        .or_else(|| key_sets.exported.get(key))
        .or_else(|| key_sets.imported.get(key))
        .or_else(|| key_sets.copied.get(key))
        .and_then(|sites| sites.first())
}

fn collect_config_key_sites_from_many(
    files: &[&FileFingerprint],
    regexes: &[Regex],
    key_capture: &str,
    exclude_regexes: &[Regex],
) -> BTreeMap<String, Vec<ConfigKeySite>> {
    let mut all_sites: BTreeMap<String, Vec<ConfigKeySite>> = BTreeMap::new();
    for regex in regexes {
        for (key, sites) in collect_config_key_sites(files, regex, key_capture, exclude_regexes) {
            all_sites.entry(key).or_default().extend(sites);
        }
    }
    all_sites
}
