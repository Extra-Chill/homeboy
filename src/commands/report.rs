use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::{escape_markdown_table_cell, CmdResult};

#[derive(Args, Debug, Clone)]
pub struct ReportArgs {
    #[command(subcommand)]
    pub command: ReportCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ReportCommand {
    /// Render a markdown failure digest from Homeboy command output JSON files
    FailureDigest(FailureDigestArgs),
}

#[derive(Args, Debug, Clone)]
pub struct FailureDigestArgs {
    /// Directory containing audit.json, lint.json, test.json, etc.
    #[arg(long, value_name = "DIR")]
    pub output_dir: String,

    /// Results JSON, e.g. '{"audit":"fail","lint":"pass"}' (supports @file)
    #[arg(long, value_name = "JSON")]
    pub results: String,

    /// Workflow run URL used as the fallback full-log link
    #[arg(long, value_name = "URL")]
    pub run_url: Option<String>,

    /// Optional tooling metadata JSON file (supports @file)
    #[arg(long, value_name = "JSON_OR_FILE")]
    pub tooling_json: Option<String>,

    /// Commands in this run, used to derive default autofix candidates
    #[arg(long, value_name = "CSV")]
    pub commands: Option<String>,

    /// Commands with autofix support. Defaults to failed audit/lint/test commands.
    #[arg(long, value_name = "CSV")]
    pub autofix_commands: Option<String>,

    /// Whether automated fixes are enabled for this run
    #[arg(long)]
    pub autofix_enabled: bool,

    /// Whether automated fixes were already attempted in this run
    #[arg(long)]
    pub autofix_attempted: bool,

    /// Output format. Markdown is the only supported report format for now.
    #[arg(long, value_parser = ["markdown"], default_value = "markdown")]
    pub format: String,
}

#[derive(Serialize)]
pub struct ReportOutput {
    pub command: String,
    pub markdown: String,
}

pub fn is_markdown_mode(args: &ReportArgs) -> bool {
    matches!(
        &args.command,
        ReportCommand::FailureDigest(failure_args) if failure_args.format == "markdown"
    )
}

pub fn run_markdown(args: ReportArgs) -> CmdResult<String> {
    match args.command {
        ReportCommand::FailureDigest(failure_args) => {
            let markdown = render_failure_digest_from_args(&failure_args)?;
            Ok((markdown, 0))
        }
    }
}

pub fn run(args: ReportArgs, _global: &super::GlobalArgs) -> CmdResult<ReportOutput> {
    match args.command {
        ReportCommand::FailureDigest(failure_args) => {
            let markdown = render_failure_digest_from_args(&failure_args)?;
            Ok((
                ReportOutput {
                    command: "report.failure-digest".to_string(),
                    markdown,
                },
                0,
            ))
        }
    }
}

pub fn render_failure_digest_from_args(args: &FailureDigestArgs) -> homeboy::core::Result<String> {
    let results = read_json_spec_value(&args.results, "results")?;
    let tooling = match args.tooling_json.as_deref() {
        Some(spec) => read_json_spec_value(spec, "tooling_json")?,
        None => Value::Object(Map::new()),
    };

    let context = FailureDigestContext {
        output_dir: PathBuf::from(&args.output_dir),
        results: normalize_object(results),
        run_url: args.run_url.clone().unwrap_or_default(),
        tooling: normalize_object(tooling),
        commands_csv: args.commands.clone().unwrap_or_default(),
        autofix_enabled: args.autofix_enabled,
        autofix_attempted: args.autofix_attempted,
        autofix_commands_csv: args.autofix_commands.clone().unwrap_or_default(),
    };

    Ok(render_failure_digest(&context))
}

pub struct FailureDigestContext {
    pub output_dir: PathBuf,
    pub results: Map<String, Value>,
    pub run_url: String,
    pub tooling: Map<String, Value>,
    pub commands_csv: String,
    pub autofix_enabled: bool,
    pub autofix_attempted: bool,
    pub autofix_commands_csv: String,
}

pub fn render_failure_digest(context: &FailureDigestContext) -> String {
    let mut out = String::new();
    out.push_str("## Failure Digest\n\n");

    if command_failed(&context.results, "lint") {
        render_lint_section(&mut out, &context.output_dir, &context.run_url);
    }
    if command_failed(&context.results, "test") {
        render_test_section(&mut out, &context.output_dir, &context.run_url);
    }
    if command_failed(&context.results, "audit") {
        render_audit_section(&mut out, &context.output_dir, &context.run_url);
    }
    if command_reported(&context.results, "trace") {
        render_trace_section(&mut out, &context.output_dir, &context.run_url);
    }
    if command_reported(&context.results, "bench") {
        render_bench_section(&mut out, &context.output_dir, &context.run_url);
    }

    render_autofix_section(&mut out, context);
    render_tooling_section(&mut out, &context.tooling);

    out.push_str("### Machine-readable artifacts\n");
    out.push_str("- `{command}.json` — structured output per command (from `homeboy --output`)\n");

    out
}

fn read_json_spec_value(spec: &str, context: &str) -> homeboy::core::Result<Value> {
    let raw = if Path::new(spec).exists() {
        std::fs::read_to_string(spec).map_err(|e| {
            homeboy::core::Error::internal_unexpected(format!("Failed to read {}: {}", spec, e))
        })?
    } else {
        homeboy::core::config::read_json_spec_to_string(spec)?
    };
    serde_json::from_str(&raw).map_err(|e| {
        homeboy::core::Error::validation_invalid_json(e, Some(context.to_string()), Some(raw))
    })
}

fn normalize_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn command_failed(results: &Map<String, Value>, command: &str) -> bool {
    results
        .get(command)
        .and_then(Value::as_str)
        .is_some_and(is_failure_status)
}

fn is_failure_status(status: &str) -> bool {
    matches!(status, "fail" | "error")
}

fn command_reported(results: &Map<String, Value>, command: &str) -> bool {
    results.contains_key(command)
}

fn command_names_from_csv(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .filter_map(|part| part.trim().split(' ').next())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

fn failed_commands(results: &Map<String, Value>) -> Vec<String> {
    let mut commands = results
        .iter()
        .filter_map(|(name, status)| {
            status
                .as_str()
                .filter(|value| is_failure_status(value))
                .map(|_| name.clone())
        })
        .collect::<Vec<_>>();
    commands.sort();
    commands
}

fn read_command_json(output_dir: &Path, command: &str) -> Option<Value> {
    let path = output_dir.join(format!("{command}.json"));
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn envelope_parts(value: Option<Value>) -> (Map<String, Value>, Map<String, Value>) {
    let Some(Value::Object(mut root)) = value else {
        return (Map::new(), Map::new());
    };

    if root.contains_key("success") || root.contains_key("data") || root.contains_key("error") {
        let take_object = |root: &mut Map<String, Value>, key: &str| {
            root.remove(key)
                .and_then(|v| match v {
                    Value::Object(map) => Some(map),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let data = take_object(&mut root, "data");
        let error = take_object(&mut root, "error");
        return (data, error);
    }

    (root, Map::new())
}

fn render_lint_section(out: &mut String, output_dir: &Path, run_url: &str) {
    out.push_str("### Lint Failure Digest\n");
    let (data, error) = envelope_parts(read_command_json(output_dir, "lint"));

    if let Some(summary) = string_value(&data, "summary") {
        let _ = writeln!(out, "- Lint summary: **{}**", summary);
    }
    if let Some(summary) = string_value(&data, "phpcs_summary") {
        let _ = writeln!(out, "- PHPCS: {}", summary);
    }
    if let Some(summary) = string_value(&data, "phpstan_summary") {
        let _ = writeln!(out, "- PHPStan: {}", summary);
    }
    if let Some(build_failed) = string_value(&data, "build_failed") {
        let _ = writeln!(out, "- Build failed: {}", build_failed);
    }
    render_error_details(out, &error);

    let top_violations = string_array(&data, "top_violations");
    append_details_block(out, "Top lint violations", &top_violations, 10);

    if !has_any_lint_detail(&data, &error) && top_violations.is_empty() {
        out.push_str("- No structured lint details available.\n");
    }
    render_full_log(out, "lint", run_url);
    out.push('\n');
}

fn render_test_section(out: &mut String, output_dir: &Path, run_url: &str) {
    out.push_str("### Test Failure Digest\n");
    let (data, error) = envelope_parts(read_command_json(output_dir, "test"));
    render_error_details(out, &error);

    let failed_tests = array_value(&data, "failed_tests");
    let failed_count = test_failed_count(&data, failed_tests.len());
    let _ = writeln!(out, "- Failed tests: **{}**", failed_count);

    let details = failed_tests
        .iter()
        .take(10)
        .enumerate()
        .map(|(idx, item)| summarize_test_failure(item, idx + 1))
        .collect::<Vec<_>>();

    if details.is_empty() {
        out.push_str("- No structured test failure details available.\n");
    } else {
        append_details_block(
            out,
            &format!("Failed test details ({} shown)", details.len()),
            &details,
            10,
        );
    }

    render_full_log(out, "test", run_url);
    out.push('\n');
}

fn render_audit_section(out: &mut String, output_dir: &Path, run_url: &str) {
    out.push_str("### Audit Failure Digest\n");
    let (data, error) = envelope_parts(read_command_json(output_dir, "audit"));
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

    let drift_increased = baseline
        .get("drift_increased")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let _ = writeln!(
        out,
        "- Drift increased: **{}**",
        if drift_increased { "yes" } else { "no" }
    );

    let new_items = array_from_object(&baseline, "new_items");
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

    let top_findings = collect_audit_findings(&data, &outlier_items);
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

    render_full_log(out, "audit", run_url);
    out.push('\n');
}

fn render_trace_section(out: &mut String, output_dir: &Path, run_url: &str) {
    let (data, error) = envelope_parts(read_command_json(output_dir, "trace"));
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

    let mut summary_lines = Vec::new();
    if let Some(summary) =
        string_value(&data, "summary").or_else(|| string_value(&results, "summary"))
    {
        summary_lines.push(summary);
    }
    if let Some(failure_message) = string_value(&results, "failure") {
        summary_lines.push(failure_message);
    }
    if let Some(stderr_excerpt) = string_value(&failure, "stderr_excerpt") {
        summary_lines.push(stderr_excerpt);
    }
    if let Some(message) = string_value(&error, "message") {
        summary_lines.push(message);
    }

    if !summary_lines.is_empty() {
        out.push_str("**Summary**\n");
        for line in summary_lines {
            let _ = writeln!(out, "- {}", line);
        }
        out.push('\n');
    }

    let artifacts = collect_trace_artifacts(&data, &results);
    if !artifacts.is_empty() {
        out.push_str("**Artifacts**\n");
        for (label, path) in artifacts {
            let _ = writeln!(out, "- {}: {}", label, path);
        }
    } else {
        out.push_str("**Artifacts**\n- No structured trace artifacts available.\n");
    }

    render_full_log(out, "trace", run_url);
    out.push('\n');
}

fn render_bench_section(out: &mut String, output_dir: &Path, run_url: &str) {
    let (data, error) = envelope_parts(read_command_json(output_dir, "bench"));

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
        let code = string_value(finding, "code").unwrap_or_else(|| "budget".to_string());
        let subject = string_value(finding, "subject")
            .or_else(|| string_value(finding, "context_label"))
            .unwrap_or_else(|| "-".to_string());
        let actual = number_value(finding, "actual")
            .map(format_report_number)
            .unwrap_or_else(|| "-".to_string());
        let expected = number_value(finding, "expected")
            .map(format_report_number)
            .unwrap_or_else(|| "-".to_string());
        let unit = string_value(finding, "unit").unwrap_or_else(|| "-".to_string());
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

fn number_value(map: &Map<String, Value>, key: &str) -> Option<f64> {
    map.get(key).and_then(Value::as_f64)
}

fn render_error_details(out: &mut String, error: &Map<String, Value>) {
    if let Some(code) = string_value(error, "code") {
        let _ = writeln!(out, "- Error code: `{}`", code);
    }
    if let Some(message) = string_value(error, "message") {
        let _ = writeln!(out, "- Error message: {}", message);
    }
    if let Some(details) = object_value(error, "details")
        .get("field")
        .and_then(Value::as_str)
    {
        let _ = writeln!(out, "- Error field: `{}`", details);
    }
    if let Some(hints) = error.get("hints").and_then(Value::as_array) {
        if let Some(first) = hints.first().and_then(Value::as_str) {
            let _ = writeln!(out, "- Hint: {}", first);
        }
    }
}

fn render_autofix_section(out: &mut String, context: &FailureDigestContext) {
    let failed = failed_commands(&context.results);
    let potential = if context.autofix_commands_csv.trim().is_empty() {
        command_names_from_csv(&context.commands_csv)
            .into_iter()
            .filter(|cmd| matches!(cmd.as_str(), "audit" | "lint" | "test"))
            .collect::<BTreeSet<_>>()
    } else {
        command_names_from_csv(&context.autofix_commands_csv)
    };
    let fixable = if context.autofix_enabled {
        potential.clone()
    } else {
        BTreeSet::new()
    };

    let mut auto_fixable_failed = Vec::new();
    let mut potential_auto_fixable_failed = Vec::new();
    let mut human_needed_failed = Vec::new();

    for cmd in &failed {
        let normalized = cmd.to_lowercase();
        if potential.contains(&normalized) {
            potential_auto_fixable_failed.push(cmd.clone());
        }
        if fixable.contains(&normalized) && !context.autofix_attempted {
            auto_fixable_failed.push(cmd.clone());
        } else {
            human_needed_failed.push(cmd.clone());
        }
    }

    let overall = if failed.is_empty() {
        "none"
    } else if !auto_fixable_failed.is_empty() && human_needed_failed.is_empty() {
        "auto_fixable"
    } else if !auto_fixable_failed.is_empty() {
        "mixed"
    } else {
        "human_needed"
    };

    out.push_str("### Autofixability classification\n");
    let _ = writeln!(out, "- Overall: **{}**", overall);
    let _ = writeln!(
        out,
        "- Autofix enabled: **{}**",
        if context.autofix_enabled { "yes" } else { "no" }
    );
    let _ = writeln!(
        out,
        "- Autofix attempted this run: **{}**",
        if context.autofix_attempted {
            "yes"
        } else {
            "no"
        }
    );

    if !auto_fixable_failed.is_empty() {
        out.push_str("- Auto-fixable failed commands:\n");
        for cmd in &auto_fixable_failed {
            let _ = writeln!(out, "  - `{}`", cmd);
        }
    }
    if !human_needed_failed.is_empty() {
        out.push_str("- Human-needed failed commands:\n");
        for cmd in &human_needed_failed {
            let _ = writeln!(out, "  - `{}`", cmd);
        }
    }
    if auto_fixable_failed.is_empty() && human_needed_failed.is_empty() {
        out.push_str("- No failed commands to classify.\n");
    }
    if !potential_auto_fixable_failed.is_empty() {
        out.push_str("- Failed commands with available automated fixes:\n");
        for cmd in &potential_auto_fixable_failed {
            let _ = writeln!(out, "  - `{}`", cmd);
        }
    }
    if !context.autofix_enabled {
        if potential.is_empty() {
            out.push_str(
                "- Automated fixes are **disabled for this step** and no fix-capable commands were detected.\n",
            );
        } else {
            let candidates = potential
                .iter()
                .map(|cmd| format!("`{cmd}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                out,
                "- Automated fixes are **disabled for this step**. Commands with available fix support in this run: {}",
                candidates
            );
        }
    }
    out.push('\n');
}

fn render_tooling_section(out: &mut String, tooling: &Map<String, Value>) {
    if tooling.is_empty() {
        return;
    }

    out.push_str("### Tooling metadata\n");
    for (key, value) in BTreeMap::from_iter(tooling.iter()) {
        let rendered = value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string);
        let _ = writeln!(out, "- {}: `{}`", key, rendered);
    }
    out.push('\n');
}

fn render_full_log(out: &mut String, command: &str, run_url: &str) {
    if run_url.is_empty() {
        let _ = writeln!(
            out,
            "- Full {} log: structured job link unavailable",
            command
        );
    } else {
        let _ = writeln!(out, "- Full {} log: {}", command, run_url);
    }
}

fn has_any_lint_detail(data: &Map<String, Value>, error: &Map<String, Value>) -> bool {
    [
        "summary",
        "phpcs_summary",
        "phpstan_summary",
        "build_failed",
    ]
    .iter()
    .any(|key| string_value(data, key).is_some())
        || ["code", "message"]
            .iter()
            .any(|key| string_value(error, key).is_some())
}

fn string_value(map: &Map<String, Value>, key: &str) -> Option<String> {
    match map.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn object_value(map: &Map<String, Value>, key: &str) -> Map<String, Value> {
    map.get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn array_value<'a>(map: &'a Map<String, Value>, key: &str) -> Vec<&'a Value> {
    map.get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn array_from_object(map: &Map<String, Value>, key: &str) -> Vec<Value> {
    map.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn collect_trace_artifacts(
    data: &Map<String, Value>,
    results: &Map<String, Value>,
) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    [
        array_value(data, "artifacts"),
        array_value(results, "artifacts"),
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

fn string_array(map: &Map<String, Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|value| match value {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(obj) => Some(Value::Object(obj.clone()).to_string()),
                    other if !other.is_null() => Some(other.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn test_failed_count(data: &Map<String, Value>, fallback: usize) -> usize {
    let counts = object_value(data, "test_counts");
    let failed = counts.get("failed").and_then(Value::as_u64).unwrap_or(0);
    let errors = counts.get("errors").and_then(Value::as_u64).unwrap_or(0);
    let total = failed + errors;
    if total > 0 {
        total as usize
    } else {
        fallback
    }
}

fn summarize_test_failure(item: &Value, idx: usize) -> String {
    let Some(obj) = item.as_object() else {
        return format!("{}. {}", idx, item.as_str().unwrap_or("unknown"));
    };

    let name = string_value(obj, "name").unwrap_or_else(|| "unknown".to_string());
    let detail = string_value(obj, "detail").or_else(|| string_value(obj, "message"));
    let location = string_value(obj, "location").or_else(|| string_value(obj, "file"));
    let mut parts = vec![format!("{}. {}", idx, name)];
    if let Some(detail) = detail {
        parts.push(detail);
    }
    if let Some(location) = location {
        parts.push(location);
    }
    parts.join(" — ")
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

fn append_details_block(out: &mut String, summary: &str, lines: &[String], limit: usize) {
    let content = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .take(limit)
        .collect::<Vec<_>>();
    if content.is_empty() {
        return;
    }

    let _ = writeln!(out, "\n<details><summary>{}</summary>\n", summary);
    out.push_str("```text\n");
    for line in content {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```\n\n</details>\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bench_digest_renders_summary_percentiles() {
        let context = FailureDigestContext {
            output_dir: PathBuf::from("/tmp/missing-homeboy-report-tests"),
            results: Map::from_iter([("bench".to_string(), Value::String("pass".to_string()))]),
            run_url: String::new(),
            tooling: Map::new(),
            commands_csv: String::new(),
            autofix_enabled: false,
            autofix_attempted: false,
            autofix_commands_csv: String::new(),
        };
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
        assert!(render_failure_digest(&context).contains("### Bench: unknown"));
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
