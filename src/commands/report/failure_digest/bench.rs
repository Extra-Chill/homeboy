use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use super::{
    array_value, number_value, object_value, read_command_json, render_full_log, string_value,
};
use crate::commands::escape_markdown_table_cell;

pub(super) fn render_bench_section(out: &mut String, output_dir: &Path, run_url: &str) {
    let (data, error) = super::envelope_parts(read_command_json(output_dir, "bench"));

    let component = string_value(&data, "component")
        .or_else(|| string_value(&object_value(&data, "results"), "component_id"))
        .unwrap_or_else(|| "unknown".to_string());
    let status = string_value(&data, "status")
        .or_else(|| string_value(&error, "code"))
        .unwrap_or_else(|| "unknown".to_string());

    let _ = writeln!(out, "### Bench: {}", component);
    let _ = writeln!(out, "**Status:** {}\n", status.to_uppercase());

    if let Some(message) = string_value(&error, "message") {
        out.push_str("**Summary**\n");
        let _ = writeln!(out, "- {}\n", message);
    }

    render_bench_summary(out, &data);
    render_budget_findings(out, &data);

    let artifacts = collect_bench_artifacts(&data);
    if !artifacts.is_empty() {
        out.push_str("**Artifacts**\n");
        for artifact in artifacts {
            let _ = writeln!(out, "- {}", artifact);
        }
    } else {
        out.push_str("**Artifacts**\n- No structured bench artifacts available.\n");
    }

    render_full_log(out, "bench", run_url);
    out.push('\n');
}

fn render_bench_summary(out: &mut String, data: &Map<String, Value>) {
    let summaries = array_value(data, "summary");
    if summaries.is_empty() {
        return;
    }

    out.push_str("**Summary**\n");
    for summary in summaries {
        let Some(summary) = summary.as_object() else {
            continue;
        };
        let scenario = string_value(summary, "scenario").unwrap_or_else(|| "unknown".to_string());
        let metric = string_value(summary, "metric").unwrap_or_else(|| "metric".to_string());
        let rows = array_value(summary, "rows")
            .into_iter()
            .filter_map(format_bench_summary_row)
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(out, "- `{}` (`{}`): {}", scenario, metric, rows.join("; "));
    }
    out.push('\n');
}

fn render_budget_findings(out: &mut String, data: &Map<String, Value>) {
    let mut findings = array_value(data, "budget_findings");
    let results = object_value(data, "results");
    if findings.is_empty() {
        findings = array_value(&results, "budget_findings");
    }
    if findings.is_empty() {
        return;
    }

    out.push_str("**Budget findings**\n");
    out.push_str("| Code | Subject | Actual | Expected | Unit | Message |\n");
    out.push_str("| --- | --- | ---: | ---: | --- | --- |\n");
    for finding in findings.iter().take(10) {
        let Some(finding) = finding.as_object() else {
            continue;
        };
        let code = budget_string_value(finding, "code")
            .or_else(|| string_value(finding, "rule"))
            .unwrap_or_else(|| "budget".to_string());
        let subject = budget_string_value(finding, "subject")
            .or_else(|| budget_string_value(finding, "context_label"))
            .unwrap_or_else(|| "-".to_string());
        let actual = budget_number_value(finding, "actual")
            .map(format_report_number)
            .unwrap_or_else(|| "-".to_string());
        let expected = budget_number_value(finding, "expected")
            .map(format_report_number)
            .unwrap_or_else(|| "-".to_string());
        let unit = budget_string_value(finding, "unit").unwrap_or_else(|| "-".to_string());
        let message = string_value(finding, "message").unwrap_or_default();
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} |",
            escape_markdown_table_cell(&code),
            escape_markdown_table_cell(&subject),
            actual,
            expected,
            escape_markdown_table_cell(&unit),
            escape_markdown_table_cell(&message)
        );
    }
    out.push('\n');
}

fn budget_string_value(finding: &Map<String, Value>, key: &str) -> Option<String> {
    string_value(finding, key)
        .or_else(|| {
            object_value(finding, "metadata")
                .get(key)
                .and_then(value_to_string)
        })
        .or_else(|| {
            object_value(finding, "raw")
                .get(key)
                .and_then(value_to_string)
        })
}

fn budget_number_value(finding: &Map<String, Value>, key: &str) -> Option<f64> {
    number_value(finding, key)
        .or_else(|| {
            object_value(finding, "metadata")
                .get(key)
                .and_then(Value::as_f64)
        })
        .or_else(|| {
            object_value(finding, "raw")
                .get(key)
                .and_then(Value::as_f64)
        })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn format_report_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn format_bench_summary_row(row: &Value) -> Option<String> {
    let row = row.as_object()?;
    let rig_id = string_value(row, "rig_id")?;
    let mut parts = vec![format!("{}", rig_id)];
    push_number_part(&mut parts, row, "n", "n", None);
    push_number_part(&mut parts, row, "p50_ms", "p50", Some("ms"));
    push_number_part(&mut parts, row, "p95_ms", "p95", Some("ms"));
    push_number_part(&mut parts, row, "mean_ms", "mean", Some("ms"));
    push_number_part(&mut parts, row, "cv_pct", "cv", Some("%"));
    push_signed_number_part(&mut parts, row, "delta_p50_pct", "delta_p50", "%");
    Some(parts.join(" "))
}

fn push_number_part(
    parts: &mut Vec<String>,
    row: &Map<String, Value>,
    key: &str,
    label: &str,
    suffix: Option<&str>,
) {
    if let Some(value) = number_value(row, key) {
        parts.push(format_number_part(label, value, suffix));
    }
}

fn push_signed_number_part(
    parts: &mut Vec<String>,
    row: &Map<String, Value>,
    key: &str,
    label: &str,
    suffix: &str,
) {
    if let Some(value) = number_value(row, key) {
        parts.push(format!("{}={:+.1}{}", label, value, suffix));
    }
}

fn format_number_part(label: &str, value: f64, suffix: Option<&str>) -> String {
    match suffix {
        Some(suffix) => format!("{}={:.1}{}", label, value, suffix),
        None if value.fract().abs() < f64::EPSILON => format!("{}={:.0}", label, value),
        None => format!("{}={:.1}", label, value),
    }
}

fn collect_bench_artifacts(data: &Map<String, Value>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut rendered = Vec::new();

    for artifact in array_value(data, "artifacts") {
        push_bench_artifact(&mut seen, &mut rendered, None, artifact);
    }

    for rig in array_value(data, "rigs") {
        let Some(rig_obj) = rig.as_object() else {
            continue;
        };
        let rig_id = string_value(rig_obj, "rig_id");
        for artifact in array_value(rig_obj, "artifacts") {
            push_bench_artifact(&mut seen, &mut rendered, rig_id.as_deref(), artifact);
        }
    }

    rendered
}

fn push_bench_artifact(
    seen: &mut BTreeSet<(Option<String>, String)>,
    rendered: &mut Vec<String>,
    rig_id: Option<&str>,
    artifact: &Value,
) {
    let Some(obj) = artifact.as_object() else {
        return;
    };
    let Some(path) = string_value(obj, "path") else {
        return;
    };
    let key = (rig_id.map(str::to_string), path.clone());
    if !seen.insert(key) {
        return;
    }

    let label = string_value(obj, "label")
        .or_else(|| string_value(obj, "name"))
        .unwrap_or_else(|| "artifact".to_string());
    let scenario = string_value(obj, "scenario_id");
    let run_index = display_value(obj, "run_index");
    let kind = string_value(obj, "kind");

    let mut prefix = Vec::new();
    if let Some(rig) = rig_id {
        prefix.push(format!("rig `{}`", rig));
    }
    if let Some(scenario) = scenario {
        prefix.push(format!("scenario `{}`", scenario));
    }
    if let Some(run_index) = run_index {
        prefix.push(format!("run {}", run_index));
    }

    let mut line = if prefix.is_empty() {
        label
    } else {
        format!("{} — {}", prefix.join(" / "), label)
    };
    if let Some(kind) = kind {
        let _ = write!(line, " ({})", kind);
    }
    let _ = write!(line, ": {}", path);
    rendered.push(line);
}

fn display_value(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|value| match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bench_digest_renders_summary_percentiles() {
        let mut data = Map::new();
        data.insert("component".to_string(), Value::String("studio".to_string()));
        data.insert(
            "summary".to_string(),
            json!([{
                "scenario": "create-site",
                "metric": "elapsed_ms",
                "rows": [
                    {"rig_id": "baseline", "n": 3, "p50_ms": 100.0, "p95_ms": 180.0, "mean_ms": 120.0, "cv_pct": 10.0},
                    {"rig_id": "candidate", "n": 3, "p50_ms": 110.0, "p95_ms": 200.0, "mean_ms": 130.0, "cv_pct": 12.0, "delta_p50_pct": 10.0}
                ]
            }]),
        );

        let mut out = String::new();
        render_bench_summary(&mut out, &data);

        assert!(out.contains("**Summary**"));
        assert!(out.contains("`create-site` (`elapsed_ms`)"));
        assert!(out.contains("baseline n=3 p50=100.0ms p95=180.0ms"));
        assert!(out.contains("candidate n=3 p50=110.0ms p95=200.0ms"));
        assert!(out.contains("delta_p50=+10.0%"));
    }

    #[test]
    fn bench_artifact_digest_renders_numeric_run_index() {
        let data = json!({
            "rigs": [{
                "rig_id": "candidate",
                "artifacts": [{
                    "scenario_id": "create-site",
                    "run_index": 2,
                    "name": "raw_result",
                    "kind": "json",
                    "path": "artifacts/run-2/raw-result.json"
                }]
            }]
        });
        let data = data.as_object().expect("object");

        let artifacts = collect_bench_artifacts(data);

        assert_eq!(
            artifacts,
            vec!["rig `candidate` / scenario `create-site` / run 2 — raw_result (json): artifacts/run-2/raw-result.json"]
        );
    }
}
