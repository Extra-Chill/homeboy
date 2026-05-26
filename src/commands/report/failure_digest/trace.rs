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
