use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use super::{object_value, read_command_json, render_full_log, string_value};

pub(super) fn render_trace_section(out: &mut String, output_dir: &Path, run_url: &str) {
    let (data, error) = super::envelope_parts(read_command_json(output_dir, "trace"));
    let results = object_value(&data, "results");
    let failure = object_value(&data, "failure");

    let component = string_value(&data, "component")
        .or_else(|| string_value(&results, "component_id"))
        .or_else(|| string_value(&failure, "component_id"))
        .unwrap_or_else(|| "unknown".to_string());
    let scenario_id = string_value(&data, "scenario_id")
        .or_else(|| string_value(&results, "scenario_id"))
        .or_else(|| string_value(&failure, "scenario_id"));
    let status = string_value(&data, "status")
        .or_else(|| string_value(&results, "status"))
        .or_else(|| string_value(&error, "code"))
        .unwrap_or_else(|| "unknown".to_string());

    let title = scenario_id
        .as_ref()
        .map(|scenario| format!("{} / {}", component, scenario))
        .unwrap_or(component);
    let _ = writeln!(out, "### Trace: {}", title);
    let _ = writeln!(out, "**Status:** {}\n", status.to_uppercase());

    render_trace_summary(out, &data, &results, &failure, &error);
    render_trace_toolchain(out, &data, &results);
    render_trace_artifacts(out, &data, &results);
    render_full_log(out, "trace", run_url);
    out.push('\n');
}

fn render_trace_summary(
    out: &mut String,
    data: &Map<String, Value>,
    results: &Map<String, Value>,
    failure: &Map<String, Value>,
    error: &Map<String, Value>,
) {
    let mut summary_lines = Vec::new();
    if let Some(summary) =
        string_value(data, "summary").or_else(|| string_value(results, "summary"))
    {
        summary_lines.push(summary);
    }
    if let Some(failure_message) = string_value(results, "failure") {
        summary_lines.push(failure_message);
    }
    if let Some(stderr_excerpt) = string_value(failure, "stderr_excerpt") {
        summary_lines.push(stderr_excerpt);
    }
    if let Some(message) = string_value(error, "message") {
        summary_lines.push(message);
    }

    if !summary_lines.is_empty() {
        out.push_str("**Summary**\n");
        for line in summary_lines {
            let _ = writeln!(out, "- {}", line);
        }
        out.push('\n');
    }

    render_trace_spans(out, data, results);
}

fn render_trace_toolchain(
    out: &mut String,
    data: &Map<String, Value>,
    results: &Map<String, Value>,
) {
    let toolchain = first_object(data, results, "toolchain");
    let components = first_object(data, results, "components");
    let Some(ref toolchain) = toolchain else {
        return;
    };

    out.push_str("**Toolchain provenance**\n");
    let mode = string_value(toolchain, "mode").unwrap_or_else(|| "unknown".to_string());
    let canonical = toolchain
        .get("canonical")
        .and_then(Value::as_bool)
        .map(|value| if value { "yes" } else { "no" })
        .unwrap_or("unknown");
    let _ = writeln!(out, "- Mode: `{mode}`; canonical: **{canonical}**");
    let homeboy = object_value(toolchain, "homeboy");
    if !homeboy.is_empty() {
        render_trace_git_provenance(out, "Homeboy", &homeboy);
    }
    let wp_codebox = object_value(toolchain, "wp_codebox");
    if !wp_codebox.is_empty() {
        render_trace_git_provenance(out, "WP Codebox", &wp_codebox);
    }
    if let Some(components) = components.as_ref() {
        let target = object_value(components, "target");
        if !target.is_empty() {
            render_trace_git_provenance(out, "Target", &target);
        }
    }
    if let Some(reasons) = toolchain.get("reasons").and_then(Value::as_array) {
        let reasons = reasons.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        if !reasons.is_empty() {
            let _ = writeln!(out, "- Non-canonical reason(s): {}", reasons.join("; "));
        }
    }
    out.push('\n');
}

fn first_object(
    data: &Map<String, Value>,
    results: &Map<String, Value>,
    key: &str,
) -> Option<Map<String, Value>> {
    [object_value(data, key), object_value(results, key)]
        .into_iter()
        .find(|value| !value.is_empty())
}

fn render_trace_git_provenance(out: &mut String, label: &str, provenance: &Map<String, Value>) {
    let path = string_value(provenance, "path").unwrap_or_else(|| "unknown".to_string());
    let sha = string_value(provenance, "sha").unwrap_or_else(|| "unknown".to_string());
    let branch = string_value(provenance, "branch").unwrap_or_else(|| "unknown".to_string());
    let dirty = provenance
        .get("dirty")
        .and_then(Value::as_bool)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let _ = writeln!(
        out,
        "- {label}: `{path}` @ `{sha}` (branch `{branch}`, dirty `{dirty}`)"
    );
}

fn render_trace_spans(out: &mut String, data: &Map<String, Value>, results: &Map<String, Value>) {
    let spans = collect_trace_spans(data, results);
    if spans.is_empty() {
        return;
    }

    out.push_str("**Spans**\n");
    out.push_str("| Span | From | To | Duration | Status | Metadata |\n");
    out.push_str("|---|---|---|---:|---|---|\n");
    for span in spans {
        let duration = span
            .duration_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".to_string());
        let _ = writeln!(
            out,
            "| `{}` | `{}` | `{}` | {} | {} | {} |",
            span.id, span.from, span.to, duration, span.status, span.metadata
        );
    }
    out.push('\n');
}

#[derive(Debug, PartialEq)]
struct TraceSpanRow {
    id: String,
    from: String,
    to: String,
    duration_ms: Option<u64>,
    status: String,
    metadata: String,
}

fn collect_trace_spans(
    data: &Map<String, Value>,
    results: &Map<String, Value>,
) -> Vec<TraceSpanRow> {
    for summaries in [
        super::array_value(data, "span_summaries"),
        super::array_value(results, "span_summaries"),
    ] {
        if !summaries.is_empty() {
            return trace_span_rows(summaries);
        }
    }

    trace_span_rows(super::array_value(results, "span_results"))
}

fn trace_span_rows(spans: Vec<&Value>) -> Vec<TraceSpanRow> {
    spans
        .iter()
        .filter_map(|value| trace_span_row(value.as_object()?))
        .collect()
}

fn trace_span_row(span: &Map<String, Value>) -> Option<TraceSpanRow> {
    let id = string_value(span, "id")?;
    let from = string_value(span, "from")?;
    let to = string_value(span, "to")?;
    let duration_ms = span.get("duration_ms").and_then(Value::as_u64);
    let mut status_parts = vec![string_value(span, "status").unwrap_or_else(|| "unknown".into())];
    let missing = span
        .get("missing")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !missing.is_empty() {
        status_parts.push(format!("missing `{}`", missing.join("`, `")));
    }
    if let Some(message) = string_value(span, "message") {
        status_parts.push(message);
    }
    Some(TraceSpanRow {
        id,
        from,
        to,
        duration_ms,
        status: status_parts.join(": "),
        metadata: span
            .get("metadata")
            .and_then(Value::as_object)
            .map(trace_span_metadata_label)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "-".to_string()),
    })
}

fn trace_span_metadata_label(metadata: &Map<String, Value>) -> String {
    let mut parts = Vec::new();
    if let Some(category) = string_value(metadata, "category") {
        parts.push(format!("category={category}"));
    }
    if let Some(blocks) = string_value(metadata, "blocks") {
        parts.push(format!("blocks={blocks}"));
    }
    for key in [
        "critical",
        "blocking",
        "cacheable",
        "prewarmable",
        "deferrable",
    ] {
        if metadata.get(key).and_then(Value::as_bool) == Some(true) {
            parts.push(key.to_string());
        }
    }
    parts.join(", ")
}

fn render_trace_artifacts(
    out: &mut String,
    data: &Map<String, Value>,
    results: &Map<String, Value>,
) {
    let artifacts = collect_trace_artifacts(data, results);
    if !artifacts.is_empty() {
        out.push_str("**Artifacts**\n");
        for (label, path) in artifacts {
            let _ = writeln!(out, "- {}: {}", label, path);
        }
    } else {
        out.push_str("**Artifacts**\n- No structured trace artifacts available.\n");
    }
}

fn collect_trace_artifacts(
    data: &Map<String, Value>,
    results: &Map<String, Value>,
) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    [
        super::array_value(data, "artifacts"),
        super::array_value(results, "artifacts"),
    ]
    .into_iter()
    .flatten()
    .filter_map(|artifact| {
        let obj = artifact.as_object()?;
        let label = string_value(obj, "label").or_else(|| string_value(obj, "name"))?;
        let path = string_value(obj, "path")?;
        if !seen.insert((label.clone(), path.clone())) {
            return None;
        }
        Some((label, path))
    })
    .collect()
}
