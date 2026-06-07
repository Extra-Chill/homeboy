use clap::Args;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::commands::escape_markdown_table_cell;

#[derive(Args, Debug, Clone)]
pub struct BrowserEvidenceCompareArgs {
    /// Directory containing baseline browser evidence JSON artifacts
    #[arg(long, value_name = "DIR")]
    pub baseline_dir: String,

    /// Directory containing candidate browser evidence JSON artifacts
    #[arg(long, value_name = "DIR")]
    pub candidate_dir: String,

    /// Label for the baseline artifact set
    #[arg(long, default_value = "baseline")]
    pub baseline_label: String,

    /// Label for the candidate artifact set
    #[arg(long, default_value = "candidate")]
    pub candidate_label: String,

    /// Include local filesystem paths in Markdown output. By default Markdown only uses relative artifact names and URLs.
    #[arg(long)]
    pub include_local_paths: bool,

    /// Output format. Markdown is direct-rendered; JSON uses the normal command envelope.
    #[arg(long, value_parser = ["markdown", "json"], default_value = "markdown")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEvidenceCompareReport {
    pub command: String,
    pub markdown: String,
    pub baseline_label: String,
    pub candidate_label: String,
    pub totals: BrowserEvidenceCompareTotals,
    pub variants: Vec<BrowserEvidenceVariantComparison>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEvidenceCompareTotals {
    pub baseline_samples: usize,
    pub candidate_samples: usize,
    pub variant_count: usize,
    pub variants_with_baseline: usize,
    pub variants_with_candidate: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserEvidenceVariantComparison {
    pub variant: BrowserEvidenceVariant,
    pub baseline_repeats: usize,
    pub candidate_repeats: usize,
    pub assertions: AssertionComparison,
    pub request_totals: MetricComparison,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub request_by_host: BTreeMap<String, MetricComparison>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub request_by_type: BTreeMap<String, MetricComparison>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub browser_metrics: BTreeMap<String, MetricComparison>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lifecycle_metrics: BTreeMap<String, MetricComparison>,
    pub console_errors: MetricComparison,
    pub page_errors: MetricComparison,
    pub artifacts: ArtifactComparison,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrowserEvidenceVariant {
    pub scenario: String,
    pub profile: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub matrix: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AssertionComparison {
    pub baseline: AssertionStats,
    pub candidate: AssertionStats,
    pub pass_delta: i64,
    pub fail_delta: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct AssertionStats {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricComparison {
    pub baseline: Option<MetricStats>,
    pub candidate: Option<MetricStats>,
    pub median_delta: Option<f64>,
    pub median_delta_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricStats {
    pub n: usize,
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ArtifactComparison {
    pub baseline: Vec<ArtifactRef>,
    pub candidate: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArtifactRef {
    pub label: String,
    pub target: String,
}

pub fn render_browser_evidence_compare_from_args(
    args: &BrowserEvidenceCompareArgs,
) -> homeboy::core::Result<String> {
    browser_evidence_compare_from_args(args).map(|report| report.markdown)
}

pub fn browser_evidence_compare_from_args(
    args: &BrowserEvidenceCompareArgs,
) -> homeboy::core::Result<BrowserEvidenceCompareReport> {
    let baseline_dir = PathBuf::from(&args.baseline_dir);
    let candidate_dir = PathBuf::from(&args.candidate_dir);
    browser_evidence_compare_from_dirs(
        &[baseline_dir],
        &[candidate_dir],
        &args.baseline_label,
        &args.candidate_label,
        args.include_local_paths,
    )
}

pub fn browser_evidence_compare_from_dirs(
    baseline_dirs: &[PathBuf],
    candidate_dirs: &[PathBuf],
    baseline_label: &str,
    candidate_label: &str,
    include_local_paths: bool,
) -> homeboy::core::Result<BrowserEvidenceCompareReport> {
    let baseline = implementation::read_evidence_dirs(baseline_dirs, include_local_paths)?;
    let candidate = implementation::read_evidence_dirs(candidate_dirs, include_local_paths)?;
    let mut notes = Vec::new();
    notes.extend(
        baseline
            .notes
            .iter()
            .map(|note| format!("{}: {}", baseline_label, note)),
    );
    notes.extend(
        candidate
            .notes
            .iter()
            .map(|note| format!("{}: {}", candidate_label, note)),
    );

    let variants = implementation::compare_variants(&baseline.samples, &candidate.samples);
    let totals = BrowserEvidenceCompareTotals {
        baseline_samples: baseline.samples.len(),
        candidate_samples: candidate.samples.len(),
        variant_count: variants.len(),
        variants_with_baseline: variants
            .iter()
            .filter(|variant| variant.baseline_repeats > 0)
            .count(),
        variants_with_candidate: variants
            .iter()
            .filter(|variant| variant.candidate_repeats > 0)
            .count(),
    };
    let markdown = implementation::render_markdown(
        baseline_label,
        candidate_label,
        &totals,
        &variants,
        &notes,
    );

    Ok(BrowserEvidenceCompareReport {
        command: "report.browser-evidence-compare".to_string(),
        markdown,
        baseline_label: baseline_label.to_string(),
        candidate_label: candidate_label.to_string(),
        totals,
        variants,
        notes,
    })
}

mod implementation {
    use super::*;

    #[derive(Debug, Clone)]
    pub(super) struct EvidenceSet {
        pub(super) samples: Vec<BrowserEvidenceSample>,
        pub(super) notes: Vec<String>,
    }

    #[derive(Debug, Clone, Default)]
    pub(super) struct BrowserEvidenceSample {
        scenario: Option<String>,
        profile: Option<String>,
        matrix: BTreeMap<String, String>,
        assertions: AssertionStats,
        request_total: Option<f64>,
        request_by_host: BTreeMap<String, f64>,
        request_by_type: BTreeMap<String, f64>,
        browser_metrics: BTreeMap<String, f64>,
        lifecycle_metrics: BTreeMap<String, f64>,
        console_errors: Option<f64>,
        page_errors: Option<f64>,
        artifacts: BTreeSet<ArtifactRef>,
        source_artifact: Option<ArtifactRef>,
        notes: Vec<String>,
    }

    #[derive(Debug, Clone, Default)]
    struct SampleContext {
        scenario: Option<String>,
        profile: Option<String>,
        matrix: BTreeMap<String, String>,
    }

    pub(super) fn read_evidence_set(
        root: &Path,
        include_local_paths: bool,
    ) -> homeboy::core::Result<EvidenceSet> {
        let mut notes = Vec::new();
        let mut files = Vec::new();
        collect_json_files(root, &mut files).map_err(|e| {
            homeboy::core::Error::internal_unexpected(format!(
                "Failed to read browser evidence directory {}: {}",
                root.display(),
                e
            ))
        })?;
        files.sort();

        let mut samples = Vec::new();
        for file in files {
            let raw = match std::fs::read_to_string(&file) {
                Ok(raw) => raw,
                Err(err) => {
                    notes.push(format!(
                        "skipped unreadable artifact {}: {}",
                        file.display(),
                        err
                    ));
                    continue;
                }
            };
            let value = match serde_json::from_str::<Value>(&raw) {
                Ok(value) => value,
                Err(err) => {
                    notes.push(format!(
                        "skipped invalid JSON artifact {}: {}",
                        file.display(),
                        err
                    ));
                    continue;
                }
            };
            let source = artifact_ref(root, &file, include_local_paths, None);
            collect_samples(&value, &SampleContext::default(), &source, &mut samples);
        }

        if samples.is_empty() {
            notes.push("no browser evidence samples found".to_string());
        }

        Ok(EvidenceSet { samples, notes })
    }

    pub(super) fn read_evidence_dirs(
        roots: &[PathBuf],
        include_local_paths: bool,
    ) -> homeboy::core::Result<EvidenceSet> {
        let mut merged = EvidenceSet {
            samples: Vec::new(),
            notes: Vec::new(),
        };
        for root in roots {
            match read_evidence_set(root, include_local_paths) {
                Ok(mut set) => {
                    merged.samples.append(&mut set.samples);
                    merged.notes.append(&mut set.notes);
                }
                Err(err) => merged.notes.push(format!(
                    "skipped unreadable evidence directory {}: {}",
                    root.display(),
                    err.message
                )),
            }
        }
        if roots.is_empty() {
            merged
                .notes
                .push("no browser evidence directories were provided".to_string());
        }
        Ok(merged)
    }

    fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect_json_files(&path, out)?;
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                out.push(path);
            }
        }
        Ok(())
    }

    fn collect_samples(
        value: &Value,
        inherited: &SampleContext,
        source: &ArtifactRef,
        samples: &mut Vec<BrowserEvidenceSample>,
    ) {
        match value {
            Value::Object(object) => collect_object_samples(object, inherited, source, samples),
            Value::Array(array) => {
                for item in array {
                    collect_samples(item, inherited, source, samples);
                }
            }
            _ => {}
        }
    }

    fn collect_object_samples(
        object: &Map<String, Value>,
        inherited: &SampleContext,
        source: &ArtifactRef,
        samples: &mut Vec<BrowserEvidenceSample>,
    ) {
        let context = context_for_object(object, inherited);
        let runs = object.get("runs").and_then(Value::as_array);

        if has_browser_signal(object) && runs.is_none() {
            samples.push(sample_from_object(object, &context, source));
        }

        if let Some(data) = object.get("data") {
            collect_samples(data, &context, source, samples);
        }
        for key in ["scenarios", "profiles", "variants", "matrix", "results"] {
            if let Some(value) = object.get(key) {
                collect_samples(value, &context, source, samples);
            }
        }
        if let Some(runs) = runs {
            for (index, run) in runs.iter().enumerate() {
                let mut run_context = context.clone();
                if run_context.profile.is_none() {
                    run_context.profile = Some(format!("repeat-{}", index + 1));
                }
                collect_samples(run, &run_context, source, samples);
            }
        }
    }

    fn context_for_object(object: &Map<String, Value>, inherited: &SampleContext) -> SampleContext {
        let mut context = inherited.clone();
        context.scenario = first_string(object, &["scenario_id", "scenario", "id"])
            .or(context.scenario)
            .filter(|value| value != "results" && value != "data");
        context.profile = first_string(
            object,
            &["profile_id", "profile", "browser_profile", "name"],
        )
        .or(context.profile);
        for key in ["matrix", "variant", "axes", "settings"] {
            if let Some(value) = object.get(key) {
                collect_matrix(value, key, &mut context.matrix);
            }
        }
        context
    }

    fn has_browser_signal(object: &Map<String, Value>) -> bool {
        [
            "assertions",
            "requests",
            "network_requests",
            "request_summary",
            "browser_metrics",
            "lifecycle_metrics",
            "dom_lifecycle",
            "console_errors",
            "page_errors",
            "errors",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
            || first_number(
                object,
                &[
                    "request_count",
                    "requests_total",
                    "dom_content_loaded_ms",
                    "load_event_ms",
                    "lcp_ms",
                ],
            )
            .is_some()
    }

    fn sample_from_object(
        object: &Map<String, Value>,
        context: &SampleContext,
        source: &ArtifactRef,
    ) -> BrowserEvidenceSample {
        let mut sample = BrowserEvidenceSample {
            scenario: context.scenario.clone(),
            profile: context.profile.clone(),
            matrix: context.matrix.clone(),
            source_artifact: Some(source.clone()),
            ..BrowserEvidenceSample::default()
        };
        sample.assertions = assertion_stats(object.get("assertions"));
        collect_requests(object, &mut sample);
        collect_metric_object(
            object.get("browser_metrics"),
            &mut sample.browser_metrics,
            &browser_metric_names(),
        );
        collect_metric_object(
            object.get("metrics"),
            &mut sample.browser_metrics,
            &browser_metric_names(),
        );
        collect_top_level_numbers(object, &mut sample.browser_metrics, &browser_metric_names());
        collect_metric_object(
            object.get("lifecycle_metrics"),
            &mut sample.lifecycle_metrics,
            &lifecycle_metric_names(),
        );
        collect_metric_object(
            object.get("dom_lifecycle"),
            &mut sample.lifecycle_metrics,
            &lifecycle_metric_names(),
        );
        collect_top_level_numbers(
            object,
            &mut sample.lifecycle_metrics,
            &lifecycle_metric_names(),
        );
        sample.console_errors = error_count(object, &["console_errors", "consoleErrors"]);
        sample.page_errors = error_count(object, &["page_errors", "pageErrors", "errors"]);
        collect_artifacts(object, &mut sample.artifacts);
        if sample.browser_metrics.is_empty() && sample.lifecycle_metrics.is_empty() {
            sample
                .notes
                .push("timing metrics missing or not numeric".to_string());
        }
        sample
    }

    pub(super) fn compare_variants(
        baseline: &[BrowserEvidenceSample],
        candidate: &[BrowserEvidenceSample],
    ) -> Vec<BrowserEvidenceVariantComparison> {
        let mut keys = BTreeSet::new();
        for sample in baseline.iter().chain(candidate.iter()) {
            keys.insert(variant_for_sample(sample));
        }

        keys.into_iter()
            .map(|variant| {
                let baseline_samples = baseline
                    .iter()
                    .filter(|sample| variant_for_sample(sample) == variant)
                    .collect::<Vec<_>>();
                let candidate_samples = candidate
                    .iter()
                    .filter(|sample| variant_for_sample(sample) == variant)
                    .collect::<Vec<_>>();
                compare_variant(variant, &baseline_samples, &candidate_samples)
            })
            .collect()
    }

    fn compare_variant(
        variant: BrowserEvidenceVariant,
        baseline: &[&BrowserEvidenceSample],
        candidate: &[&BrowserEvidenceSample],
    ) -> BrowserEvidenceVariantComparison {
        let baseline_assertions = assertion_sum(baseline);
        let candidate_assertions = assertion_sum(candidate);
        let notes = baseline
            .iter()
            .chain(candidate.iter())
            .flat_map(|sample| sample.notes.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        BrowserEvidenceVariantComparison {
            variant,
            baseline_repeats: baseline.len(),
            candidate_repeats: candidate.len(),
            assertions: AssertionComparison {
                pass_delta: candidate_assertions.passed as i64 - baseline_assertions.passed as i64,
                fail_delta: candidate_assertions.failed as i64 - baseline_assertions.failed as i64,
                baseline: baseline_assertions,
                candidate: candidate_assertions,
            },
            request_totals: compare_metric_values(
                &baseline
                    .iter()
                    .filter_map(|sample| sample.request_total)
                    .collect::<Vec<_>>(),
                &candidate
                    .iter()
                    .filter_map(|sample| sample.request_total)
                    .collect::<Vec<_>>(),
            ),
            request_by_host: compare_metric_maps(baseline, candidate, |sample| {
                &sample.request_by_host
            }),
            request_by_type: compare_metric_maps(baseline, candidate, |sample| {
                &sample.request_by_type
            }),
            browser_metrics: compare_metric_maps(baseline, candidate, |sample| {
                &sample.browser_metrics
            }),
            lifecycle_metrics: compare_metric_maps(baseline, candidate, |sample| {
                &sample.lifecycle_metrics
            }),
            console_errors: compare_metric_values(
                &baseline
                    .iter()
                    .filter_map(|sample| sample.console_errors)
                    .collect::<Vec<_>>(),
                &candidate
                    .iter()
                    .filter_map(|sample| sample.console_errors)
                    .collect::<Vec<_>>(),
            ),
            page_errors: compare_metric_values(
                &baseline
                    .iter()
                    .filter_map(|sample| sample.page_errors)
                    .collect::<Vec<_>>(),
                &candidate
                    .iter()
                    .filter_map(|sample| sample.page_errors)
                    .collect::<Vec<_>>(),
            ),
            artifacts: ArtifactComparison {
                baseline: artifact_refs(baseline),
                candidate: artifact_refs(candidate),
            },
            notes,
        }
    }

    pub(super) fn render_markdown(
        baseline_label: &str,
        candidate_label: &str,
        totals: &BrowserEvidenceCompareTotals,
        variants: &[BrowserEvidenceVariantComparison],
        notes: &[String],
    ) -> String {
        let mut out = String::new();
        out.push_str("## Browser Evidence Comparison\n\n");
        let _ = writeln!(out, "- Baseline: `{}`", baseline_label);
        let _ = writeln!(out, "- Candidate: `{}`", candidate_label);
        let _ = writeln!(out, "- Variants: **{}**", totals.variant_count);
        let _ = writeln!(
            out,
            "- Samples: **{}** baseline / **{}** candidate\n",
            totals.baseline_samples, totals.candidate_samples
        );

        out.push_str("### Scenario / Profile Matrix Summary\n");
        if variants.is_empty() {
            out.push_str("- No comparable browser evidence variants found.\n\n");
        } else {
            out.push_str("| Scenario | Profile | Matrix | Baseline repeats | Candidate repeats | Assertions fail delta | Request delta | Console error delta | Page error delta |\n");
            out.push_str("| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
            for variant in variants {
                let _ = writeln!(
                    out,
                    "| `{}` | `{}` | {} | {} | {} | {} | {} | {} | {} |",
                    escape_markdown_table_cell(&variant.variant.scenario),
                    escape_markdown_table_cell(&variant.variant.profile),
                    escape_markdown_table_cell(&format_matrix(&variant.variant.matrix)),
                    variant.baseline_repeats,
                    variant.candidate_repeats,
                    signed(variant.assertions.fail_delta as f64),
                    fmt_delta(variant.request_totals.median_delta),
                    fmt_delta(variant.console_errors.median_delta),
                    fmt_delta(variant.page_errors.median_delta),
                );
            }
            out.push('\n');
        }

        for variant in variants {
            let _ = writeln!(
                out,
                "### `{}` / `{}`\n",
                variant.variant.scenario, variant.variant.profile
            );
            if !variant.variant.matrix.is_empty() {
                let _ = writeln!(
                    out,
                    "- Matrix: `{}`",
                    format_matrix(&variant.variant.matrix)
                );
            }
            render_assertions(&mut out, variant);
            render_metric_section(&mut out, "Request Counts By Host", &variant.request_by_host);
            render_metric_section(
                &mut out,
                "Request Counts By Resource Type",
                &variant.request_by_type,
            );
            render_metric_section(&mut out, "Browser Metrics", &variant.browser_metrics);
            render_metric_section(
                &mut out,
                "DOM Lifecycle Metrics",
                &variant.lifecycle_metrics,
            );
            render_artifacts(&mut out, variant);
            if !variant.notes.is_empty() {
                out.push_str("**Notes**\n");
                for note in &variant.notes {
                    let _ = writeln!(out, "- {}", note);
                }
                out.push('\n');
            }
        }

        if !notes.is_empty() {
            out.push_str("### Report Notes\n");
            for note in notes {
                let _ = writeln!(out, "- {}", note);
            }
            out.push('\n');
        }

        out
    }

    fn render_assertions(out: &mut String, variant: &BrowserEvidenceVariantComparison) {
        out.push_str("**Pass/fail assertion deltas**\n");
        out.push_str("| Set | Total | Passed | Failed | Skipped |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
        for (label, stats) in [
            ("Baseline", &variant.assertions.baseline),
            ("Candidate", &variant.assertions.candidate),
        ] {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                label, stats.total, stats.passed, stats.failed, stats.skipped
            );
        }
        let _ = writeln!(
            out,
            "| Delta | - | {} | {} | - |\n",
            signed(variant.assertions.pass_delta as f64),
            signed(variant.assertions.fail_delta as f64)
        );
    }

    fn render_metric_section(
        out: &mut String,
        title: &str,
        metrics: &BTreeMap<String, MetricComparison>,
    ) {
        let _ = writeln!(out, "**{}**", title);
        if metrics.is_empty() {
            out.push_str("- No comparable metrics found.\n\n");
            return;
        }
        out.push_str("| Metric | Baseline min/median/max | Candidate min/median/max | Median delta | Delta % |\n");
        out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
        for (metric, comparison) in metrics.iter().take(12) {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} |",
                escape_markdown_table_cell(metric),
                fmt_stats(comparison.baseline.as_ref()),
                fmt_stats(comparison.candidate.as_ref()),
                fmt_delta(comparison.median_delta),
                comparison
                    .median_delta_pct
                    .map(|value| format!("{}%", signed(value)))
                    .unwrap_or_else(|| "-".to_string())
            );
        }
        out.push('\n');
    }

    fn render_artifacts(out: &mut String, variant: &BrowserEvidenceVariantComparison) {
        out.push_str("**Artifacts**\n");
        for (label, artifacts) in [
            ("Baseline", &variant.artifacts.baseline),
            ("Candidate", &variant.artifacts.candidate),
        ] {
            if artifacts.is_empty() {
                let _ = writeln!(out, "- {}: none recorded", label);
                continue;
            }
            let rendered = artifacts
                .iter()
                .take(8)
                .map(|artifact| format!("{}: {}", artifact.label, artifact.target))
                .collect::<Vec<_>>()
                .join("; ");
            let _ = writeln!(out, "- {}: {}", label, rendered);
        }
        out.push('\n');
    }

    fn variant_for_sample(sample: &BrowserEvidenceSample) -> BrowserEvidenceVariant {
        BrowserEvidenceVariant {
            scenario: sample
                .scenario
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            profile: sample
                .profile
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            matrix: sample.matrix.clone(),
        }
    }

    fn assertion_stats(value: Option<&Value>) -> AssertionStats {
        let Some(value) = value else {
            return AssertionStats::default();
        };
        if let Some(object) = value.as_object() {
            return AssertionStats {
                total: u64_value(object, "total").unwrap_or_default(),
                passed: u64_value(object, "passed").unwrap_or_default(),
                failed: u64_value(object, "failed").unwrap_or_default(),
                skipped: u64_value(object, "skipped").unwrap_or_default(),
            };
        }
        let mut stats = AssertionStats::default();
        for assertion in value.as_array().into_iter().flatten() {
            let status = assertion
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            stats.total += 1;
            match status {
                "pass" | "passed" | "ok" | "success" => stats.passed += 1,
                "fail" | "failed" | "error" => stats.failed += 1,
                "skip" | "skipped" => stats.skipped += 1,
                _ => {}
            }
        }
        stats
    }

    fn collect_requests(object: &Map<String, Value>, sample: &mut BrowserEvidenceSample) {
        sample.request_total = first_number(object, &["request_count", "requests_total"]);
        if let Some(requests) = object
            .get("requests")
            .or_else(|| object.get("network_requests"))
            .and_then(Value::as_array)
        {
            sample.request_total = Some(requests.len() as f64);
            for request in requests {
                if let Some(host) = request_host(request) {
                    *sample.request_by_host.entry(host).or_default() += 1.0;
                }
                if let Some(resource_type) =
                    first_value_string(request, &["resource_type", "resourceType", "type"])
                {
                    *sample.request_by_type.entry(resource_type).or_default() += 1.0;
                }
            }
        }
        if let Some(summary) = object.get("request_summary").and_then(Value::as_object) {
            if sample.request_total.is_none() {
                sample.request_total = first_number(summary, &["total", "count"]);
            }
            collect_count_map(summary.get("by_host"), &mut sample.request_by_host);
            collect_count_map(summary.get("by_type"), &mut sample.request_by_type);
            collect_count_map(summary.get("by_resource_type"), &mut sample.request_by_type);
        }
    }

    fn collect_artifacts(object: &Map<String, Value>, artifacts: &mut BTreeSet<ArtifactRef>) {
        let Some(values) = object.get("artifacts").and_then(Value::as_array) else {
            return;
        };
        for artifact in values {
            let label = first_value_string(artifact, &["label", "kind", "type", "name"])
                .unwrap_or_else(|| "artifact".to_string());
            let target = first_value_string(artifact, &["url", "href", "path", "target"]);
            if let Some(target) = target {
                artifacts.insert(ArtifactRef { label, target });
            }
        }
    }

    fn artifact_refs(samples: &[&BrowserEvidenceSample]) -> Vec<ArtifactRef> {
        samples
            .iter()
            .flat_map(|sample| {
                let mut artifacts = sample.artifacts.iter().cloned().collect::<Vec<_>>();
                if let Some(source) = &sample.source_artifact {
                    artifacts.push(source.clone());
                }
                artifacts
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn assertion_sum(samples: &[&BrowserEvidenceSample]) -> AssertionStats {
        samples
            .iter()
            .fold(AssertionStats::default(), |mut acc, sample| {
                acc.total += sample.assertions.total;
                acc.passed += sample.assertions.passed;
                acc.failed += sample.assertions.failed;
                acc.skipped += sample.assertions.skipped;
                acc
            })
    }

    fn compare_metric_maps(
        baseline: &[&BrowserEvidenceSample],
        candidate: &[&BrowserEvidenceSample],
        map: fn(&BrowserEvidenceSample) -> &BTreeMap<String, f64>,
    ) -> BTreeMap<String, MetricComparison> {
        let keys = baseline
            .iter()
            .chain(candidate.iter())
            .flat_map(|sample| map(sample).keys().cloned())
            .collect::<BTreeSet<_>>();
        keys.into_iter()
            .map(|key| {
                let baseline_values = baseline
                    .iter()
                    .filter_map(|sample| map(sample).get(&key).copied())
                    .collect::<Vec<_>>();
                let candidate_values = candidate
                    .iter()
                    .filter_map(|sample| map(sample).get(&key).copied())
                    .collect::<Vec<_>>();
                (
                    key,
                    compare_metric_values(&baseline_values, &candidate_values),
                )
            })
            .collect()
    }

    fn compare_metric_values(baseline: &[f64], candidate: &[f64]) -> MetricComparison {
        let baseline_stats = metric_stats(baseline);
        let candidate_stats = metric_stats(candidate);
        let median_delta = baseline_stats
            .as_ref()
            .zip(candidate_stats.as_ref())
            .map(|(baseline, candidate)| candidate.median - baseline.median);
        let median_delta_pct = baseline_stats
            .as_ref()
            .zip(candidate_stats.as_ref())
            .and_then(|(baseline, candidate)| {
                (baseline.median.abs() > f64::EPSILON)
                    .then(|| ((candidate.median - baseline.median) / baseline.median) * 100.0)
            });
        MetricComparison {
            baseline: baseline_stats,
            candidate: candidate_stats,
            median_delta,
            median_delta_pct,
        }
    }

    fn metric_stats(values: &[f64]) -> Option<MetricStats> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        Some(MetricStats {
            n: sorted.len(),
            min: sorted[0],
            median: median(&sorted),
            max: *sorted.last().unwrap_or(&sorted[0]),
        })
    }

    fn median(sorted: &[f64]) -> f64 {
        let mid = sorted.len() / 2;
        if sorted.len() % 2 == 0 {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        }
    }

    fn collect_metric_object(
        value: Option<&Value>,
        out: &mut BTreeMap<String, f64>,
        names: &[&str],
    ) {
        let Some(object) = value.and_then(Value::as_object) else {
            return;
        };
        collect_top_level_numbers(object, out, names);
    }

    fn collect_top_level_numbers(
        object: &Map<String, Value>,
        out: &mut BTreeMap<String, f64>,
        names: &[&str],
    ) {
        for name in names {
            if let Some(value) = number_value(object, name) {
                out.insert((*name).to_string(), value);
            }
        }
    }

    fn collect_count_map(value: Option<&Value>, out: &mut BTreeMap<String, f64>) {
        let Some(object) = value.and_then(Value::as_object) else {
            return;
        };
        for (key, value) in object {
            if let Some(value) = value.as_f64() {
                out.insert(key.clone(), value);
            }
        }
    }

    fn collect_matrix(value: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    if let Some(value) = scalar_string(value) {
                        out.insert(key.clone(), value);
                    }
                }
            }
            Value::String(value) => {
                out.insert(prefix.to_string(), value.clone());
            }
            _ => {}
        }
    }

    fn artifact_ref(
        root: &Path,
        path: &Path,
        include_local_paths: bool,
        label: Option<String>,
    ) -> ArtifactRef {
        let target = if include_local_paths {
            path.display().to_string()
        } else {
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string()
        };
        ArtifactRef {
            label: label.unwrap_or_else(|| "source".to_string()),
            target,
        }
    }

    fn request_host(value: &Value) -> Option<String> {
        first_value_string(value, &["host", "hostname"]).or_else(|| {
            let url = first_value_string(value, &["url", "href"])?;
            host_from_url(&url)
        })
    }

    fn host_from_url(url: &str) -> Option<String> {
        let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
        after_scheme
            .split(['/', '?', '#'])
            .next()
            .filter(|host| !host.is_empty())
            .map(|host| host.to_string())
    }

    fn error_count(object: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
        for key in keys {
            if let Some(value) = object.get(*key) {
                if let Some(count) = value.as_f64() {
                    return Some(count);
                }
                if let Some(array) = value.as_array() {
                    return Some(array.len() as f64);
                }
            }
        }
        None
    }

    fn first_string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| object.get(*key).and_then(scalar_string))
    }

    fn first_value_string(value: &Value, keys: &[&str]) -> Option<String> {
        let object = value.as_object()?;
        first_string(object, keys)
    }

    fn first_number(object: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
        keys.iter().find_map(|key| number_value(object, key))
    }

    fn number_value(object: &Map<String, Value>, key: &str) -> Option<f64> {
        object.get(key).and_then(Value::as_f64)
    }

    fn u64_value(object: &Map<String, Value>, key: &str) -> Option<u64> {
        object.get(key).and_then(Value::as_u64)
    }

    fn scalar_string(value: &Value) -> Option<String> {
        match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        }
    }

    fn browser_metric_names() -> Vec<&'static str> {
        vec![
            "fcp_ms",
            "lcp_ms",
            "cls",
            "ttfb_ms",
            "total_blocking_time_ms",
            "load_ms",
            "duration_ms",
            "ready_ms",
        ]
    }

    fn lifecycle_metric_names() -> Vec<&'static str> {
        vec![
            "dom_content_loaded_ms",
            "domContentLoaded_ms",
            "load_event_ms",
            "network_idle_ms",
            "first_paint_ms",
            "interactive_ms",
        ]
    }

    fn format_matrix(matrix: &BTreeMap<String, String>) -> String {
        if matrix.is_empty() {
            return "-".to_string();
        }
        matrix
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn fmt_stats(stats: Option<&MetricStats>) -> String {
        stats
            .map(|stats| {
                format!(
                    "{} / {} / {} (n={})",
                    fmt_number(stats.min),
                    fmt_number(stats.median),
                    fmt_number(stats.max),
                    stats.n
                )
            })
            .unwrap_or_else(|| "-".to_string())
    }

    fn fmt_delta(value: Option<f64>) -> String {
        value.map(signed).unwrap_or_else(|| "-".to_string())
    }

    fn signed(value: f64) -> String {
        if value > 0.0 {
            format!("+{}", fmt_number(value))
        } else {
            fmt_number(value)
        }
    }

    fn fmt_number(value: f64) -> String {
        if value.fract().abs() < f64::EPSILON {
            format!("{:.0}", value)
        } else {
            format!("{:.2}", value)
        }
    }
}
