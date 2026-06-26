use clap::Args;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod audit;
mod bench;
pub(super) mod budget_values;
mod trace;

use audit::render_audit_section;
use bench::render_bench_section;
use trace::render_trace_section;

const LINT_FINDING_DIGEST_LIMIT: usize = 10;

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

struct FailureDigestContext {
    output_dir: PathBuf,
    results: Map<String, Value>,
    run_url: String,
    tooling: Map<String, Value>,
    commands_csv: String,
    autofix_enabled: bool,
    autofix_attempted: bool,
    autofix_commands_csv: String,
}

fn render_failure_digest(context: &FailureDigestContext) -> String {
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

    render_failure_origin_section(&mut out, context);
    render_autofix_section(&mut out, context);
    render_tooling_section(&mut out, &context.tooling);

    out.push_str("### Machine-readable artifacts\n");
    out.push_str("- `{command}.json` — structured output per command (from `homeboy --output`)\n");

    out
}

mod detail {
    use super::*;
    pub fn read_json_spec_value(spec: &str, context: &str) -> homeboy::core::Result<Value> {
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

    pub fn normalize_object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    pub fn command_failed(results: &Map<String, Value>, command: &str) -> bool {
        results
            .get(command)
            .and_then(Value::as_str)
            .is_some_and(is_attention_status)
    }

    pub fn is_attention_status(status: &str) -> bool {
        matches!(
            status,
            "fail"
                | "error"
                | "baseline_red"
                | "baseline-red"
                | "inconclusive"
                | "executed_with_findings"
                | "execution_failed"
        )
    }

    pub fn command_reported(results: &Map<String, Value>, command: &str) -> bool {
        results.contains_key(command)
    }

    pub fn command_names_from_csv(raw: &str) -> BTreeSet<String> {
        raw.split(',')
            .filter_map(|part| part.trim().split(' ').next())
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| part.to_lowercase())
            .collect()
    }

    pub fn failed_commands(results: &Map<String, Value>) -> Vec<String> {
        let mut commands = results
            .iter()
            .filter_map(|(name, status)| {
                status
                    .as_str()
                    .filter(|value| is_attention_status(value))
                    .map(|_| name.clone())
            })
            .collect::<Vec<_>>();
        commands.sort();
        commands
    }

    pub fn read_command_json(output_dir: &Path, command: &str) -> Option<Value> {
        let path = output_dir.join(format!("{command}.json"));
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn read_json_file(path: &Path) -> Result<Value, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {}", path.display(), error))?;
        serde_json::from_str(&raw)
            .map_err(|error| format!("failed to parse {}: {}", path.display(), error))
    }

    pub fn envelope_parts(value: Option<Value>) -> (Map<String, Value>, Map<String, Value>) {
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

    pub fn render_lint_section(out: &mut String, output_dir: &Path, run_url: &str) {
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
        render_formatting_findings(out, &data);

        let top_violations = string_array(&data, "top_violations");
        let findings = array_value(&data, "findings");
        render_lint_findings(out, &findings, &data);
        append_details_block(out, "Top lint violations", &top_violations, 10);

        if !has_any_lint_detail(&data, &error) && top_violations.is_empty() && findings.is_empty() {
            out.push_str("- No structured lint details available.\n");
        }
        render_full_log(out, "lint", run_url);
        out.push('\n');
    }

    pub fn render_formatting_findings(out: &mut String, data: &Map<String, Value>) {
        let formatting = object_value(data, "formatting_findings");
        if formatting.is_empty() {
            return;
        }

        let files = string_array(&formatting, "files");
        let command = string_value(&formatting, "suggested_command")
            .unwrap_or_else(|| "cargo fmt".to_string());
        let summary = string_value(&formatting, "summary");

        out.push_str("- Formatting findings:");
        if let Some(summary) = summary {
            let _ = write!(out, " {}", summary);
        }
        out.push('\n');
        if files.is_empty() {
            out.push_str("  - Files needing formatting: unavailable from formatter output\n");
        } else {
            out.push_str("  - Files needing formatting:\n");
            for file in files.iter().take(LINT_FINDING_DIGEST_LIMIT) {
                let _ = writeln!(out, "    - `{}`", file);
            }
            if files.len() > LINT_FINDING_DIGEST_LIMIT {
                let _ = writeln!(
                out,
                "    - {} more file(s) omitted from this comment; see `lint.json` or the full lint log.",
                files.len() - LINT_FINDING_DIGEST_LIMIT
            );
            }
        }
        let _ = writeln!(out, "  - Suggested command: `{}`", command);
    }

    pub fn render_test_section(out: &mut String, output_dir: &Path, run_url: &str) {
        out.push_str("### Test Failure Digest\n");
        let (data, error) = envelope_parts(read_command_json(output_dir, "test"));
        render_error_details(out, &error);

        let findings = array_value(&data, "findings");
        let failed_count = test_failed_count(&data, findings.len());
        let _ = writeln!(out, "- Failed tests: **{}**", failed_count);

        let details = findings
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

    pub fn number_value(map: &Map<String, Value>, key: &str) -> Option<f64> {
        map.get(key).and_then(Value::as_f64)
    }

    pub fn render_error_details(out: &mut String, error: &Map<String, Value>) {
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

    pub(super) fn render_autofix_section(out: &mut String, context: &FailureDigestContext) {
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

    pub(super) fn render_failure_origin_section(out: &mut String, context: &FailureDigestContext) {
        let failed = failed_commands(&context.results);
        if failed.is_empty() {
            return;
        }

        out.push_str("### Failure origin classification\n");
        for command in failed {
            let head_path = context.output_dir.join(format!("{command}.json"));
            let head = match read_json_file(&head_path) {
                Ok(value) => value,
                Err(error) => {
                    let _ = writeln!(
                        out,
                        "- `{}`: **inconclusive** (head artifact unavailable: {})",
                        command, error
                    );
                    continue;
                }
            };
            let Some((baseline_path, baseline_result)) =
                read_baseline_command_json(&context.output_dir, &command)
            else {
                let _ = writeln!(
                    out,
                    "- `{}`: **inconclusive** (baseline artifact unavailable)",
                    command
                );
                continue;
            };
            let baseline = match baseline_result {
                Ok(value) => value,
                Err(error) => {
                    let _ = writeln!(
                        out,
                        "- `{}`: **inconclusive** (baseline artifact `{}` unavailable: {})",
                        command,
                        baseline_path.display(),
                        error
                    );
                    continue;
                }
            };

            let head_failures = failure_fingerprints(&head);
            let baseline_failures = failure_fingerprints(&baseline);
            let classification =
                classify_failure_origin(&head_failures, &baseline_failures, &baseline);
            let _ = writeln!(
                out,
                "- `{}`: **{}** (head: {}, baseline: {}, shared: {})",
                command,
                classification.label(),
                head_failures.len(),
                baseline_failures.len(),
                head_failures.intersection(&baseline_failures).count()
            );
            render_baseline_evidence(out, &command, &baseline_path, &baseline);
        }
        out.push('\n');
    }

    pub fn read_baseline_command_json(
        output_dir: &Path,
        command: &str,
    ) -> Option<(PathBuf, Result<Value, String>)> {
        baseline_command_paths(output_dir, command)
            .into_iter()
            .find(|path| path.exists())
            .map(|path| {
                let value = read_json_file(&path);
                (path, value)
            })
    }

    pub fn baseline_command_paths(output_dir: &Path, command: &str) -> Vec<PathBuf> {
        vec![
            output_dir.join(format!("baseline-{command}.json")),
            output_dir.join(format!("{command}-baseline.json")),
            output_dir.join("baseline").join(format!("{command}.json")),
        ]
    }

    #[derive(Debug, PartialEq, Eq)]
    enum FailureOriginClassification {
        BranchIntroduced,
        BaselinePresent,
        BaselineRed,
        Inconclusive,
    }

    impl FailureOriginClassification {
        fn label(&self) -> &'static str {
            match self {
                Self::BranchIntroduced => "branch-introduced",
                Self::BaselinePresent => "baseline-present",
                Self::BaselineRed => "baseline-red",
                Self::Inconclusive => "inconclusive",
            }
        }
    }

    fn classify_failure_origin(
        head: &BTreeSet<String>,
        baseline: &BTreeSet<String>,
        baseline_artifact: &Value,
    ) -> FailureOriginClassification {
        if baseline.is_empty() && baseline_artifact_reports_failure(baseline_artifact) {
            return FailureOriginClassification::BaselineRed;
        }
        if head.is_empty() || baseline.is_empty() {
            return FailureOriginClassification::Inconclusive;
        }
        if head.is_subset(baseline) {
            FailureOriginClassification::BaselinePresent
        } else if head.is_disjoint(baseline) {
            FailureOriginClassification::BranchIntroduced
        } else {
            FailureOriginClassification::Inconclusive
        }
    }

    pub fn baseline_artifact_reports_failure(value: &Value) -> bool {
        let root = value.as_object().cloned().unwrap_or_default();
        if root.get("success").and_then(Value::as_bool) == Some(false) {
            return true;
        }
        let (data, error) = envelope_parts(Some(value.clone()));
        !error.is_empty()
            || data.get("passed").and_then(Value::as_bool) == Some(false)
            || string_value(&data, "status").is_some_and(|status| is_attention_status(&status))
            || data
                .get("exit_code")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
    }

    pub fn render_baseline_evidence(
        out: &mut String,
        command: &str,
        path: &Path,
        baseline: &Value,
    ) {
        let (data, error) = envelope_parts(Some(baseline.clone()));
        let command_line = baseline_command_line(&data, &error).unwrap_or_else(|| {
            format!(
                "baseline {} command not recorded; inspect `{}`",
                command,
                path.display()
            )
        });
        let _ = writeln!(out, "  - Baseline command: `{}`", command_line);
        let _ = writeln!(out, "  - Baseline artifact: `{}`", path.display());

        let evidence = baseline_evidence_refs(&data, &error);
        if !evidence.is_empty() {
            let _ = writeln!(out, "  - Baseline evidence: {}", evidence.join(", "));
        }
    }

    pub fn baseline_command_line(
        data: &Map<String, Value>,
        error: &Map<String, Value>,
    ) -> Option<String> {
        for map in [data, error] {
            for key in ["baseline_command", "command_line", "command", "cmd"] {
                if let Some(value) = string_value(map, key) {
                    return Some(value);
                }
            }
            let details = object_value(map, "details");
            for key in ["baseline_command", "command_line", "command", "cmd"] {
                if let Some(value) = string_value(&details, key) {
                    return Some(value);
                }
            }
        }
        None
    }

    pub fn baseline_evidence_refs(
        data: &Map<String, Value>,
        error: &Map<String, Value>,
    ) -> Vec<String> {
        let mut refs = BTreeSet::new();
        collect_artifact_refs(data, &mut refs);
        collect_artifact_refs(error, &mut refs);
        collect_path_refs(data, &mut refs);
        collect_path_refs(error, &mut refs);
        refs.into_iter().collect()
    }

    pub fn collect_artifact_refs(map: &Map<String, Value>, refs: &mut BTreeSet<String>) {
        for key in ["artifacts", "artifact_refs"] {
            if let Some(items) = map.get(key).and_then(Value::as_array) {
                for item in items {
                    if let Some(text) = item.as_str() {
                        refs.insert(format!("`{text}`"));
                        continue;
                    }
                    if let Some(obj) = item.as_object() {
                        let label = string_value(obj, "label")
                            .or_else(|| string_value(obj, "name"))
                            .unwrap_or_else(|| "artifact".to_string());
                        if let Some(path) = string_value(obj, "path") {
                            refs.insert(format!("{} `{}`", label, path));
                        }
                    }
                }
            }
        }
    }

    pub fn collect_path_refs(map: &Map<String, Value>, refs: &mut BTreeSet<String>) {
        for key in [
            "log_path",
            "stderr_path",
            "stdout_path",
            "summary_path",
            "manifest_path",
        ] {
            if let Some(path) = string_value(map, key) {
                refs.insert(format!("{} `{}`", key, path));
            }
        }
        let details = object_value(map, "details");
        if !details.is_empty() {
            collect_path_refs(&details, refs);
        }
    }

    pub fn failure_fingerprints(value: &Value) -> BTreeSet<String> {
        let (data, error) = envelope_parts(Some(value.clone()));
        let mut fingerprints = BTreeSet::new();
        collect_failure_fingerprints_from_map(&data, &mut fingerprints);
        collect_failure_fingerprints_from_map(&error, &mut fingerprints);
        fingerprints
    }

    pub fn collect_failure_fingerprints_from_map(
        map: &Map<String, Value>,
        out: &mut BTreeSet<String>,
    ) {
        for key in [
            "findings",
            "failures",
            "test_failures",
            "errors",
            "budget_findings",
        ] {
            if let Some(items) = map.get(key).and_then(Value::as_array) {
                for item in items {
                    if let Some(fingerprint) = failure_fingerprint(item) {
                        out.insert(fingerprint);
                    }
                }
            }
        }
    }

    pub fn failure_fingerprint(item: &Value) -> Option<String> {
        let obj = item.as_object()?;
        for key in ["fingerprint", "id"] {
            if let Some(value) = string_value(obj, key) {
                return Some(format!("{key}:{value}"));
            }
        }

        let metadata = object_value(obj, "metadata");
        let test_name = string_value(&metadata, "test_name").or_else(|| string_value(obj, "name"));
        let tool = string_value(obj, "tool").unwrap_or_default();
        let rule = string_value(obj, "rule").unwrap_or_default();
        let file = string_value(obj, "file").unwrap_or_default();
        let line = string_value(obj, "line").unwrap_or_default();
        let message = string_value(obj, "message").or_else(|| string_value(obj, "detail"));

        let parts = [
            tool,
            rule,
            file,
            line,
            test_name.unwrap_or_default(),
            message.unwrap_or_default(),
        ]
        .into_iter()
        .map(|part| part.trim().to_string())
        .collect::<Vec<_>>();
        if parts.iter().all(String::is_empty) {
            None
        } else {
            Some(parts.join("|"))
        }
    }

    pub fn render_tooling_section(out: &mut String, tooling: &Map<String, Value>) {
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

    pub fn render_full_log(out: &mut String, command: &str, run_url: &str) {
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

    pub fn has_any_lint_detail(data: &Map<String, Value>, error: &Map<String, Value>) -> bool {
        [
            "summary",
            "phpcs_summary",
            "phpstan_summary",
            "build_failed",
        ]
        .iter()
        .any(|key| string_value(data, key).is_some())
            || !object_value(data, "formatting_findings").is_empty()
            || ["code", "message"]
                .iter()
                .any(|key| string_value(error, key).is_some())
    }

    pub fn render_lint_findings(out: &mut String, findings: &[&Value], data: &Map<String, Value>) {
        if findings.is_empty() {
            return;
        }

        let shown = findings.len().min(LINT_FINDING_DIGEST_LIMIT);
        let _ = writeln!(out, "- Actionable lint findings ({} shown):", shown);
        for (idx, item) in findings.iter().take(LINT_FINDING_DIGEST_LIMIT).enumerate() {
            let _ = writeln!(out, "  - {}", summarize_lint_finding(item, idx + 1));
        }
        if findings.len() > shown {
            let _ = writeln!(
            out,
            "- {} more lint finding(s) omitted from this comment; see `lint.json` or the full lint log.",
            findings.len() - shown
        );
        }
        if let Some(autofix) = object_value_opt(data, "autofix") {
            let files_modified = autofix
                .get("files_modified")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let _ = writeln!(
                out,
                "- Autofix applied: **{}** ({} file(s) modified)",
                if files_modified > 0 { "yes" } else { "no" },
                files_modified
            );
        }
    }

    pub fn summarize_lint_finding(item: &Value, idx: usize) -> String {
        let Some(obj) = item.as_object() else {
            return format!(
                "{}. {}",
                idx,
                item.as_str().unwrap_or("unknown lint finding")
            );
        };

        let file = string_value(obj, "file").unwrap_or_else(|| "unknown file".to_string());
        let location = obj
            .get("line")
            .and_then(Value::as_i64)
            .map(|line| format!("{}:{}", file, line))
            .unwrap_or(file);
        let severity =
            string_value(obj, "severity").unwrap_or_else(|| "unknown severity".to_string());
        let tool = string_value(obj, "tool").unwrap_or_else(|| "lint".to_string());
        let rule = string_value(obj, "rule").unwrap_or_else(|| "unknown rule".to_string());
        let message =
            string_value(obj, "message").unwrap_or_else(|| "No message provided".to_string());

        format!(
            "{}. `{}` [{}] {}/{}: {}",
            idx, location, severity, tool, rule, message
        )
    }

    pub fn string_value(map: &Map<String, Value>, key: &str) -> Option<String> {
        match map.get(key)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub fn object_value(map: &Map<String, Value>, key: &str) -> Map<String, Value> {
        map.get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    pub fn object_value_opt<'a>(
        map: &'a Map<String, Value>,
        key: &str,
    ) -> Option<&'a Map<String, Value>> {
        map.get(key).and_then(Value::as_object)
    }

    pub fn array_value<'a>(map: &'a Map<String, Value>, key: &str) -> Vec<&'a Value> {
        map.get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    }

    pub fn array_from_object(map: &Map<String, Value>, key: &str) -> Vec<Value> {
        map.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    pub fn string_array(map: &Map<String, Value>, key: &str) -> Vec<String> {
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

    pub fn test_failed_count(data: &Map<String, Value>, fallback: usize) -> usize {
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

    pub fn summarize_test_failure(item: &Value, idx: usize) -> String {
        let Some(obj) = item.as_object() else {
            return format!("{}. {}", idx, item.as_str().unwrap_or("unknown"));
        };

        let name = object_value(obj, "metadata")
            .get("test_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| string_value(obj, "name"))
            .unwrap_or_else(|| "unknown".to_string());
        let detail = string_value(obj, "detail").or_else(|| string_value(obj, "message"));
        let location = string_value(obj, "location").or_else(|| {
            string_value(obj, "file").map(|file| {
                obj.get("line")
                    .and_then(Value::as_i64)
                    .map(|line| format!("{}:{}", file, line))
                    .unwrap_or(file)
            })
        });
        let mut parts = vec![format!("{}. {}", idx, name)];
        if let Some(detail) = detail {
            parts.push(detail);
        }
        if let Some(location) = location {
            parts.push(location);
        }
        parts.join(" — ")
    }

    pub fn append_details_block(out: &mut String, summary: &str, lines: &[String], limit: usize) {
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
}
pub use detail::*;

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(render_failure_digest(&context).contains("### Bench: unknown"));
    }
}
