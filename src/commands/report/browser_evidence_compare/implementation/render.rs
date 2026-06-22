use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::commands::escape_markdown_table_cell;

use super::super::types::{
    ArtifactComparison, AssertionStats, BrowserEvidenceCompareTotals,
    BrowserEvidenceVariantComparison, MetricComparison, MetricStats,
};

pub(in crate::commands::report::browser_evidence_compare) fn render_markdown(
    baseline_label: &str,
    candidate_label: &str,
    totals: &BrowserEvidenceCompareTotals,
    artifacts: &ArtifactComparison,
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
        render_visual_compare(&mut out, variant);
        if !variant.notes.is_empty() {
            out.push_str("**Notes**\n");
            for note in &variant.notes {
                let _ = writeln!(out, "- {}", note);
            }
            out.push('\n');
        }
    }

    render_provenance_artifacts(&mut out, artifacts);

    if !notes.is_empty() {
        out.push_str("### Report Notes\n");
        for note in notes {
            let _ = writeln!(out, "- {}", note);
        }
        out.push('\n');
    }

    out
}

fn render_provenance_artifacts(out: &mut String, artifacts: &ArtifactComparison) {
    if artifacts.baseline.is_empty() && artifacts.candidate.is_empty() {
        return;
    }
    out.push_str("### Provenance / Artifact Records\n");
    for (label, refs) in [
        ("Baseline", &artifacts.baseline),
        ("Candidate", &artifacts.candidate),
    ] {
        if refs.is_empty() {
            let _ = writeln!(out, "- {}: none recorded", label);
            continue;
        }
        let rendered = refs
            .iter()
            .take(12)
            .map(|artifact| format!("{}: {}", artifact.label, artifact.target))
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(out, "- {}: {}", label, rendered);
    }
    out.push('\n');
}

fn render_assertions(out: &mut String, variant: &BrowserEvidenceVariantComparison) {
    out.push_str("**Pass/fail assertion deltas**\n");
    out.push_str("| Set | Total | Passed | Failed | Advisory failed | Skipped |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | ---: |\n");
    for (label, stats) in [
        ("Baseline", &variant.assertions.baseline),
        ("Candidate", &variant.assertions.candidate),
    ] {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            label, stats.total, stats.passed, stats.failed, stats.advisory_failed, stats.skipped
        );
    }
    let _ = writeln!(
        out,
        "| Delta | - | {} | {} | {} | - |\n",
        signed(variant.assertions.pass_delta as f64),
        signed(variant.assertions.fail_delta as f64),
        signed(
            variant.assertions.candidate.advisory_failed as f64
                - variant.assertions.baseline.advisory_failed as f64
        )
    );

    render_advisory_failures(out, "Baseline", &variant.assertions.baseline);
    render_advisory_failures(out, "Candidate", &variant.assertions.candidate);
}

fn render_advisory_failures(out: &mut String, label: &str, stats: &AssertionStats) {
    if stats.failed_advisory_assertions.is_empty() {
        return;
    }
    let _ = writeln!(out, "**{} failed advisory assertions**", label);
    for failure in &stats.failed_advisory_assertions {
        let selector = failure
            .selector
            .as_deref()
            .map(|selector| format!(" selector `{}`", selector))
            .unwrap_or_default();
        let message = failure
            .message
            .as_deref()
            .map(|message| format!(" - {}", message))
            .unwrap_or_default();
        let _ = writeln!(out, "- `{}`{}{}", failure.id, selector, message);
    }
    out.push('\n');
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

fn render_visual_compare(out: &mut String, variant: &BrowserEvidenceVariantComparison) {
    let Some(visual) = variant.visual_compare.as_ref() else {
        return;
    };
    out.push_str("**Visual compare**\n");
    let _ = writeln!(
        out,
        "- Status: `{}`",
        visual.status.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        out,
        "- Mismatch: {} pixels / {} total ({})",
        visual
            .mismatch_pixels
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        visual
            .total_pixels
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        visual
            .mismatch_ratio
            .map(|value| format!("{:.4}", value))
            .unwrap_or_else(|| "unknown".to_string())
    );
    if let Some(dimension_mismatch) = visual.dimension_mismatch {
        let _ = writeln!(out, "- Dimension mismatch: `{}`", dimension_mismatch);
    }
    if !visual.artifacts.is_empty() {
        let rendered = visual
            .artifacts
            .iter()
            .map(|artifact| format!("{}: {}", artifact.label, artifact.target))
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(out, "- Artifacts: {}", rendered);
    }
    out.push('\n');
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
