use std::collections::BTreeMap;

use serde_json::Value;

use super::super::summary_json::{array_len, string_value, usize_value, value_at};

/// Render generic bench/fuzzer coverage metadata when runners provide it.
///
/// The extractor is intentionally schema-blind. It does not know product
/// domains or artifact schemas; it only looks for generic coverage keys and
/// count fields commonly present in bench result metadata.
pub(crate) fn bench_coverage_lines(output: &Value) -> Vec<String> {
    let Some(summary) = collect_coverage_summary(output) else {
        return Vec::new();
    };

    let mut lines = vec!["Coverage:".to_string()];
    let mut counts = Vec::new();
    if let Some(value) = summary.surface_count {
        counts.push(format!("discovered={value}"));
    }
    if let Some(value) = summary.exercised_count {
        counts.push(format!("exercised={value}"));
    }
    if let Some(value) = summary.skipped_count {
        counts.push(format!("skipped_unsafe={value}"));
    }
    if let Some(value) = summary.failed_count {
        counts.push(format!("failed={value}"));
    }
    if !counts.is_empty() {
        lines.push(format!("  Surfaces: {}", counts.join(" ")));
    }
    if let Some(value) = summary.coverage_gap_count {
        lines.push(format!("  Coverage gaps: {value}"));
    }
    if !summary.top_uncovered_groups.is_empty() {
        lines.push("  Top uncovered groups:".to_string());
        for group in summary.top_uncovered_groups {
            match group.count {
                Some(count) => lines.push(format!("    {}: {count}", group.name)),
                None => lines.push(format!("    {}", group.name)),
            }
        }
    }

    if lines.len() == 1 {
        return Vec::new();
    }
    lines
}

#[derive(Debug, Default)]
struct CoverageSummaryArgs {
    surface_count: Option<usize>,
    exercised_count: Option<usize>,
    skipped_count: Option<usize>,
    failed_count: Option<usize>,
    coverage_gap_count: Option<usize>,
    top_uncovered_groups: Vec<UncoveredGroup>,
}

#[derive(Debug)]
struct UncoveredGroup {
    name: String,
    count: Option<usize>,
}

fn collect_coverage_summary(output: &Value) -> Option<CoverageSummaryArgs> {
    let mut summary = CoverageSummaryArgs::default();
    let candidates = coverage_candidates(output);
    if candidates.is_empty() {
        return None;
    }

    for candidate in &candidates {
        if let Some(value) = usize_value(candidate, &["surface_count"]) {
            summary.surface_count.get_or_insert(value);
        }
        if let Some(value) = usize_value(candidate, &["exercised_count"]) {
            summary.exercised_count.get_or_insert(value);
        }
        if let Some(value) = usize_value(candidate, &["skipped_count"]) {
            summary.skipped_count.get_or_insert(value);
        }
        if let Some(value) = usize_value(candidate, &["failed_count"]) {
            summary.failed_count.get_or_insert(value);
        }
        if summary.coverage_gap_count.is_none() {
            summary.coverage_gap_count = array_len(candidate, &["coverage_gaps"])
                .or_else(|| usize_value(candidate, &["coverage_gap_count"]));
        }
        if summary.top_uncovered_groups.is_empty() {
            summary.top_uncovered_groups = top_uncovered_groups(candidate);
        }
    }

    if summary.surface_count.is_none()
        && summary.exercised_count.is_none()
        && summary.skipped_count.is_none()
        && summary.failed_count.is_none()
        && summary.coverage_gap_count.is_none()
        && summary.top_uncovered_groups.is_empty()
    {
        return None;
    }
    Some(summary)
}

fn coverage_candidates(output: &Value) -> Vec<&Value> {
    let mut candidates = Vec::new();
    for path in [
        vec!["coverage_summary"],
        vec!["results", "coverage_summary"],
        vec!["run_metadata", "coverage_summary"],
        vec!["results", "run_metadata", "coverage_summary"],
        vec!["metadata", "coverage_summary"],
    ] {
        if let Some(value) = value_at(output, &path) {
            candidates.push(value);
        }
    }
    for path in [
        Vec::<&str>::new(),
        vec!["results"],
        vec!["run_metadata"],
        vec!["results", "run_metadata"],
        vec!["metadata"],
    ] {
        if let Some(value) = value_at(output, &path) {
            candidates.push(value);
        } else if path.is_empty() {
            candidates.push(output);
        }
    }
    for path in [
        vec!["artifacts"],
        vec!["results", "artifacts"],
        vec!["metadata", "artifacts"],
    ] {
        for artifact in value_at(output, &path)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(value) = value_at(artifact, &["coverage_summary"]) {
                candidates.push(value);
            }
            candidates.push(artifact);
        }
    }
    candidates
}

fn top_uncovered_groups(candidate: &Value) -> Vec<UncoveredGroup> {
    for key in ["top_uncovered_groups", "uncovered_groups"] {
        if let Some(groups) = value_at(candidate, &[key]).and_then(Value::as_array) {
            let groups = groups
                .iter()
                .filter_map(group_from_value)
                .take(5)
                .collect::<Vec<_>>();
            if !groups.is_empty() {
                return groups;
            }
        }
    }

    let Some(gaps) = value_at(candidate, &["coverage_gaps"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for gap in gaps {
        if let Some(group) = gap_group(gap) {
            *counts.entry(group).or_default() += 1;
        }
    }
    let mut groups = counts
        .into_iter()
        .map(|(name, count)| UncoveredGroup {
            name,
            count: Some(count),
        })
        .collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        b.count
            .unwrap_or(0)
            .cmp(&a.count.unwrap_or(0))
            .then_with(|| a.name.cmp(&b.name))
    });
    groups.truncate(5);
    groups
}

fn group_from_value(value: &Value) -> Option<UncoveredGroup> {
    if let Some(name) = value.as_str() {
        return Some(UncoveredGroup {
            name: name.to_string(),
            count: None,
        });
    }
    let name = string_value(value, &["group"])
        .or_else(|| string_value(value, &["name"]))
        .or_else(|| string_value(value, &["id"]))?;
    Some(UncoveredGroup {
        name: name.to_string(),
        count: usize_value(value, &["count"]).or_else(|| usize_value(value, &["uncovered_count"])),
    })
}

fn gap_group(value: &Value) -> Option<String> {
    if let Some(group) = string_value(value, &["group"])
        .or_else(|| string_value(value, &["surface_group"]))
        .or_else(|| string_value(value, &["category"]))
    {
        return Some(group.to_string());
    }
    let text = value.as_str()?;
    for separator in ["::", ":", "/", "."] {
        if let Some((group, _)) = text.split_once(separator) {
            if !group.is_empty() {
                return Some(group.to_string());
            }
        }
    }
    Some(text.to_string())
}
