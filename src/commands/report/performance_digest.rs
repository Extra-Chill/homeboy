use clap::Args;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::commands::escape_markdown_table_cell;
use homeboy::core::engine::run_dir::files;

#[derive(Args, Debug, Clone)]
pub struct PerformanceDigestArgs {
    /// Directory containing Homeboy run artifacts such as resource-summary.json and bench.json
    #[arg(long, value_name = "DIR")]
    pub output_dir: String,

    /// Optional run metadata JSON, e.g. observation metadata or a status file (supports @file)
    #[arg(long, value_name = "JSON_OR_FILE")]
    pub metadata_json: Option<String>,

    /// Workflow run URL used as the fallback full-log link
    #[arg(long, value_name = "URL")]
    pub run_url: Option<String>,

    /// Minimum run count for baseline health checks
    #[arg(long, default_value_t = 3)]
    pub min_samples: u64,

    /// Maximum coefficient of variation percentage before a baseline is considered noisy
    #[arg(long, default_value_t = 20.0)]
    pub max_cv_pct: f64,

    /// Output format. Markdown is the only direct-render report format for now.
    #[arg(long, value_parser = ["markdown"], default_value = "markdown")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PerformanceDigestReport {
    pub markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_summary: Option<ResourceSummaryDigest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub budget_findings: Vec<BudgetFindingDigest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub benchmark_memory: Vec<BenchmarkMemoryDigest>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub baseline_health: Vec<BaselineHealthDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_pressure: Option<HostPressureDigest>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lab_offload: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResourceSummaryDigest {
    pub label: Option<String>,
    pub duration_ms: Option<f64>,
    pub platform: Option<String>,
    pub load_average_before: Option<LoadDigest>,
    pub load_average_after: Option<LoadDigest>,
    pub homeboy_rss_bytes_before: Option<u64>,
    pub homeboy_rss_bytes_after: Option<u64>,
    pub extension_children: Vec<ChildResourceDigest>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LoadDigest {
    pub one: Option<f64>,
    pub five: Option<f64>,
    pub fifteen: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChildResourceDigest {
    pub root_pid: Option<u64>,
    pub command_label: Option<String>,
    pub duration_ms: Option<f64>,
    pub sampled_peak_rss_bytes: Option<u64>,
    pub sampled_peak_cpu_percent: Option<f64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BudgetFindingDigest {
    pub code: String,
    pub subject: String,
    pub actual: Option<f64>,
    pub expected: Option<f64>,
    pub unit: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BenchmarkMemoryDigest {
    pub scenario: String,
    pub peak_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BaselineHealthDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HostPressureDigest {
    pub severity: String,
    pub command: Option<String>,
    pub warned: Option<bool>,
    pub force_hot: Option<bool>,
    pub message: Option<String>,
    pub host: BTreeMap<String, Value>,
}

use helpers::*;

pub fn render_performance_digest_from_args(
    args: &PerformanceDigestArgs,
) -> homeboy::core::Result<String> {
    performance_digest_from_args(args).map(|report| report.markdown)
}

pub fn performance_digest_from_args(
    args: &PerformanceDigestArgs,
) -> homeboy::core::Result<PerformanceDigestReport> {
    let output_dir = PathBuf::from(&args.output_dir);
    let mut gaps = Vec::new();

    let resource_summary = read_json_artifact(&output_dir, &[files::RESOURCE_SUMMARY])
        .and_then(|value| resource_summary_digest(&value));
    if resource_summary.is_none() {
        gaps.push("resource-summary.json not found or not parseable".to_string());
    }

    let bench_json = read_json_artifact(&output_dir, &["bench.json", files::BENCH_RESULTS]);
    if bench_json.is_none() {
        gaps.push("bench.json not found or not parseable".to_string());
    }
    let bench_data = bench_json.as_ref().map(envelope_data).unwrap_or_default();

    let metadata = read_metadata(args, &output_dir, &bench_data)?;
    let host_pressure =
        find_object_recursive(&metadata, "resource_policy").and_then(host_pressure_digest);
    let lab_offload = find_object_recursive(&metadata, "lab_offload")
        .map(scalar_object)
        .unwrap_or_default();

    let mut baseline_health =
        collect_baseline_health(&bench_data, args.min_samples, args.max_cv_pct);
    baseline_health.extend(collect_metadata_baseline_health(
        &metadata,
        args.min_samples,
        args.max_cv_pct,
    ));
    baseline_health.extend(metadata_baseline_health(&metadata, host_pressure.as_ref()));
    baseline_health.sort_by(|a, b| {
        a.code
            .cmp(&b.code)
            .then(a.scenario.cmp(&b.scenario))
            .then(a.metric.cmp(&b.metric))
    });

    let budget_findings = collect_budget_findings(&bench_data);
    let benchmark_memory = collect_benchmark_memory(&bench_data);
    let markdown = render_markdown(
        resource_summary.as_ref(),
        &budget_findings,
        &benchmark_memory,
        &baseline_health,
        host_pressure.as_ref(),
        &lab_offload,
        &gaps,
        args.run_url.as_deref().unwrap_or_default(),
    );

    Ok(PerformanceDigestReport {
        markdown,
        resource_summary,
        budget_findings,
        benchmark_memory,
        baseline_health,
        host_pressure,
        lab_offload,
        gaps,
    })
}

mod helpers {
    use super::*;

    pub(super) fn read_json_file(path: &Path) -> Option<Value> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub(super) fn read_json_artifact(output_dir: &Path, filenames: &[&str]) -> Option<Value> {
        for filename in filenames {
            if let Some(value) = read_json_file(&output_dir.join(filename)) {
                return Some(value);
            }
        }

        let mut candidates = std::fs::read_dir(output_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        candidates.sort();

        for filename in filenames {
            let suffix = format!("-{filename}");
            for path in &candidates {
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name.ends_with(&suffix) {
                    if let Some(value) = read_json_file(path) {
                        return Some(value);
                    }
                }
            }
        }

        None
    }

    pub(super) fn read_json_spec_value(spec: &str, context: &str) -> homeboy::core::Result<Value> {
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

    pub(super) fn read_metadata(
        args: &PerformanceDigestArgs,
        output_dir: &Path,
        bench_data: &Map<String, Value>,
    ) -> homeboy::core::Result<Value> {
        if let Some(spec) = args.metadata_json.as_deref() {
            return read_json_spec_value(spec, "metadata_json");
        }
        if let Some(value) = read_json_file(&output_dir.join("metadata.json")) {
            return Ok(value);
        }
        if let Some(value) = bench_data.get("metadata") {
            return Ok(value.clone());
        }
        Ok(Value::Object(Map::new()))
    }

    pub(super) fn envelope_data(value: &Value) -> Map<String, Value> {
        let Some(root) = value.as_object() else {
            return Map::new();
        };
        root.get("data")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(|| root.clone())
    }

    pub(super) fn resource_summary_digest(value: &Value) -> Option<ResourceSummaryDigest> {
        let obj = value.as_object()?;
        Some(ResourceSummaryDigest {
            label: string_value(obj, "label"),
            duration_ms: number_value(obj, "duration_ms"),
            platform: string_value(obj, "platform"),
            load_average_before: obj.get("load_average_before").and_then(load_digest),
            load_average_after: obj.get("load_average_after").and_then(load_digest),
            homeboy_rss_bytes_before: u64_value(obj, "homeboy_rss_bytes_before"),
            homeboy_rss_bytes_after: u64_value(obj, "homeboy_rss_bytes_after"),
            extension_children: array_value(obj, "extension_children")
                .into_iter()
                .filter_map(child_resource_digest)
                .collect(),
            warnings: string_array(obj, "warnings"),
        })
    }

    pub(super) fn load_digest(value: &Value) -> Option<LoadDigest> {
        let obj = value.as_object()?;
        Some(LoadDigest {
            one: number_value(obj, "one"),
            five: number_value(obj, "five"),
            fifteen: number_value(obj, "fifteen"),
        })
    }

    pub(super) fn child_resource_digest(value: Value) -> Option<ChildResourceDigest> {
        let obj = value.as_object()?;
        Some(ChildResourceDigest {
            root_pid: u64_value(obj, "root_pid"),
            command_label: string_value(obj, "command_label"),
            duration_ms: number_value(obj, "duration_ms"),
            sampled_peak_rss_bytes: u64_value(obj, "sampled_peak_rss_bytes"),
            sampled_peak_cpu_percent: number_value(obj, "sampled_peak_cpu_percent"),
            warnings: string_array(obj, "warnings"),
        })
    }

    pub(super) fn collect_budget_findings(data: &Map<String, Value>) -> Vec<BudgetFindingDigest> {
        let mut findings = array_value(data, "budget_findings");
        let results = object_value(data, "results");
        if findings.is_empty() {
            findings = array_value(&results, "budget_findings");
        }
        findings
            .into_iter()
            .filter_map(|value| {
                let obj = value.as_object()?;
                Some(BudgetFindingDigest {
                    code: budget_string_value(obj, "code")
                        .or_else(|| string_value(obj, "rule"))
                        .unwrap_or_else(|| "budget".to_string()),
                    subject: budget_string_value(obj, "subject")
                        .or_else(|| budget_string_value(obj, "context_label"))
                        .unwrap_or_else(|| "-".to_string()),
                    actual: budget_number_value(obj, "actual"),
                    expected: budget_number_value(obj, "expected"),
                    unit: budget_string_value(obj, "unit").unwrap_or_else(|| "-".to_string()),
                    severity: string_value(obj, "severity").unwrap_or_else(|| "error".to_string()),
                    message: string_value(obj, "message").unwrap_or_default(),
                })
            })
            .collect()
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

    pub(super) fn collect_benchmark_memory(
        data: &Map<String, Value>,
    ) -> Vec<BenchmarkMemoryDigest> {
        let results = object_value(data, "results");
        let mut scenarios = array_value(&results, "scenarios");
        if scenarios.is_empty() {
            scenarios = array_value(data, "scenarios");
        }

        scenarios
            .into_iter()
            .filter_map(|scenario| {
                let obj = scenario.as_object()?;
                let scenario = string_value(obj, "id").unwrap_or_else(|| "unknown".to_string());
                let peak_bytes = obj
                    .get("memory")
                    .and_then(Value::as_object)
                    .and_then(|memory| u64_value(memory, "peak_bytes"))
                    .or_else(|| max_run_peak_bytes(obj))
                    .or_else(|| {
                        obj.get("metrics")
                            .and_then(Value::as_object)
                            .and_then(|metrics| number_value(metrics, "peak_rss_bytes"))
                            .map(|value| value.max(0.0) as u64)
                    })?;
                Some(BenchmarkMemoryDigest {
                    scenario,
                    peak_bytes,
                })
            })
            .collect()
    }

    fn max_run_peak_bytes(scenario: &Map<String, Value>) -> Option<u64> {
        array_value(scenario, "runs")
            .into_iter()
            .filter_map(|run| {
                run.as_object()?
                    .get("memory")?
                    .as_object()
                    .and_then(|memory| u64_value(memory, "peak_bytes"))
            })
            .max()
    }

    pub(super) fn collect_baseline_health(
        data: &Map<String, Value>,
        min_samples: u64,
        max_cv_pct: f64,
    ) -> Vec<BaselineHealthDiagnostic> {
        let mut diagnostics = Vec::new();
        let results = object_value(data, "results");
        let mut scenarios = array_value(&results, "scenarios");
        if scenarios.is_empty() {
            scenarios = array_value(data, "scenarios");
        }
        for scenario in scenarios {
            let Some(scenario_obj) = scenario.as_object() else {
                continue;
            };
            let scenario_id =
                string_value(scenario_obj, "id").unwrap_or_else(|| "unknown".to_string());
            collect_runs_summary_diagnostics(
                &mut diagnostics,
                &scenario_id,
                object_value(scenario_obj, "runs_summary"),
                min_samples,
                max_cv_pct,
            );
        }
        for metric in array_value(&object_value(data, "metadata"), "scenario_metrics") {
            let Some(metric_obj) = metric.as_object() else {
                continue;
            };
            let scenario_id =
                string_value(metric_obj, "scenario_id").unwrap_or_else(|| "unknown".to_string());
            collect_runs_summary_diagnostics(
                &mut diagnostics,
                &scenario_id,
                object_value(metric_obj, "runs_summary"),
                min_samples,
                max_cv_pct,
            );
        }
        dedupe_diagnostics(diagnostics)
    }

    pub(super) fn collect_metadata_baseline_health(
        metadata: &Value,
        min_samples: u64,
        max_cv_pct: f64,
    ) -> Vec<BaselineHealthDiagnostic> {
        let mut diagnostics = Vec::new();
        let Some(metadata) = metadata.as_object() else {
            return diagnostics;
        };
        for metric in array_value(metadata, "scenario_metrics") {
            let Some(metric_obj) = metric.as_object() else {
                continue;
            };
            let scenario_id =
                string_value(metric_obj, "scenario_id").unwrap_or_else(|| "unknown".to_string());
            collect_runs_summary_diagnostics(
                &mut diagnostics,
                &scenario_id,
                object_value(metric_obj, "runs_summary"),
                min_samples,
                max_cv_pct,
            );
        }
        dedupe_diagnostics(diagnostics)
    }

    pub(super) fn collect_runs_summary_diagnostics(
        diagnostics: &mut Vec<BaselineHealthDiagnostic>,
        scenario_id: &str,
        runs_summary: Map<String, Value>,
        min_samples: u64,
        max_cv_pct: f64,
    ) {
        for (metric, distribution) in runs_summary {
            let Some(distribution) = distribution.as_object() else {
                continue;
            };
            if let Some(n) = u64_value(distribution, "n") {
                if n < min_samples {
                    let mut details = BTreeMap::new();
                    details.insert("n".to_string(), Value::from(n));
                    details.insert("min_samples".to_string(), Value::from(min_samples));
                    diagnostics.push(BaselineHealthDiagnostic {
                        code: "baseline.too_few_samples".to_string(),
                        severity: "warning".to_string(),
                        message: format!(
                            "scenario `{}` metric `{}` has {} sample(s); minimum is {}",
                            scenario_id, metric, n, min_samples
                        ),
                        scenario: Some(scenario_id.to_string()),
                        metric: Some(metric.clone()),
                        details,
                    });
                }
            }
            if let Some(cv_pct) = number_value(distribution, "cv_pct") {
                if cv_pct > max_cv_pct {
                    let mut details = BTreeMap::new();
                    details.insert("cv_pct".to_string(), Value::from(cv_pct));
                    details.insert("max_cv_pct".to_string(), Value::from(max_cv_pct));
                    diagnostics.push(BaselineHealthDiagnostic {
                    code: "baseline.high_variance".to_string(),
                    severity: "warning".to_string(),
                    message: format!(
                        "scenario `{}` metric `{}` has {:.1}% coefficient of variation; maximum is {:.1}%",
                        scenario_id, metric, cv_pct, max_cv_pct
                    ),
                    scenario: Some(scenario_id.to_string()),
                    metric: Some(metric),
                    details,
                });
                }
            }
        }
    }

    pub(super) fn metadata_baseline_health(
        metadata: &Value,
        host_pressure: Option<&HostPressureDigest>,
    ) -> Vec<BaselineHealthDiagnostic> {
        let mut diagnostics = Vec::new();
        if let Some(warmup) = find_number_recursive(metadata, "warmup_iterations") {
            if warmup <= 0.0 {
                let mut details = BTreeMap::new();
                details.insert("warmup_iterations".to_string(), Value::from(warmup));
                diagnostics.push(BaselineHealthDiagnostic {
                    code: "baseline.missing_warmup".to_string(),
                    severity: "info".to_string(),
                    message: "baseline metadata reports no warmup iterations".to_string(),
                    scenario: None,
                    metric: None,
                    details,
                });
            }
        }
        if let Some(host_pressure) = host_pressure {
            if host_pressure.severity != "ok" {
                let mut details = BTreeMap::new();
                details.insert(
                    "resource_policy_severity".to_string(),
                    Value::from(host_pressure.severity.clone()),
                );
                diagnostics.push(BaselineHealthDiagnostic {
                    code: "baseline.noisy_host".to_string(),
                    severity: "warning".to_string(),
                    message: format!(
                    "resource policy severity was `{}`; treat this as a noisy baseline candidate",
                    host_pressure.severity
                ),
                    scenario: None,
                    metric: None,
                    details,
                });
            }
        }
        diagnostics
    }

    pub(super) fn host_pressure_digest(obj: &Map<String, Value>) -> Option<HostPressureDigest> {
        let severity = string_value(obj, "severity")?;
        Some(HostPressureDigest {
            severity,
            command: string_value(obj, "command"),
            warned: bool_value(obj, "warned"),
            force_hot: bool_value(obj, "force_hot"),
            message: string_value(obj, "message"),
            host: object_value(obj, "host").into_iter().collect(),
        })
    }

    pub(super) fn render_markdown(
        resource_summary: Option<&ResourceSummaryDigest>,
        budget_findings: &[BudgetFindingDigest],
        benchmark_memory: &[BenchmarkMemoryDigest],
        baseline_health: &[BaselineHealthDiagnostic],
        host_pressure: Option<&HostPressureDigest>,
        lab_offload: &BTreeMap<String, String>,
        gaps: &[String],
        run_url: &str,
    ) -> String {
        let mut out = String::new();
        out.push_str("## Performance Digest\n\n");
        render_resource_summary(&mut out, resource_summary);
        render_budget_findings(&mut out, budget_findings);
        render_benchmark_memory(&mut out, benchmark_memory);
        render_baseline_health(&mut out, baseline_health);
        render_host_pressure(&mut out, host_pressure);
        render_lab_offload(&mut out, lab_offload);
        render_gaps(&mut out, gaps);
        if !run_url.is_empty() {
            let _ = writeln!(out, "### Full run\n- {}\n", run_url);
        }
        out
    }

    pub(super) fn render_resource_summary(
        out: &mut String,
        summary: Option<&ResourceSummaryDigest>,
    ) {
        out.push_str("### Resource Summary\n");
        let Some(summary) = summary else {
            out.push_str("- No structured resource summary available.\n\n");
            return;
        };
        if let Some(label) = &summary.label {
            let _ = writeln!(out, "- Label: `{}`", label);
        }
        if let Some(duration) = summary.duration_ms {
            let _ = writeln!(out, "- Duration: **{} ms**", format_number(duration));
        }
        if let Some(platform) = &summary.platform {
            let _ = writeln!(out, "- Platform: `{}`", platform);
        }
        if let Some(load) = &summary.load_average_before {
            let _ = writeln!(out, "- Load before: {}", format_load(load));
        }
        if let Some(load) = &summary.load_average_after {
            let _ = writeln!(out, "- Load after: {}", format_load(load));
        }
        if summary.homeboy_rss_bytes_before.is_some() || summary.homeboy_rss_bytes_after.is_some() {
            let _ = writeln!(
                out,
                "- Homeboy RSS: {} -> {}",
                format_bytes(summary.homeboy_rss_bytes_before),
                format_bytes(summary.homeboy_rss_bytes_after)
            );
        }
        if !summary.extension_children.is_empty() {
            out.push_str("\n**Child processes**\n");
            out.push_str("| Command | PID | Duration | Peak RSS | Peak CPU |\n");
            out.push_str("| --- | ---: | ---: | ---: | ---: |\n");
            for child in summary.extension_children.iter().take(10) {
                let command = child.command_label.as_deref().unwrap_or("unknown");
                let pid = child
                    .root_pid
                    .map_or_else(|| "-".to_string(), |pid| pid.to_string());
                let duration = child
                    .duration_ms
                    .map(|duration| format!("{} ms", format_number(duration)))
                    .unwrap_or_else(|| "-".to_string());
                let cpu = child
                    .sampled_peak_cpu_percent
                    .map(|cpu| format!("{:.1}%", cpu))
                    .unwrap_or_else(|| "-".to_string());
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} |",
                    escape_markdown_table_cell(command),
                    pid,
                    duration,
                    format_bytes(child.sampled_peak_rss_bytes),
                    cpu
                );
            }
        }
        if !summary.warnings.is_empty() {
            out.push_str("\n**Resource warnings**\n");
            for warning in &summary.warnings {
                let _ = writeln!(out, "- `{}`", warning);
            }
        }
        out.push('\n');
    }

    pub(super) fn render_budget_findings(out: &mut String, findings: &[BudgetFindingDigest]) {
        out.push_str("### Bench Budget Findings\n");
        if findings.is_empty() {
            out.push_str("- No structured bench budget findings available.\n\n");
            return;
        }
        out.push_str("| Code | Subject | Actual | Expected | Unit | Severity | Message |\n");
        out.push_str("| --- | --- | ---: | ---: | --- | --- | --- |\n");
        for finding in findings.iter().take(10) {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} |",
                escape_markdown_table_cell(&finding.code),
                escape_markdown_table_cell(&finding.subject),
                finding
                    .actual
                    .map(format_number)
                    .unwrap_or_else(|| "-".to_string()),
                finding
                    .expected
                    .map(format_number)
                    .unwrap_or_else(|| "-".to_string()),
                escape_markdown_table_cell(&finding.unit),
                escape_markdown_table_cell(&finding.severity),
                escape_markdown_table_cell(&finding.message)
            );
        }
        out.push('\n');
    }

    pub(super) fn render_benchmark_memory(out: &mut String, memory: &[BenchmarkMemoryDigest]) {
        out.push_str("### Benchmark Memory\n");
        if memory.is_empty() {
            out.push_str("- No scenario-level memory evidence available.\n\n");
            return;
        }

        out.push_str("| Scenario | Peak RSS |\n");
        out.push_str("| --- | ---: |\n");
        for entry in memory.iter().take(10) {
            let _ = writeln!(
                out,
                "| `{}` | {} |",
                escape_markdown_table_cell(&entry.scenario),
                format_bytes(Some(entry.peak_bytes))
            );
        }
        out.push('\n');
    }

    pub(super) fn render_baseline_health(
        out: &mut String,
        diagnostics: &[BaselineHealthDiagnostic],
    ) {
        out.push_str("### Baseline Health\n");
        if diagnostics.is_empty() {
            out.push_str("- No baseline health diagnostics reported.\n\n");
            return;
        }
        for diagnostic in diagnostics.iter().take(20) {
            let _ = writeln!(
                out,
                "- **{}** `{}`: {}",
                diagnostic.severity, diagnostic.code, diagnostic.message
            );
        }
        out.push('\n');
    }

    pub(super) fn render_host_pressure(
        out: &mut String,
        host_pressure: Option<&HostPressureDigest>,
    ) {
        out.push_str("### Host Pressure\n");
        let Some(host_pressure) = host_pressure else {
            out.push_str("- No resource policy metadata available.\n\n");
            return;
        };
        let _ = writeln!(out, "- Severity: **{}**", host_pressure.severity);
        if let Some(command) = &host_pressure.command {
            let _ = writeln!(out, "- Command: `{}`", command);
        }
        if let Some(message) = &host_pressure.message {
            let _ = writeln!(out, "- Message: {}", message);
        }
        for (key, value) in &host_pressure.host {
            let _ = writeln!(out, "- Host {}: `{}`", key, scalar_display(value));
        }
        out.push('\n');
    }

    pub(super) fn render_lab_offload(out: &mut String, lab_offload: &BTreeMap<String, String>) {
        if lab_offload.is_empty() {
            return;
        }
        out.push_str("### Lab Offload\n");
        for (key, value) in lab_offload {
            let _ = writeln!(out, "- {}: `{}`", key, value);
        }
        out.push('\n');
    }

    pub(super) fn render_gaps(out: &mut String, gaps: &[String]) {
        out.push_str("### Artifact Gaps\n");
        if gaps.is_empty() {
            out.push_str("- No expected artifact gaps detected.\n\n");
            return;
        }
        for gap in gaps {
            let _ = writeln!(out, "- {}", gap);
        }
        out.push('\n');
    }

    pub(super) fn find_object_recursive<'a>(
        value: &'a Value,
        key: &str,
    ) -> Option<&'a Map<String, Value>> {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(found)) = map.get(key) {
                    return Some(found);
                }
                map.values()
                    .find_map(|child| find_object_recursive(child, key))
            }
            Value::Array(items) => items
                .iter()
                .find_map(|child| find_object_recursive(child, key)),
            _ => None,
        }
    }

    pub(super) fn find_number_recursive(value: &Value, key: &str) -> Option<f64> {
        match value {
            Value::Object(map) => map.get(key).and_then(Value::as_f64).or_else(|| {
                map.values()
                    .find_map(|child| find_number_recursive(child, key))
            }),
            Value::Array(items) => items
                .iter()
                .find_map(|child| find_number_recursive(child, key)),
            _ => None,
        }
    }

    pub(super) fn scalar_object(map: &Map<String, Value>) -> BTreeMap<String, String> {
        map.iter()
            .filter_map(|(key, value)| match value {
                Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                    Some((key.clone(), scalar_display(value)))
                }
                _ => None,
            })
            .collect()
    }

    pub(super) fn dedupe_diagnostics(
        diagnostics: Vec<BaselineHealthDiagnostic>,
    ) -> Vec<BaselineHealthDiagnostic> {
        let mut seen = BTreeSet::new();
        let mut deduped = Vec::new();
        for diagnostic in diagnostics {
            let key = (
                diagnostic.code.clone(),
                diagnostic.scenario.clone(),
                diagnostic.metric.clone(),
            );
            if seen.insert(key) {
                deduped.push(diagnostic);
            }
        }
        deduped
    }

    pub(super) fn string_value(map: &Map<String, Value>, key: &str) -> Option<String> {
        map.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    pub(super) fn number_value(map: &Map<String, Value>, key: &str) -> Option<f64> {
        map.get(key).and_then(Value::as_f64)
    }

    pub(super) fn u64_value(map: &Map<String, Value>, key: &str) -> Option<u64> {
        map.get(key).and_then(Value::as_u64)
    }

    pub(super) fn bool_value(map: &Map<String, Value>, key: &str) -> Option<bool> {
        map.get(key).and_then(Value::as_bool)
    }

    pub(super) fn object_value(map: &Map<String, Value>, key: &str) -> Map<String, Value> {
        map.get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn array_value(map: &Map<String, Value>, key: &str) -> Vec<Value> {
        map.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn string_array(map: &Map<String, Value>, key: &str) -> Vec<String> {
        map.get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn format_load(load: &LoadDigest) -> String {
        format!(
            "1m={} 5m={} 15m={}",
            load.one
                .map(format_number)
                .unwrap_or_else(|| "-".to_string()),
            load.five
                .map(format_number)
                .unwrap_or_else(|| "-".to_string()),
            load.fifteen
                .map(format_number)
                .unwrap_or_else(|| "-".to_string())
        )
    }

    pub(super) fn format_bytes(bytes: Option<u64>) -> String {
        let Some(bytes) = bytes else {
            return "-".to_string();
        };
        if bytes >= 1024 * 1024 * 1024 {
            format!("{:.1} GiB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
        } else if bytes >= 1024 * 1024 {
            format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0)
        } else if bytes >= 1024 {
            format!("{:.1} KiB", bytes as f64 / 1024.0)
        } else {
            format!("{} B", bytes)
        }
    }

    pub(super) fn format_number(value: f64) -> String {
        if value.fract().abs() < f64::EPSILON {
            format!("{value:.0}")
        } else {
            format!("{value:.2}")
        }
    }

    pub(super) fn scalar_display(value: &Value) -> String {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnoses_high_variance_and_low_sample_runs_summary() {
        let mut data = Map::new();
        data.insert(
            "results".to_string(),
            json!({
                "scenarios": [{
                    "id": "fixture-scenario",
                    "runs_summary": {
                        "elapsed_ms": { "n": 2, "mean": 100.0, "cv_pct": 25.0, "p50": 100.0, "p95": 120.0 }
                    }
                }]
            }),
        );

        let diagnostics = collect_baseline_health(&data, 3, 20.0);

        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "baseline.too_few_samples"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "baseline.high_variance"));
    }
}
