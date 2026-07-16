use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use super::{
    append_details_block, array_from_object, array_value, object_value, read_command_json,
    render_error_details, render_full_log,
};

pub(super) fn render_audit_section(out: &mut String, output_dir: &Path, run_url: &str) {
    out.push_str("### Audit Failure Digest\n");
    let (data, error) = super::envelope_parts(read_command_json(output_dir, "audit"));
    render_error_details(out, &error);

    let summary = object_value(&data, "summary");
    let baseline = object_value(&data, "baseline_comparison");

    if let Some(score) = summary.get("alignment_score").and_then(Value::as_f64) {
        let _ = writeln!(out, "- Alignment score: **{:.3}**", score);
    }
    let severity_counts = severity_counts(&data);
    if !severity_counts.is_empty() {
        let text = severity_counts
            .iter()
            .map(|(severity, count)| format!("{severity}: {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "- Severity counts: **{}**", text);
    }
    if let Some(outliers) = summary.get("outliers_found").and_then(Value::as_i64) {
        let _ = writeln!(out, "- Outliers in current run: **{}**", outliers);
    }

    let outlier_items = collect_outlier_items(&data);
    if !outlier_items.is_empty() {
        let _ = writeln!(out, "- Parsed outlier entries: **{}**", outlier_items.len());
    }

    render_audit_baseline_summary(out, &baseline);
    render_audit_findings(out, &data, &outlier_items);
    render_full_log(out, "audit", run_url);
    out.push('\n');
}

fn render_audit_baseline_summary(out: &mut String, baseline: &Map<String, Value>) {
    let drift_increased = baseline
        .get("drift_increased")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let _ = writeln!(
        out,
        "- Drift increased: **{}**",
        if drift_increased { "yes" } else { "no" }
    );

    let new_items = array_from_object(baseline, "new_items");
    if !new_items.is_empty() {
        let _ = writeln!(
            out,
            "- New findings since baseline: **{}**",
            new_items.len()
        );
        for (idx, item) in new_items.iter().take(5).enumerate() {
            let context = item_string(item, &["context_label", "file"], "unknown");
            let message = item_string(item, &["description", "message"], "(new finding)");
            let fingerprint = item_string(item, &["fingerprint"], "");
            let _ = write!(out, "  {}. **{}**", idx + 1, context);
            if !message.is_empty() {
                let _ = write!(out, " — {}", message);
            }
            if !fingerprint.is_empty() {
                let _ = write!(out, " (`{}`)", fingerprint);
            }
            out.push('\n');
        }
    }
}

fn render_audit_findings(out: &mut String, data: &Map<String, Value>, outlier_items: &[Value]) {
    let top_findings = collect_audit_findings(data, outlier_items);
    if top_findings.is_empty() {
        out.push_str("- No structured audit findings available.\n");
    } else {
        out.push_str("- Top actionable findings:\n");
        for (idx, finding) in top_findings.iter().take(5).enumerate() {
            let _ = writeln!(out, "  {}. {}", idx + 1, format_audit_finding(finding));
        }
        let detail_lines = top_findings
            .iter()
            .take(300)
            .enumerate()
            .map(|(idx, finding)| format!("{}. {}", idx + 1, format_audit_finding(finding)))
            .collect::<Vec<_>>();
        append_details_block(
            out,
            &format!("All parsed audit findings ({})", top_findings.len()),
            &detail_lines,
            300,
        );
    }
}

fn severity_counts(data: &Map<String, Value>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let outliers = collect_outlier_items(data);
    for finding in collect_audit_findings(data, &outliers) {
        let severity = item_string(&finding, &["severity", "level"], "unknown").to_lowercase();
        *counts.entry(severity).or_insert(0) += 1;
    }
    counts
}

fn collect_outlier_items(data: &Map<String, Value>) -> Vec<Value> {
    let mut outliers = Vec::new();
    for convention in array_value(data, "conventions") {
        let Some(obj) = convention.as_object() else {
            continue;
        };
        let label = item_string(
            convention,
            &["context_label", "name", "rule", "pattern"],
            "unknown",
        );
        for outlier in array_value(obj, "outliers") {
            let mut item = outlier.clone();
            if let Value::Object(ref mut map) = item {
                map.entry("context_label".to_string())
                    .or_insert_with(|| Value::String(label.clone()));
            }
            outliers.push(item);
        }
    }
    outliers
}

fn collect_audit_findings(data: &Map<String, Value>, outliers: &[Value]) -> Vec<Value> {
    let mut findings = array_value(data, "findings")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    findings.extend(outliers.iter().cloned());
    findings
}

fn item_string(item: &Value, keys: &[&str], fallback: &str) -> String {
    let Some(obj) = item.as_object() else {
        return fallback.to_string();
    };

    for key in keys {
        if let Some(value) = obj.get(*key) {
            if let Some(s) = value.as_str() {
                if !s.is_empty() {
                    return s.to_string();
                }
            } else if !value.is_null() {
                return value.to_string();
            }
        }
    }
    fallback.to_string()
}

fn format_audit_finding(finding: &Value) -> String {
    let file = item_string(finding, &["file", "path", "context_label"], "unknown");
    let rule = item_string(finding, &["rule", "kind", "category"], "outlier");
    let message = item_string(finding, &["description", "message"], "");
    if message.is_empty() {
        format!("**{}** — {}", file, rule)
    } else {
        format!("**{}** — {} — {}", file, rule, message)
    }
}
