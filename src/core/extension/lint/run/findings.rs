//! Finding filtering, producer-summary assembly, and summary construction.

use super::types::{LintRunWorkflowArgs, LintSummaryOutput, ScopedLintRun};
use crate::core::finding::{FindingProducerSummary, FindingSource, HomeboyFinding};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn filter_lint_findings(
    findings: Vec<HomeboyFinding>,
    args: &LintRunWorkflowArgs,
) -> Vec<HomeboyFinding> {
    let included_sniffs = parse_csv_filter(args.sniff_filters.sniffs.as_deref());
    let excluded_sniffs = parse_csv_filter(args.sniff_filters.exclude_sniffs.as_deref());
    let category = args
        .category
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    findings
        .into_iter()
        .filter(|finding| {
            category.is_none_or(|expected| finding.category.as_deref() == Some(expected))
                && (included_sniffs.is_empty()
                    || included_sniffs
                        .iter()
                        .any(|expected| finding_matches_sniff(finding, expected)))
                && !excluded_sniffs
                    .iter()
                    .any(|excluded| finding_matches_sniff(finding, excluded))
        })
        .collect()
}

pub(super) fn filter_findings_to_scoped_files(
    findings: Vec<HomeboyFinding>,
    scoped_runs: Option<&[ScopedLintRun]>,
) -> Vec<HomeboyFinding> {
    let Some(scoped_runs) = scoped_runs else {
        return findings;
    };
    let scoped_files: std::collections::BTreeSet<&str> = scoped_runs
        .iter()
        .flat_map(|run| run.changed_files.iter().map(String::as_str))
        .collect();
    if scoped_files.is_empty() {
        return findings;
    }
    findings
        .into_iter()
        .filter(|finding| {
            finding
                .location
                .file
                .as_deref()
                .is_some_and(|file| scoped_files.contains(file))
        })
        .collect()
}

fn parse_csv_filter(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn finding_matches_sniff(finding: &HomeboyFinding, sniff: &str) -> bool {
    finding.category.as_deref() == Some(sniff)
        || finding.rule.as_deref().is_some_and(|rule| rule == sniff)
        || finding.fingerprint.as_deref() == Some(sniff)
        || finding
            .fingerprint
            .as_deref()
            .is_some_and(|id| id.split("::").any(|part| part == sniff) || id.ends_with(sniff))
}

pub(super) fn build_lint_producer_summaries(
    findings: &[HomeboyFinding],
    findings_source_path: &Path,
    producers_source_path: &Path,
    declared_producers: Vec<FindingProducerSummary>,
    runner_success: bool,
    runner_exit_code: i32,
    step: Option<&str>,
) -> Vec<FindingProducerSummary> {
    let mut counts = BTreeMap::<String, usize>::new();
    for finding in findings {
        *counts.entry(finding.tool.clone()).or_insert(0) += 1;
    }

    if !declared_producers.is_empty() {
        return declared_producers
            .into_iter()
            .map(|mut summary| {
                summary.finding_count = counts
                    .get(&summary.tool)
                    .copied()
                    .unwrap_or(summary.finding_count);
                if summary.source.is_none() {
                    summary.source = Some(
                        FindingSource::new("sidecar")
                            .label("lint-producers")
                            .path(producers_source_path.display().to_string()),
                    );
                }
                if summary.step.is_none() {
                    summary.step = step.map(str::to_string);
                }
                summary
                    .metadata
                    .entry("source_sidecar".to_string())
                    .or_insert_with(|| serde_json::json!("lint-producers"));
                summary
                    .metadata
                    .entry("source_sidecar_path".to_string())
                    .or_insert_with(|| {
                        serde_json::json!(producers_source_path.display().to_string())
                    });
                summary
                    .metadata
                    .entry("exit_code".to_string())
                    .or_insert_with(|| serde_json::json!(runner_exit_code));
                summary
            })
            .collect();
    }

    if counts.is_empty() {
        counts.insert("lint".to_string(), 0);
    }

    counts
        .into_iter()
        .map(|(tool, finding_count)| {
            let status = if runner_exit_code >= 2 || (!runner_success && finding_count == 0) {
                "error"
            } else if finding_count > 0 {
                "failed"
            } else {
                "passed"
            };
            let mut summary = FindingProducerSummary::new(tool, status)
                .finding_count(finding_count)
                .source(
                    FindingSource::new("sidecar")
                        .label("lint-findings")
                        .path(findings_source_path.display().to_string()),
                )
                .metadata("source_sidecar", "lint-findings")
                .metadata(
                    "source_sidecar_path",
                    findings_source_path.display().to_string(),
                )
                .metadata("exit_code", runner_exit_code);
            if let Some(step) = step {
                summary = summary.step(step.to_string());
            }
            summary
        })
        .collect()
}

pub(super) fn parse_lint_producer_summaries_file(
    path: &Path,
) -> crate::core::Result<Vec<FindingProducerSummary>> {
    fn parse_error(path: &Path, error: std::io::Error) -> crate::core::Error {
        crate::core::Error::internal_io(
            format!(
                "Failed to read lint producer summaries file {}: {}",
                path.display(),
                error
            ),
            Some("lint.producers.parse".to_string()),
        )
    }

    fn json_error(path: &Path, error: serde_json::Error) -> crate::core::Error {
        crate::core::Error::internal_io(
            format!(
                "Malformed lint producer summaries JSON in {}: {}",
                path.display(),
                error
            ),
            Some("lint.producers.parse".to_string()),
        )
    }

    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| parse_error(path, e))?;

    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(&content).map_err(|e| json_error(path, e))
}

pub(super) fn mark_zero_finding_producers_passed(
    producer_summaries: &mut [FindingProducerSummary],
) {
    for summary in producer_summaries {
        if summary.finding_count == 0 && (summary.status == "failed" || summary.status == "error") {
            summary.status = "passed".to_string();
        }
    }
}

pub(super) fn build_lint_summary(
    findings: &[HomeboyFinding],
    producer_summaries: &[FindingProducerSummary],
    exit_code: i32,
) -> LintSummaryOutput {
    let mut categories = BTreeMap::new();
    for finding in findings {
        let category = finding
            .category
            .clone()
            .unwrap_or_else(|| "uncategorized".to_string());
        *categories.entry(category).or_insert(0) += 1;
    }

    LintSummaryOutput {
        total_findings: findings.len(),
        categories,
        top_findings: findings.iter().take(20).cloned().collect(),
        producer_summaries: producer_summaries.to_vec(),
        exit_code,
    }
}
