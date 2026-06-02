use std::fs;
use std::path::{Path, PathBuf};

use homeboy::commands::report::{performance_digest_from_args, PerformanceDigestArgs};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-performance-digest-{name}-{nanos}"))
}

fn write_fixture_file(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, body).unwrap_or_else(|err| {
        panic!(
            "failed to write performance digest fixture {}: {}",
            path.display(),
            err
        )
    });
}

fn args(dir: &Path) -> PerformanceDigestArgs {
    PerformanceDigestArgs {
        output_dir: dir.to_string_lossy().to_string(),
        metadata_json: None,
        run_url: Some("https://github.com/Extra-Chill/homeboy/actions/runs/456".to_string()),
        min_samples: 3,
        max_cv_pct: 20.0,
        format: "markdown".to_string(),
    }
}

#[test]
fn renders_resource_summary_budget_findings_and_baseline_health() {
    let dir = tmp_dir("full");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_fixture_file(
        &dir,
        "resource-summary.json",
        r#"{
            "label": "bench fixture",
            "duration_ms": 12345,
            "platform": "darwin",
            "load_average_before": { "one": 1.2, "five": 1.1, "fifteen": 1.0 },
            "load_average_after": { "one": 1.8, "five": 1.3, "fifteen": 1.1 },
            "homeboy_rss_bytes_before": 1048576,
            "homeboy_rss_bytes_after": 2097152,
            "extension_children": [{
                "root_pid": 123,
                "command_label": "fixture workload",
                "duration_ms": 1000,
                "sampled_peak_rss_bytes": 3145728,
                "sampled_peak_cpu_percent": 87.5,
                "warnings": []
            }],
            "warnings": ["load_average_unsupported"]
        }"#,
    );
    write_fixture_file(
        &dir,
        "bench.json",
        r#"{
            "success": true,
            "data": {
                "budget_findings": [{
                    "tool": "budget",
                    "rule": "metric.max_value",
                    "category": "budget",
                    "severity": "error",
                    "message": "Metric exceeded budget",
                    "fingerprint": "metric.max_value:fixture-subject",
                    "source": { "kind": "budget", "label": "profile:generic" },
                    "metadata": {
                        "code": "metric.max_value",
                        "context_label": "profile:generic",
                        "actual": 42,
                        "expected": 20,
                        "unit": "count",
                        "subject": "fixture-subject",
                        "passed": false
                    },
                    "raw": {
                        "category": "budget",
                        "code": "metric.max_value",
                        "severity": "error",
                        "context_label": "profile:generic",
                        "message": "Metric exceeded budget",
                        "actual": 42,
                        "expected": 20,
                        "unit": "count",
                        "subject": "fixture-subject",
                        "passed": false
                    }
                }],
                "results": {
                    "scenarios": [{
                        "id": "fixture-scenario",
                        "memory": { "peak_bytes": 8388608 },
                        "runs_summary": {
                            "elapsed_ms": { "n": 2, "mean": 100, "stdev": 25, "cv_pct": 25, "p50": 100, "p95": 130 }
                        }
                    }]
                }
            }
        }"#,
    );
    write_fixture_file(
        &dir,
        "metadata.json",
        r#"{
            "warmup_iterations": 0,
            "resource_policy": {
                "command": "bench",
                "severity": "hot",
                "force_hot": true,
                "warned": true,
                "message": "machine is hot",
                "host": {
                    "load_severity": "hot",
                    "load_one": 8.0,
                    "cpu_count": 4,
                    "memory_severity": "warm"
                }
            },
            "lab_offload": {
                "runner_id": "lab-a",
                "mode": "remote",
                "status": "completed",
                "fallback_reason": ""
            },
            "preview": {
                "schema": "homeboy/preview/v1",
                "provider": "dummy",
                "local_url": "http://127.0.0.1:8080",
                "public_url": "https://preview.example.test/run-1",
                "hold_seconds": 600,
                "expires_at": "2026-06-01T22:00:00Z",
                "status": "running",
                "process_id": "pid-123",
                "runtime_id": "runtime-abc",
                "cleanup_status": "pending"
            }
        }"#,
    );

    let report = performance_digest_from_args(&args(&dir)).expect("digest should render");

    assert!(report.resource_summary.is_some());
    assert_eq!(report.budget_findings.len(), 1);
    assert_eq!(report.benchmark_memory.len(), 1);
    assert_eq!(report.benchmark_memory[0].scenario, "fixture-scenario");
    assert_eq!(report.benchmark_memory[0].peak_bytes, 8388608);
    assert!(report
        .baseline_health
        .iter()
        .any(|diagnostic| diagnostic.code == "baseline.high_variance"));
    assert!(report
        .baseline_health
        .iter()
        .any(|diagnostic| diagnostic.code == "baseline.too_few_samples"));
    assert!(report
        .baseline_health
        .iter()
        .any(|diagnostic| diagnostic.code == "baseline.missing_warmup"));
    assert!(report
        .baseline_health
        .iter()
        .any(|diagnostic| diagnostic.code == "baseline.noisy_host"));
    assert_eq!(
        report
            .host_pressure
            .as_ref()
            .map(|host| host.severity.as_str()),
        Some("hot")
    );
    assert_eq!(
        report.lab_offload.get("runner_id"),
        Some(&"lab-a".to_string())
    );
    assert_eq!(
        report.preview.get("public_url"),
        Some(&"https://preview.example.test/run-1".to_string())
    );
    assert!(report.markdown.contains("## Performance Digest"));
    assert!(report.markdown.contains("### Resource Summary"));
    assert!(report.markdown.contains("- Duration: **12345 ms**"));
    assert!(report.markdown.contains("fixture workload"));
    assert!(report.markdown.contains("### Benchmark Memory"));
    assert!(report.markdown.contains("| `fixture-scenario` | 8.0 MiB |"));
    assert!(report.markdown.contains("### Bench Budget Findings"));
    assert!(report.markdown.contains("| `metric.max_value` | fixture-subject | 42 | 20 | count | error | Metric exceeded budget |"));
    assert!(report.markdown.contains("### Baseline Health"));
    assert!(report.markdown.contains("`baseline.high_variance`"));
    assert!(report.markdown.contains("### Host Pressure"));
    assert!(report.markdown.contains("- Severity: **hot**"));
    assert!(report.markdown.contains("### Lab Offload"));
    assert!(report.markdown.contains("### Preview"));
    assert!(report
        .markdown
        .contains("- local_url: `http://127.0.0.1:8080`"));
    assert!(report
        .markdown
        .contains("- public_url: `https://preview.example.test/run-1`"));
    assert!(report.markdown.contains("- hold_seconds: `600`"));
    assert!(report
        .markdown
        .contains("- expires_at: `2026-06-01T22:00:00Z`"));
    assert!(report.markdown.contains("- process_id: `pid-123`"));
    assert!(report.markdown.contains("- runtime_id: `runtime-abc`"));
    assert!(report.markdown.contains("- cleanup_status: `pending`"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reads_persisted_uuid_prefixed_artifacts() {
    let dir = tmp_dir("persisted");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_fixture_file(
        &dir,
        "123-resource-summary.json",
        r#"{
            "label": "persisted bench",
            "duration_ms": 54321,
            "platform": "macos",
            "extension_children": [],
            "warnings": []
        }"#,
    );
    write_fixture_file(
        &dir,
        "456-bench-results.json",
        r#"{
            "component_id": "homeboy",
            "iterations": 3,
            "metadata": {
                "warmup_iterations": 0
            },
            "budget_findings": [{
                "code": "metric.max_value",
                "severity": "error",
                "message": "Metric exceeded budget",
                "actual": 42,
                "expected": 20,
                "unit": "count",
                "subject": "persisted-subject",
                "passed": false
            }],
            "scenarios": [{
                "id": "audit-self",
                "metrics": { "peak_rss_bytes": 41943040 },
                "runs_summary": {
                    "p95_ms": { "n": 3, "mean": 100, "stdev": 30, "cv_pct": 30, "p50": 100, "p95": 140 }
                }
            }]
        }"#,
    );

    let report = performance_digest_from_args(&args(&dir)).expect("digest should render");

    assert!(report.gaps.is_empty());
    assert_eq!(
        report
            .resource_summary
            .as_ref()
            .and_then(|summary| summary.label.as_deref()),
        Some("persisted bench")
    );
    assert_eq!(report.budget_findings.len(), 1);
    assert_eq!(report.benchmark_memory.len(), 1);
    assert_eq!(report.benchmark_memory[0].scenario, "audit-self");
    assert_eq!(report.benchmark_memory[0].peak_bytes, 41943040);
    assert!(report
        .baseline_health
        .iter()
        .any(|diagnostic| diagnostic.code == "baseline.high_variance"));
    assert!(report
        .baseline_health
        .iter()
        .any(|diagnostic| diagnostic.code == "baseline.missing_warmup"));
    assert!(report.markdown.contains("- Duration: **54321 ms**"));
    assert!(report.markdown.contains("persisted-subject"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn missing_optional_artifacts_degrade_gracefully() {
    let dir = tmp_dir("missing");
    fs::create_dir_all(&dir).expect("temp dir should exist");

    let report = performance_digest_from_args(&args(&dir)).expect("digest should render");

    assert!(report.resource_summary.is_none());
    assert!(report.budget_findings.is_empty());
    assert!(report.baseline_health.is_empty());
    assert!(report
        .gaps
        .contains(&"resource-summary.json not found or not parseable".to_string()));
    assert!(report
        .gaps
        .contains(&"bench.json not found or not parseable".to_string()));
    assert!(report
        .markdown
        .contains("- No structured resource summary available."));
    assert!(report
        .markdown
        .contains("- No structured bench budget findings available."));
    assert!(report
        .markdown
        .contains("- No baseline health diagnostics reported."));
    assert!(report
        .markdown
        .contains("- No resource policy metadata available."));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn renders_max_peak_rss_from_multi_run_memory() {
    let dir = tmp_dir("multi-run-memory");
    fs::create_dir_all(&dir).expect("temp dir should exist");
    write_fixture_file(
        &dir,
        "bench-results.json",
        r#"{
            "component_id": "homeboy",
            "scenarios": [{
                "id": "audit-self",
                "runs": [
                    { "memory": { "peak_bytes": 1048576 }, "metrics": { "p95_ms": 10 } },
                    { "memory": { "peak_bytes": 33554432 }, "metrics": { "p95_ms": 12 } },
                    { "memory": { "peak_bytes": 16777216 }, "metrics": { "p95_ms": 11 } }
                ]
            }]
        }"#,
    );

    let report = performance_digest_from_args(&args(&dir)).expect("digest should render");

    assert_eq!(report.benchmark_memory.len(), 1);
    assert_eq!(report.benchmark_memory[0].scenario, "audit-self");
    assert_eq!(report.benchmark_memory[0].peak_bytes, 33554432);
    assert!(report.markdown.contains("| `audit-self` | 32.0 MiB |"));

    let _ = fs::remove_dir_all(&dir);
}
